//! SQLite persistence and traversal for the code-structure graph.
//!
//! The graph is derived data: every row can be rebuilt by reindexing the
//! workspace. Persistence still versions its format and migrates on load so
//! an upgraded engine never misreads old rows — but the recorded last-resort
//! migration is dropping the graph tables and reindexing from source.

use crate::{ProjectConfig, StoreConfigError, REVIEW_DB_FILE_NAME};
use localmind_core::{GraphEdge, GraphEdgeId, GraphEndpoint, GraphNode, GraphNodeId, NodeKind};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use time::OffsetDateTime;

/// Version of the on-disk graph format this build reads and writes.
pub const GRAPH_FORMAT_VERSION: i64 = 1;

pub struct GraphStore {
    connection: Connection,
}

impl GraphStore {
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self, GraphStoreError> {
        let config = ProjectConfig::discover(project_root).map_err(GraphStoreError::Config)?;
        let state_dir = config.project_root.join(".localmind");
        fs::create_dir_all(&state_dir).map_err(|source| GraphStoreError::CreateStateDir {
            path: state_dir.clone(),
            source,
        })?;
        let db_path = state_dir.join(REVIEW_DB_FILE_NAME);
        let connection =
            Connection::open(&db_path).map_err(|source| GraphStoreError::OpenDatabase {
                path: db_path,
                source,
            })?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> Result<Self, GraphStoreError> {
        let store = Self { connection };
        store.migrate()?;
        Ok(store)
    }

    /// Creates the graph tables when absent and reconciles the stored format
    /// version with [`GRAPH_FORMAT_VERSION`].
    pub fn migrate(&self) -> Result<(), GraphStoreError> {
        self.connection
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS graph_meta (
                    format_version INTEGER NOT NULL,
                    applied_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS graph_nodes (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    name TEXT NOT NULL,
                    qualified_name TEXT NOT NULL,
                    path TEXT,
                    content_hash TEXT NOT NULL,
                    superseded_at TEXT,
                    node_json TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_graph_nodes_kind
                    ON graph_nodes(kind);
                CREATE INDEX IF NOT EXISTS idx_graph_nodes_name
                    ON graph_nodes(name);
                CREATE INDEX IF NOT EXISTS idx_graph_nodes_path
                    ON graph_nodes(path);

                CREATE TABLE IF NOT EXISTS graph_edges (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    from_scope TEXT NOT NULL,
                    from_id TEXT NOT NULL,
                    to_scope TEXT NOT NULL,
                    to_id TEXT NOT NULL,
                    derivation TEXT NOT NULL,
                    superseded_at TEXT,
                    edge_json TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_graph_edges_from
                    ON graph_edges(from_id, kind);
                CREATE INDEX IF NOT EXISTS idx_graph_edges_to
                    ON graph_edges(to_id, kind);
                "#,
            )
            .map_err(GraphStoreError::Sqlite)?;

        let stored: Option<i64> = self
            .connection
            .query_row("SELECT MAX(format_version) FROM graph_meta", [], |row| {
                row.get(0)
            })
            .map_err(GraphStoreError::Sqlite)?;

        match stored {
            None => {
                self.connection
                    .execute(
                        "INSERT INTO graph_meta(format_version, applied_at) VALUES(?1, ?2)",
                        params![GRAPH_FORMAT_VERSION, now_string()],
                    )
                    .map_err(GraphStoreError::Sqlite)?;
                Ok(())
            }
            Some(version) if version == GRAPH_FORMAT_VERSION => Ok(()),
            Some(version) if version < GRAPH_FORMAT_VERSION => {
                // No incremental migrations exist yet. The graph is derived
                // data, so the recorded last-resort migration applies: drop
                // and reindex from source.
                self.reset_graph()?;
                self.connection
                    .execute(
                        "INSERT INTO graph_meta(format_version, applied_at) VALUES(?1, ?2)",
                        params![GRAPH_FORMAT_VERSION, now_string()],
                    )
                    .map_err(GraphStoreError::Sqlite)?;
                Ok(())
            }
            Some(version) => Err(GraphStoreError::UnsupportedFormatVersion {
                stored: version,
                supported: GRAPH_FORMAT_VERSION,
            }),
        }
    }

    /// Drops all graph rows (not the tables). The next reindex rebuilds them.
    pub fn reset_graph(&self) -> Result<(), GraphStoreError> {
        self.connection
            .execute_batch("DELETE FROM graph_edges; DELETE FROM graph_nodes;")
            .map_err(GraphStoreError::Sqlite)
    }

    pub fn format_version(&self) -> Result<i64, GraphStoreError> {
        self.connection
            .query_row("SELECT MAX(format_version) FROM graph_meta", [], |row| {
                row.get(0)
            })
            .map_err(GraphStoreError::Sqlite)
    }

    pub fn upsert_node(&self, node: &GraphNode) -> Result<(), GraphStoreError> {
        let node_json = serde_json::to_string(node).map_err(GraphStoreError::Serialize)?;
        let path = node.location.as_ref().map(|location| location.path.clone());
        let superseded_at = node.superseded_at.map(|stamp| stamp.to_string());
        self.connection
            .execute(
                r#"
                INSERT INTO graph_nodes
                    (id, kind, name, qualified_name, path, content_hash, superseded_at, node_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    name = excluded.name,
                    qualified_name = excluded.qualified_name,
                    path = excluded.path,
                    content_hash = excluded.content_hash,
                    superseded_at = excluded.superseded_at,
                    node_json = excluded.node_json
                "#,
                params![
                    node.id.as_str(),
                    node.kind.as_str(),
                    node.name,
                    node.qualified_name,
                    path,
                    node.content_hash,
                    superseded_at,
                    node_json,
                ],
            )
            .map_err(GraphStoreError::Sqlite)?;
        Ok(())
    }

    pub fn upsert_edge(&self, edge: &GraphEdge) -> Result<(), GraphStoreError> {
        let edge_json = serde_json::to_string(edge).map_err(GraphStoreError::Serialize)?;
        let (from_scope, from_id) = endpoint_columns(&edge.from);
        let (to_scope, to_id) = endpoint_columns(&edge.to);
        let superseded_at = edge.superseded_at.map(|stamp| stamp.to_string());
        self.connection
            .execute(
                r#"
                INSERT INTO graph_edges
                    (id, kind, from_scope, from_id, to_scope, to_id, derivation,
                     superseded_at, edge_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    from_scope = excluded.from_scope,
                    from_id = excluded.from_id,
                    to_scope = excluded.to_scope,
                    to_id = excluded.to_id,
                    derivation = excluded.derivation,
                    superseded_at = excluded.superseded_at,
                    edge_json = excluded.edge_json
                "#,
                params![
                    edge.id.as_str(),
                    edge.kind.as_str(),
                    from_scope,
                    from_id,
                    to_scope,
                    to_id,
                    edge.derivation.as_str(),
                    superseded_at,
                    edge_json,
                ],
            )
            .map_err(GraphStoreError::Sqlite)?;
        Ok(())
    }

    pub fn node(&self, id: &GraphNodeId) -> Result<Option<GraphNode>, GraphStoreError> {
        self.connection
            .query_row(
                "SELECT node_json FROM graph_nodes WHERE id = ?1",
                params![id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(GraphStoreError::Sqlite)?
            .map(|json| serde_json::from_str(&json).map_err(GraphStoreError::Deserialize))
            .transpose()
    }

    pub fn nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<GraphNode>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT node_json FROM graph_nodes
                 WHERE kind = ?1 AND superseded_at IS NULL
                 ORDER BY qualified_name",
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![kind.as_str()], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        collect_nodes(rows)
    }

    pub fn edge(&self, id: &GraphEdgeId) -> Result<Option<GraphEdge>, GraphStoreError> {
        self.connection
            .query_row(
                "SELECT edge_json FROM graph_edges WHERE id = ?1",
                params![id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(GraphStoreError::Sqlite)?
            .map(|json| serde_json::from_str(&json).map_err(GraphStoreError::Deserialize))
            .transpose()
    }

    /// Active file nodes as `(relative path, content hash)` pairs — the
    /// stored side of incremental change detection.
    pub fn active_file_hashes(&self) -> Result<Vec<(String, String)>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT qualified_name, content_hash FROM graph_nodes
                 WHERE kind = 'file' AND superseded_at IS NULL
                 ORDER BY qualified_name",
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(GraphStoreError::Sqlite)?;
        let mut hashes = Vec::new();
        for row in rows {
            hashes.push(row.map_err(GraphStoreError::Sqlite)?);
        }
        Ok(hashes)
    }

    /// Supersedes every active node located in `path` (including the file
    /// node itself) and every active edge touching those nodes. Rows stay in
    /// place; provenance survives. Returns the superseded edge ids so a
    /// reindex can revive the ones whose endpoints come back unchanged.
    pub fn supersede_path(&self, path: &str) -> Result<Vec<GraphEdgeId>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id FROM graph_nodes
                 WHERE (path = ?1 OR (kind = 'file' AND qualified_name = ?1))
                     AND superseded_at IS NULL",
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![path], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(GraphNodeId::new(row.map_err(GraphStoreError::Sqlite)?));
        }
        drop(statement);

        let mut superseded_edges = Vec::new();
        for id in &ids {
            self.supersede_node(id)?;
            superseded_edges.extend(self.supersede_edges_touching(id)?);
        }
        Ok(superseded_edges)
    }

    /// Supersedes every active edge with `id` as either endpoint; returns the
    /// affected edge ids.
    pub fn supersede_edges_touching(
        &self,
        id: &GraphNodeId,
    ) -> Result<Vec<GraphEdgeId>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, edge_json FROM graph_edges
                 WHERE (from_id = ?1 OR to_id = ?1) AND superseded_at IS NULL",
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![id.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(GraphStoreError::Sqlite)?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row.map_err(GraphStoreError::Sqlite)?);
        }
        drop(statement);

        let stamp_time = OffsetDateTime::now_utc();
        let stamp = stamp_time.to_string();
        let mut affected = Vec::new();
        for (edge_id, edge_json) in edges {
            let mut edge: GraphEdge =
                serde_json::from_str(&edge_json).map_err(GraphStoreError::Deserialize)?;
            edge.superseded_at = Some(stamp_time);
            let updated = serde_json::to_string(&edge).map_err(GraphStoreError::Serialize)?;
            self.connection
                .execute(
                    "UPDATE graph_edges SET superseded_at = ?1, edge_json = ?2 WHERE id = ?3",
                    params![stamp, updated, edge_id],
                )
                .map_err(GraphStoreError::Sqlite)?;
            affected.push(GraphEdgeId::new(edge_id));
        }
        Ok(affected)
    }

    /// Revives a superseded edge when its endpoints are valid again: node
    /// endpoints must be active nodes; memory endpoints are accepted as-is
    /// (memory lifecycle is owned elsewhere). Returns whether it revived.
    /// This is how knowledge anchored to an unchanged symbol survives a
    /// reindex of the file around it.
    pub fn revive_edge_if_anchored(&self, id: &GraphEdgeId) -> Result<bool, GraphStoreError> {
        let Some(edge) = self.edge(id)? else {
            return Ok(false);
        };
        if edge.superseded_at.is_none() {
            return Ok(true);
        }
        for endpoint in [&edge.from, &edge.to] {
            if let GraphEndpoint::Node(node_id) = endpoint {
                let active = self
                    .node(node_id)?
                    .map(|node| node.superseded_at.is_none())
                    .unwrap_or(false);
                if !active {
                    return Ok(false);
                }
            }
        }
        let mut revived = edge;
        revived.superseded_at = None;
        self.upsert_edge(&revived)?;
        Ok(true)
    }

    /// Active (non-superseded) edges, ordered by id — the comparison view for
    /// reindex equivalence.
    pub fn active_edge_ids(&self) -> Result<Vec<String>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT id FROM graph_edges WHERE superseded_at IS NULL ORDER BY id")
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(GraphStoreError::Sqlite)?);
        }
        Ok(ids)
    }

