use crate::{
    MemoryPathResolver, ProjectConfig, ReviewQueue, ReviewQueueError, ReviewQueueItem,
    StoreConfigError,
};
use localmind_core::{
    AuditEventKind, Confidence, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus,
    ReviewItemId, ReviewState, SkillDraft,
};
use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;
use time::OffsetDateTime;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditRecord {
    pub id: i64,
    pub kind: String,
    pub actor: String,
    pub subject: String,
    pub metadata_json: String,
    pub happened_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemorySearchResult {
    pub memory_id: MemoryEntryId,
    pub path: PathBuf,
    pub score: i64,
    pub snippet: String,
    /// Index timestamp of the entry, as stored (RFC-ish text).
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemoryRecord {
    pub memory_id: MemoryEntryId,
    pub path: PathBuf,
    pub scope: String,
    pub category: String,
    pub status: String,
    pub body: String,
}

pub struct MemoryPersistence {
    config: ProjectConfig,
    connection: Connection,
}

impl MemoryPersistence {
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self, MemoryPersistenceError> {
        let config =
            ProjectConfig::discover(project_root).map_err(MemoryPersistenceError::Config)?;
        let state_dir = config.project_root.join(".localmind");
        fs::create_dir_all(&state_dir).map_err(|source| {
            MemoryPersistenceError::CreateStateDir {
                path: state_dir.clone(),
                source,
            }
        })?;
        let db_path = state_dir.join("localmind.sqlite");
        let connection =
            Connection::open(&db_path).map_err(|source| MemoryPersistenceError::OpenDatabase {
                path: db_path,
                source,
            })?;
        let persistence = Self { config, connection };
        persistence.migrate()?;
        Ok(persistence)
    }

    pub fn migrate(&self) -> Result<(), MemoryPersistenceError> {
        self.connection
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS audit_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    kind TEXT NOT NULL,
                    actor TEXT NOT NULL,
                    subject TEXT NOT NULL,
                    metadata_json TEXT NOT NULL,
                    happened_at TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS memory_index (
                    memory_id TEXT PRIMARY KEY,
                    path TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    category TEXT NOT NULL,
                    body TEXT NOT NULL,
                    source_session TEXT,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );

                CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
                    USING fts5(memory_id UNINDEXED, body);

                CREATE TABLE IF NOT EXISTS memory_relationships (
                    memory_id TEXT NOT NULL,
                    relation_kind TEXT NOT NULL,
                    target TEXT NOT NULL,
                    PRIMARY KEY(memory_id, relation_kind, target)
                );
                "#,
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Ok(())
    }

    pub fn promote_review_item(
        &self,
        item_id: &ReviewItemId,
    ) -> Result<MemoryEntry, MemoryPersistenceError> {
        let queue = ReviewQueue::open_project(&self.config.project_root)?;
        let item =
            queue
                .get(item_id)?
                .ok_or_else(|| MemoryPersistenceError::MissingReviewItem {
                    item_id: item_id.clone(),
                })?;
        if !matches!(item.state, ReviewState::Accepted | ReviewState::Edited) {
            return Err(MemoryPersistenceError::ReviewItemNotAccepted {
                item_id: item_id.clone(),
                state: format!("{:?}", item.state),
            });
        }

        let body = item
            .replacement_summary
            .clone()
            .unwrap_or_else(|| item.candidate.summary().to_string());
        let entry = MemoryEntry {
            id: MemoryEntryId::new(item.candidate.id.as_str()),
            scope: MemoryScope::Project,
            body,
            category: item.candidate.category.clone(),
            confidence: Confidence::new(item.candidate.confidence.value())?,
            source_session: Some(item.session_id.clone()),
            evidence: item.candidate.evidence().to_vec(),
            tags: vec!["accepted".to_string()],
            related_files: item.candidate.related_files.clone(),
            related_entities: item.candidate.related_entities.clone(),
            created_at: Some(OffsetDateTime::now_utc()),
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        };
        self.persist_memory_entry(&entry)?;
        self.write_audit(
            AuditEventKind::MemoryPromoted,
            item.reviewer.as_deref().unwrap_or("unknown"),
            entry.id.as_str(),
            &format!(
                r#"{{"review_item":"{}","session":"{}"}}"#,
                item.id, item.session_id
            ),
        )?;
        Ok(entry)
    }

    /// Persists an accepted memory entry: the Markdown file plus its search
    /// index row. Review-queue promotion goes through here; hosts accepting
    /// memory through their own review surfaces may use it directly.
    pub fn persist_memory_entry(
        &self,
        entry: &MemoryEntry,
    ) -> Result<PathBuf, MemoryPersistenceError> {
        let path = MemoryPathResolver::write_memory_file(&self.config, entry)?;
        self.index_memory(entry, &path)?;
        Ok(path)
    }

    pub fn record_review_audit(&self) -> Result<usize, MemoryPersistenceError> {
        let queue = ReviewQueue::open_project(&self.config.project_root)?;
        let mut count = 0;
        for item in queue.list()? {
            if item.reviewer_action.is_none() {
                continue;
            }
            self.record_review_item_audit(&item)?;
            count += 1;
        }
        Ok(count)
    }

    pub fn record_review_item_audit(
        &self,
        item: &ReviewQueueItem,
    ) -> Result<(), MemoryPersistenceError> {
        self.write_audit(
            AuditEventKind::ReviewDecisionRecorded,
            item.reviewer.as_deref().unwrap_or("unknown"),
            item.id.as_str(),
            &format!(
                r#"{{"state":"{:?}","session":"{}","action":"{}"}}"#,
                item.state,
                item.session_id,
                item.reviewer_action.as_deref().unwrap_or_default()
            ),
        )
    }

    pub fn record_context_export(
        &self,
        query: &str,
        target: &str,
    ) -> Result<(), MemoryPersistenceError> {
        self.write_audit(
            AuditEventKind::ContextPackExported,
            "cli",
            target,
            &format!(
                r#"{{"query":"{}","target":"{}"}}"#,
                escape_json(query),
                target
            ),
        )
    }

    pub fn record_skill_draft_created(
        &self,
        draft: &SkillDraft,
    ) -> Result<(), MemoryPersistenceError> {
        self.write_audit(
            AuditEventKind::SkillDraftCreated,
            "cli",
            draft.id.as_str(),
            &format!(
                r#"{{"name":"{}","disabled":true}}"#,
                escape_json(&draft.name)
            ),
        )
    }

    pub fn list_memory(&self) -> Result<Vec<MemoryRecord>, MemoryPersistenceError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT memory_id, path, scope, category, status, body
                FROM memory_index
                WHERE status = 'active'
                ORDER BY created_at, memory_id
                "#,
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok(MemoryRecord {
                    memory_id: MemoryEntryId::new(row.get::<_, String>(0)?),
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    scope: row.get(2)?,
                    category: row.get(3)?,
                    status: row.get(4)?,
                    body: row.get(5)?,
                })
            })
            .map_err(MemoryPersistenceError::Sqlite)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(MemoryPersistenceError::Sqlite)?);
        }
        Ok(records)
    }

    pub fn delete_memory(
        &self,
        memory_id: &MemoryEntryId,
        actor: &str,
    ) -> Result<bool, MemoryPersistenceError> {
        let path = self.memory_path(memory_id)?;
        let Some(path) = path else {
            return Ok(false);
        };

        if !path_is_under_root(&self.config.memory_root(), &path) {
            return Err(MemoryPersistenceError::UnsafeIndexedMemoryPath { path });
        }

        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MemoryPersistenceError::DeleteMemoryFile {
                    path: path.clone(),
                    source,
                });
            }
        }

        self.connection
            .execute(
                "DELETE FROM memory_relationships WHERE memory_id = ?1",
                params![memory_id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        self.connection
            .execute(
                "DELETE FROM memory_fts WHERE memory_id = ?1",
                params![memory_id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        self.connection
            .execute(
                "DELETE FROM memory_index WHERE memory_id = ?1",
                params![memory_id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        self.write_audit(
            AuditEventKind::MemoryDeleted,
            actor,
            memory_id.as_str(),
            "{}",
        )?;
        Ok(true)
    }

    pub fn search(&self, query: &str) -> Result<Vec<MemorySearchResult>, MemoryPersistenceError> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_ascii_lowercase())
            .filter(|term| !term.is_empty())
            .collect();
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT memory_id, path, body, created_at
                FROM memory_index
                WHERE status = 'active'
                "#,
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(MemoryPersistenceError::Sqlite)?;

        let mut results = Vec::new();
        for row in rows {
            let (memory_id, path, body, created_at) =
                row.map_err(MemoryPersistenceError::Sqlite)?;
            let body_lower = body.to_ascii_lowercase();
            let score = terms
                .iter()
                .map(|term| body_lower.matches(term).count() as i64)
                .sum::<i64>();
            if score > 0 {
                results.push(MemorySearchResult {
                    memory_id: MemoryEntryId::new(memory_id),
                    path: PathBuf::from(path),
                    score,
                    snippet: body.chars().take(160).collect(),
                    created_at,
                });
            }
        }
        results.sort_by_key(|result| std::cmp::Reverse(result.score));
        Ok(results)
    }

    pub fn audit_records(&self) -> Result<Vec<AuditRecord>, MemoryPersistenceError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, kind, actor, subject, metadata_json, happened_at FROM audit_events ORDER BY id",
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok(AuditRecord {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    actor: row.get(2)?,
                    subject: row.get(3)?,
                    metadata_json: row.get(4)?,
                    happened_at: row.get(5)?,
                })
            })
            .map_err(MemoryPersistenceError::Sqlite)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(MemoryPersistenceError::Sqlite)?);
        }
        Ok(records)
    }

    pub fn relationships_for(
        &self,
        memory_id: &MemoryEntryId,
    ) -> Result<Vec<(String, String)>, MemoryPersistenceError> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT relation_kind, target
                FROM memory_relationships
                WHERE memory_id = ?1
                ORDER BY relation_kind, target
                "#,
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map(params![memory_id.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(MemoryPersistenceError::Sqlite)?;
        let mut relationships = Vec::new();
        for row in rows {
            relationships.push(row.map_err(MemoryPersistenceError::Sqlite)?);
        }
        Ok(relationships)
    }

    fn memory_path(
        &self,
        memory_id: &MemoryEntryId,
    ) -> Result<Option<PathBuf>, MemoryPersistenceError> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM memory_index WHERE memory_id = ?1 AND status = 'active'")
            .map_err(MemoryPersistenceError::Sqlite)?;
        match statement.query_row(params![memory_id.as_str()], |row| {
            Ok(PathBuf::from(row.get::<_, String>(0)?))
        }) {
            Ok(path) => Ok(Some(path)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(MemoryPersistenceError::Sqlite(error)),
        }
    }

    fn index_memory(&self, entry: &MemoryEntry, path: &Path) -> Result<(), MemoryPersistenceError> {
        self.connection
            .execute(
                r#"
                INSERT INTO memory_index
                (memory_id, path, scope, category, body, source_session, status, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7)
                ON CONFLICT(memory_id) DO UPDATE SET
                    path = excluded.path,
                    scope = excluded.scope,
                    category = excluded.category,
                    body = excluded.body,
                    source_session = excluded.source_session,
                    status = excluded.status
                "#,
                params![
                    entry.id.as_str(),
                    path.to_string_lossy().to_string(),
                    format!("{:?}", entry.scope),
                    format!("{:?}", entry.category),
                    entry.body.as_str(),
                    entry
                        .source_session
                        .as_ref()
                        .map(|id| id.as_str().to_string()),
                    OffsetDateTime::now_utc().to_string()
                ],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        self.connection
            .execute(
                "DELETE FROM memory_fts WHERE memory_id = ?1",
                params![entry.id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        self.connection
            .execute(
                "INSERT INTO memory_fts(memory_id, body) VALUES(?1, ?2)",
                params![entry.id.as_str(), entry.body.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        self.record_relationships(entry)?;
        Ok(())
    }

    fn record_relationships(&self, entry: &MemoryEntry) -> Result<(), MemoryPersistenceError> {
        self.relationship(entry, "category", &format!("{:?}", entry.category))?;
        if let Some(session_id) = &entry.source_session {
            self.relationship(entry, "session", session_id.as_str())?;
        }
        for file in &entry.related_files {
            self.relationship(entry, "file", file)?;
        }
        for entity in &entry.related_entities {
            self.relationship(entry, "entity", entity)?;
        }
        Ok(())
    }

    fn relationship(
        &self,
        entry: &MemoryEntry,
        kind: &str,
        target: &str,
    ) -> Result<(), MemoryPersistenceError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO memory_relationships(memory_id, relation_kind, target) VALUES(?1, ?2, ?3)",
                params![entry.id.as_str(), kind, target],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Ok(())
    }

    fn write_audit(
        &self,
        kind: AuditEventKind,
        actor: &str,
        subject: &str,
        metadata_json: &str,
    ) -> Result<(), MemoryPersistenceError> {
        self.connection
            .execute(
                "INSERT INTO audit_events(kind, actor, subject, metadata_json, happened_at) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    format!("{kind:?}"),
                    actor,
                    subject,
                    metadata_json,
                    OffsetDateTime::now_utc().to_string()
                ],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Ok(())
    }
}

fn escape_json(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn path_is_under_root(root: &Path, path: &Path) -> bool {
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(root)
}

#[derive(Debug, Error)]
pub enum MemoryPersistenceError {
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    #[error(transparent)]
    ReviewQueue(#[from] ReviewQueueError),
    #[error(transparent)]
    MemoryPath(#[from] crate::MemoryPathError),
    #[error(transparent)]
    Contract(#[from] localmind_core::ContractError),
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
    #[error("indexed memory path escapes LocalMind memory root: {path:?}")]
    UnsafeIndexedMemoryPath { path: PathBuf },
    #[error("failed to delete LocalMind memory file {path:?}: {source}")]
    DeleteMemoryFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("review item does not exist: {item_id}")]
    MissingReviewItem { item_id: ReviewItemId },
    #[error("review item {item_id} is not accepted or edited: {state}")]
    ReviewItemNotAccepted {
        item_id: ReviewItemId,
        state: String,
    },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