    /// Active nodes as `(id, content hash)` ordered by id — the comparison
    /// view for reindex equivalence.
    pub fn active_node_summaries(&self) -> Result<Vec<(String, String)>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, content_hash FROM graph_nodes
                 WHERE superseded_at IS NULL ORDER BY id",
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(GraphStoreError::Sqlite)?;
        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row.map_err(GraphStoreError::Sqlite)?);
        }
        Ok(summaries)
    }

    /// Marks a node superseded instead of deleting it, so anchored knowledge
    /// and provenance survive reindexes.
    pub fn supersede_node(&self, id: &GraphNodeId) -> Result<(), GraphStoreError> {
        let stamp = now_string();
        let node_json: Option<String> = self
            .connection
            .query_row(
                "SELECT node_json FROM graph_nodes WHERE id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(GraphStoreError::Sqlite)?;
        let Some(node_json) = node_json else {
            return Err(GraphStoreError::MissingNode { id: id.clone() });
        };
        let mut node: GraphNode =
            serde_json::from_str(&node_json).map_err(GraphStoreError::Deserialize)?;
        node.superseded_at = Some(OffsetDateTime::now_utc());
        let updated = serde_json::to_string(&node).map_err(GraphStoreError::Serialize)?;
        self.connection
            .execute(
                "UPDATE graph_nodes SET superseded_at = ?1, node_json = ?2 WHERE id = ?3",
                params![stamp, updated, id.as_str()],
            )
            .map_err(GraphStoreError::Sqlite)?;
        Ok(())
    }

    /// Nodes reachable from `start` within `max_depth` hops, walking edges in
    /// both directions, excluding superseded rows and `start` itself.
    pub fn neighbors(
        &self,
        start: &GraphNodeId,
        max_depth: u32,
    ) -> Result<Vec<GraphNode>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                WITH RECURSIVE reachable(id, depth) AS (
                    VALUES (?1, 0)
                    UNION
                    SELECT step.next_id, reachable.depth + 1
                    FROM reachable
                    JOIN (
                        SELECT from_id AS this_id, to_id AS next_id FROM graph_edges
                        WHERE superseded_at IS NULL
                            AND from_scope = 'node' AND to_scope = 'node'
                        UNION ALL
                        SELECT to_id AS this_id, from_id AS next_id FROM graph_edges
                        WHERE superseded_at IS NULL
                            AND from_scope = 'node' AND to_scope = 'node'
                    ) AS step ON step.this_id = reachable.id
                    WHERE reachable.depth < ?2
                )
                SELECT nodes.node_json
                FROM graph_nodes AS nodes
                JOIN reachable ON reachable.id = nodes.id
                WHERE nodes.id != ?1 AND nodes.superseded_at IS NULL
                ORDER BY nodes.qualified_name
                "#,
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![start.as_str(), max_depth], |row| {
                row.get::<_, String>(0)
            })
            .map_err(GraphStoreError::Sqlite)?;
        collect_nodes(rows)
    }

    /// Nodes pointed at by `from` over edges of `kind` (one hop, outgoing).
    pub fn outgoing(
        &self,
        from: &GraphNodeId,
        kind: localmind_core::EdgeKind,
    ) -> Result<Vec<GraphNode>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT nodes.node_json
                FROM graph_edges AS edges
                JOIN graph_nodes AS nodes ON nodes.id = edges.to_id
                WHERE edges.from_id = ?1 AND edges.kind = ?2
                    AND edges.superseded_at IS NULL
                    AND edges.to_scope = 'node'
                    AND nodes.superseded_at IS NULL
                ORDER BY nodes.qualified_name
                "#,
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![from.as_str(), kind.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(GraphStoreError::Sqlite)?;
        collect_nodes(rows)
    }

    /// Nodes pointing at `to` over edges of `kind` (one hop, incoming).
    pub fn incoming(
        &self,
        to: &GraphNodeId,
        kind: localmind_core::EdgeKind,
    ) -> Result<Vec<GraphNode>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT nodes.node_json
                FROM graph_edges AS edges
                JOIN graph_nodes AS nodes ON nodes.id = edges.from_id
                WHERE edges.to_id = ?1 AND edges.kind = ?2
                    AND edges.superseded_at IS NULL
                    AND edges.from_scope = 'node'
                    AND nodes.superseded_at IS NULL
                ORDER BY nodes.qualified_name
                "#,
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![to.as_str(), kind.as_str()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(GraphStoreError::Sqlite)?;
        collect_nodes(rows)
    }

    /// Active nodes whose qualified name or plain name equals `symbol`.
    pub fn find_symbol(&self, symbol: &str) -> Result<Vec<GraphNode>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT node_json FROM graph_nodes
                 WHERE (qualified_name = ?1 OR name = ?1) AND superseded_at IS NULL
                 ORDER BY qualified_name",
            )
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![symbol], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        collect_nodes(rows)
    }

    /// All active edges, ordered by id — the export view.
    pub fn active_edges(&self) -> Result<Vec<GraphEdge>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT edge_json FROM graph_edges WHERE superseded_at IS NULL ORDER BY id")
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        let mut edges = Vec::new();
        for row in rows {
            let json = row.map_err(GraphStoreError::Sqlite)?;
            edges.push(serde_json::from_str(&json).map_err(GraphStoreError::Deserialize)?);
        }
        Ok(edges)
    }

    /// All active nodes, ordered by id — the export view.
    pub fn active_nodes(&self) -> Result<Vec<GraphNode>, GraphStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT node_json FROM graph_nodes WHERE superseded_at IS NULL ORDER BY id")
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        collect_nodes(rows)
    }

    /// Active knowledge anchors pointing at `node`: the memory ids anchored
    /// to it, with each anchor's edge. This is the graph→memory direction of
    /// the join.
    pub fn memories_anchored_to(
        &self,
        node: &GraphNodeId,
    ) -> Result<Vec<GraphEdge>, GraphStoreError> {
        self.anchor_edges("to_id", node.as_str())
    }

    /// Active anchors owned by a memory entry: the code nodes it is anchored
    /// to. This is the memory→graph direction of the join.
    pub fn anchors_of_memory(
        &self,
        memory_id: &localmind_core::MemoryEntryId,
    ) -> Result<Vec<GraphEdge>, GraphStoreError> {
        self.anchor_edges("from_id", memory_id.as_str())
    }

    fn anchor_edges(&self, column: &str, value: &str) -> Result<Vec<GraphEdge>, GraphStoreError> {
        // `column` is one of two fixed identifiers, never user input.
        let sql = format!(
            "SELECT edge_json FROM graph_edges
             WHERE {column} = ?1 AND kind = 'anchored_to' AND superseded_at IS NULL
             ORDER BY id"
        );
        let mut statement = self
            .connection
            .prepare(&sql)
            .map_err(GraphStoreError::Sqlite)?;
        let rows = statement
            .query_map(params![value], |row| row.get::<_, String>(0))
            .map_err(GraphStoreError::Sqlite)?;
        let mut edges = Vec::new();
        for row in rows {
            let json = row.map_err(GraphStoreError::Sqlite)?;
            edges.push(serde_json::from_str(&json).map_err(GraphStoreError::Deserialize)?);
        }
        Ok(edges)
    }

    /// Test nodes attached to `target` via `tested_by` edges.
    pub fn tests_of(&self, target: &GraphNodeId) -> Result<Vec<GraphNode>, GraphStoreError> {
        self.outgoing(target, localmind_core::EdgeKind::TestedBy)
    }

    /// One shortest path of node ids from `start` to `goal`, bounded by
    /// `max_depth` hops, or `None` when no path exists within the bound.
    pub fn path_between(
        &self,
        start: &GraphNodeId,
        goal: &GraphNodeId,
        max_depth: u32,
    ) -> Result<Option<Vec<GraphNodeId>>, GraphStoreError> {
        let trail: Option<String> = self
            .connection
            .query_row(
                r#"
                WITH RECURSIVE walk(id, depth, trail) AS (
                    VALUES (?1, 0, ?1)
                    UNION
                    SELECT step.next_id, walk.depth + 1,
                           walk.trail || char(31) || step.next_id
                    FROM walk
                    JOIN (
                        SELECT from_id AS this_id, to_id AS next_id FROM graph_edges
                        WHERE superseded_at IS NULL
                            AND from_scope = 'node' AND to_scope = 'node'
                        UNION ALL
                        SELECT to_id AS this_id, from_id AS next_id FROM graph_edges
                        WHERE superseded_at IS NULL
                            AND from_scope = 'node' AND to_scope = 'node'
                    ) AS step ON step.this_id = walk.id
                    WHERE walk.depth < ?3
                        AND instr(walk.trail, step.next_id) = 0
                )
                SELECT trail FROM walk WHERE id = ?2
                ORDER BY depth LIMIT 1
                "#,
                params![start.as_str(), goal.as_str(), max_depth],
                |row| row.get(0),
            )
            .optional()
            .map_err(GraphStoreError::Sqlite)?;
        Ok(trail.map(|trail| {
            trail
                .split('\u{1f}')
                .map(GraphNodeId::new)
                .collect::<Vec<_>>()
        }))
    }
}

fn endpoint_columns(endpoint: &GraphEndpoint) -> (&'static str, &str) {
    match endpoint {
        GraphEndpoint::Node(id) => ("node", id.as_str()),
        GraphEndpoint::Memory(id) => ("memory", id.as_str()),
    }
}

fn collect_nodes(
    rows: impl Iterator<Item = Result<String, rusqlite::Error>>,
) -> Result<Vec<GraphNode>, GraphStoreError> {
    let mut nodes = Vec::new();
    for row in rows {
        let json = row.map_err(GraphStoreError::Sqlite)?;
        nodes.push(serde_json::from_str(&json).map_err(GraphStoreError::Deserialize)?);
    }
    Ok(nodes)
}

fn now_string() -> String {
    OffsetDateTime::now_utc().to_string()
}

#[derive(Debug, Error)]
pub enum GraphStoreError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error("failed to create LocalMind state directory {path:?}: {source}")]
    CreateStateDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to open LocalMind database {path:?}: {source}")]
    OpenDatabase {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("stored graph format version {stored} is newer than supported version {supported}")]
    UnsupportedFormatVersion { stored: i64, supported: i64 },
    #[error("graph node {id} does not exist")]
    MissingNode { id: GraphNodeId },
    #[error("failed to serialize graph record: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to deserialize graph record: {0}")]
    Deserialize(#[source] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use super::{GraphStore, GraphStoreError, GRAPH_FORMAT_VERSION};
    use localmind_core::{
        content_fingerprint, Confidence, EdgeDerivation, EdgeKind, EvidenceKind, EvidenceRef,
        GraphEdge, GraphNode, GraphNodeId, MemoryEntryId, NodeKind,
    };
    use rusqlite::{params, Connection};

    fn store() -> Result<GraphStore, GraphStoreError> {
        let connection = Connection::open_in_memory().map_err(GraphStoreError::Sqlite)?;
        GraphStore::from_connection(connection)
    }

    fn evidence() -> EvidenceRef {
        EvidenceRef::new(EvidenceKind::Other("code_parse".to_string()), "span")
    }

    fn confidence(value: f32) -> Confidence {
        match Confidence::new(value) {
            Ok(confidence) => confidence,
            Err(_) => unreachable!("test confidence values are in range"),
        }
    }

    fn node(kind: NodeKind, qualified_name: &str) -> GraphNode {
        let name = qualified_name.rsplit("::").next().unwrap_or(qualified_name);
        GraphNode::new(
            kind,
            name,
            qualified_name,
            content_fingerprint(qualified_name),
            evidence(),
            confidence(0.9),
        )
    }

    fn parsed_edge(kind: EdgeKind, from: &GraphNode, to: &GraphNode) -> GraphEdge {
        GraphEdge::structural(
            kind,
            from.id.clone(),
            to.id.clone(),
            EdgeDerivation::Parsed,
            confidence(1.0),
            evidence(),
        )
    }

    #[test]
    fn nodes_and_edges_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let function = node(NodeKind::Function, "geometry::Point::norm");
        let test = node(NodeKind::Test, "geometry::tests::norm_works");
        let edge = parsed_edge(EdgeKind::TestedBy, &function, &test);

        store.upsert_node(&function)?;
        store.upsert_node(&test)?;
        store.upsert_edge(&edge)?;

        assert_eq!(store.node(&function.id)?, Some(function.clone()));
        assert_eq!(store.edge(&edge.id)?, Some(edge));
        assert_eq!(store.nodes_by_kind(NodeKind::Function)?, vec![function]);
        Ok(())
    }

    #[test]
    fn upsert_replaces_changed_nodes() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let mut function = node(NodeKind::Function, "geometry::Point::norm");
        store.upsert_node(&function)?;

        function.content_hash = content_fingerprint("changed body");
        store.upsert_node(&function)?;

        let stored = store.node(&function.id)?.ok_or("node missing")?;
        assert_eq!(stored.content_hash, function.content_hash);
        Ok(())
    }

    #[test]
    fn provenance_and_confidence_survive_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let function = node(NodeKind::Function, "geometry::Point::norm");
        let anchor = GraphEdge::anchor(
            MemoryEntryId::new("memory-1"),
            function.id.clone(),
            confidence(0.7),
            evidence(),
        );
        store.upsert_node(&function)?;
        store.upsert_edge(&anchor)?;

        let stored_node = store.node(&function.id)?.ok_or("node missing")?;
        assert_eq!(stored_node.provenance, function.provenance);
        assert_eq!(stored_node.confidence, function.confidence);

        let stored_edge = store.edge(&anchor.id)?.ok_or("edge missing")?;
        assert_eq!(stored_edge.confidence, anchor.confidence);
        assert_eq!(stored_edge.evidence, anchor.evidence);
        Ok(())
    }

    #[test]
    fn superseded_nodes_leave_traversal_but_keep_their_row(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let function = node(NodeKind::Function, "geometry::Point::norm");
        store.upsert_node(&function)?;

        store.supersede_node(&function.id)?;

        assert!(store.nodes_by_kind(NodeKind::Function)?.is_empty());
        let kept = store.node(&function.id)?.ok_or("row missing")?;
        assert!(kept.superseded_at.is_some());
        Ok(())
    }

    #[test]
    fn superseding_a_missing_node_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let missing = GraphNodeId::new("cgn-missing");
        assert!(matches!(
            store.supersede_node(&missing),
            Err(GraphStoreError::MissingNode { .. })
        ));
        Ok(())
    }

    #[test]
    fn fresh_store_records_current_format_version() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        assert_eq!(store.format_version()?, GRAPH_FORMAT_VERSION);
        Ok(())
    }

    #[test]
    fn older_format_version_drops_rows_and_upgrades() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let function = node(NodeKind::Function, "geometry::Point::norm");
        store.upsert_node(&function)?;
        store.connection.execute(
            "UPDATE graph_meta SET format_version = ?1",
            params![GRAPH_FORMAT_VERSION - 1],
        )?;

        store.migrate()?;

        assert_eq!(store.format_version()?, GRAPH_FORMAT_VERSION);
        assert!(store.node(&function.id)?.is_none());
        Ok(())
    }

    #[test]
    fn newer_format_version_refuses_to_open() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        store.connection.execute(
            "UPDATE graph_meta SET format_version = ?1",
            params![GRAPH_FORMAT_VERSION + 1],
        )?;

        assert!(matches!(
            store.migrate(),
            Err(GraphStoreError::UnsupportedFormatVersion { .. })
        ));
        Ok(())
    }

    #[test]
    fn current_format_version_migrate_is_a_no_op() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let function = node(NodeKind::Function, "geometry::Point::norm");
        store.upsert_node(&function)?;

        store.migrate()?;

        assert!(store.node(&function.id)?.is_some());
        Ok(())
    }

    #[test]
    fn neighbors_walk_both_directions_with_depth_bound() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let file = node(NodeKind::File, "src/geometry.rs");
        let function = node(NodeKind::Function, "geometry::Point::norm");
        let test = node(NodeKind::Test, "geometry::tests::norm_works");
        for graph_node in [&file, &function, &test] {
            store.upsert_node(graph_node)?;
        }
        store.upsert_edge(&parsed_edge(EdgeKind::ImplementedBy, &file, &function))?;
        store.upsert_edge(&parsed_edge(EdgeKind::TestedBy, &function, &test))?;

        let one_hop = store.neighbors(&file.id, 1)?;
        assert_eq!(one_hop, vec![function.clone()]);

        let two_hops = store.neighbors(&file.id, 2)?;
        assert_eq!(two_hops.len(), 2);
        assert!(two_hops.contains(&test));

        let from_test = store.neighbors(&test.id, 2)?;
        assert!(from_test.contains(&file));
        Ok(())
    }

    #[test]
    fn directional_walks_and_tests_of() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let caller = node(NodeKind::Function, "geometry::draw");
        let callee = node(NodeKind::Function, "geometry::Point::norm");
        let test = node(NodeKind::Test, "geometry::tests::norm_works");
        for graph_node in [&caller, &callee, &test] {
            store.upsert_node(graph_node)?;
        }
        store.upsert_edge(&parsed_edge(EdgeKind::Uses, &caller, &callee))?;
        store.upsert_edge(&parsed_edge(EdgeKind::TestedBy, &callee, &test))?;

        assert_eq!(
            store.outgoing(&caller.id, EdgeKind::Uses)?,
            vec![callee.clone()]
        );
        assert_eq!(store.incoming(&callee.id, EdgeKind::Uses)?, vec![caller]);
        assert_eq!(store.tests_of(&callee.id)?, vec![test]);
        Ok(())
    }

    #[test]
    fn path_between_finds_shortest_route_within_bound() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        let repo = node(NodeKind::Repository, "workspace");
        let file = node(NodeKind::File, "src/geometry.rs");
        let function = node(NodeKind::Function, "geometry::Point::norm");
        for graph_node in [&repo, &file, &function] {
            store.upsert_node(graph_node)?;
        }
        store.upsert_edge(&parsed_edge(EdgeKind::BelongsToProject, &file, &repo))?;
        store.upsert_edge(&parsed_edge(EdgeKind::ImplementedBy, &file, &function))?;

        let path = store
            .path_between(&repo.id, &function.id, 4)?
            .ok_or("expected a path")?;
        assert_eq!(path, vec![repo.id.clone(), file.id, function.id.clone()]);

        let too_shallow = store.path_between(&repo.id, &function.id, 1)?;
        assert!(too_shallow.is_none());
        Ok(())
    }

    #[test]
    fn graph_persists_across_reopens_in_the_project_database(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        std::fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;

        let function = node(NodeKind::Function, "geometry::Point::norm");
        let test = node(NodeKind::Test, "geometry::tests::norm_works");
        {
            let store = GraphStore::open_project(temp_dir.path())?;
            store.upsert_node(&function)?;
            store.upsert_node(&test)?;
            store.upsert_edge(&parsed_edge(EdgeKind::TestedBy, &function, &test))?;
        }

        // The graph shares the project database with the review queue; both
        // must keep working after the graph tables exist.
        crate::ReviewQueue::open_project(temp_dir.path())?;

        let reopened = GraphStore::open_project(temp_dir.path())?;
        assert_eq!(reopened.format_version()?, GRAPH_FORMAT_VERSION);
        assert_eq!(reopened.tests_of(&function.id)?, vec![test]);
        Ok(())
    }

    #[test]
    fn traversal_stays_fast_on_a_repo_sized_graph() -> Result<(), Box<dyn std::error::Error>> {
        let store = store()?;
        // Shape modelled on a large workspace: 200 files belonging to one
        // repository, 40 functions per file (8,000 functions), a test for
        // every fourth function, and a call edge chaining functions together.
        let repo = node(NodeKind::Repository, "workspace");
        store.upsert_node(&repo)?;
        let mut functions = Vec::new();
        for file_index in 0..200 {
            let file = node(NodeKind::File, &format!("src/module_{file_index}.rs"));
            store.upsert_node(&file)?;
            store.upsert_edge(&parsed_edge(EdgeKind::BelongsToProject, &file, &repo))?;
            for function_index in 0..40 {
                let function = node(
                    NodeKind::Function,
                    &format!("module_{file_index}::function_{function_index}"),
                );
                store.upsert_node(&function)?;
                store.upsert_edge(&parsed_edge(EdgeKind::ImplementedBy, &file, &function))?;
                if function_index % 4 == 0 {
                    let test = node(
                        NodeKind::Test,
                        &format!("module_{file_index}::tests::function_{function_index}"),
                    );
                    store.upsert_node(&test)?;
                    store.upsert_edge(&parsed_edge(EdgeKind::TestedBy, &function, &test))?;
                }
                if let Some(previous) = functions.last() {
                    store.upsert_edge(&parsed_edge(EdgeKind::Uses, previous, &function))?;
                }
                functions.push(function);
            }
        }

        let probe = &functions[functions.len() / 2];
        let started = std::time::Instant::now();
        let neighborhood = store.neighbors(&probe.id, 3)?;
        let neighbors_elapsed = started.elapsed();

        let started = std::time::Instant::now();
        let path = store.path_between(&functions[0].id, &functions[40].id, 6)?;
        let path_elapsed = started.elapsed();

        assert!(!neighborhood.is_empty());
        assert!(path.is_some());
        println!(
            "graph traversal timings: neighbors(depth 3) {neighbors_elapsed:?}, \
             path_between(bound 6) {path_elapsed:?}"
        );
        // Generous CI-safe bound; the recorded local timings live in the
        // store's performance notes.
        assert!(neighbors_elapsed.as_secs() < 5);
        assert!(path_elapsed.as_secs() < 5);
        Ok(())
    }
}
