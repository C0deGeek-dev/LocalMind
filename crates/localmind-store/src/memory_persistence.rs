use crate::{
    MemoryPathResolver, ProjectConfig, ReviewQueue, ReviewQueueError, ReviewQueueItem,
    StoreConfigError,
};
use localmind_core::{
    content_fingerprint, AuditEventKind, CandidateDestination, Confidence, EpistemicStatus,
    MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus, ReviewItemId, ReviewState, SkillDraft,
};
use localmind_inference::{InferenceCapability, InferenceError, TokenUsage};
use rusqlite::{params, Connection, OptionalExtension};
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
    /// The memory's lesson category (e.g. `SecurityWarning`, `ProjectConvention`),
    /// exposed at retrieval time so a caller can gate or dedup injection by
    /// category without a second lookup.
    pub category: String,
    /// Index timestamp of the entry, as stored (RFC-ish text).
    pub created_at: String,
    /// Flagged by change-aware invalidation: the code this memory was anchored to
    /// changed, so it may be stale and is awaiting review. Still served — callers
    /// down-rank or mark it rather than dropping it.
    pub stale_candidate: bool,
    /// The memory's deterministic epistemic status (observation/hypothesis/fact/
    /// decision/procedure), so trust is legible at retrieval time.
    pub epistemic_status: EpistemicStatus,
    /// True when this memory is in a `contradicts` relationship with another, so
    /// the agent can flag the conflict instead of asserting one side blindly.
    pub contradicted: bool,
}

/// The provenance answer for one memory — "why do you think that?". Source
/// session, confidence, epistemic status, and the memories it contradicts.
#[derive(Clone, Debug, PartialEq)]
pub struct MemoryProvenance {
    pub memory_id: MemoryEntryId,
    pub source_session: Option<String>,
    pub confidence: f32,
    pub epistemic_status: EpistemicStatus,
    pub status: String,
    pub stale_candidate: bool,
    pub contradicts: Vec<MemoryEntryId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorSearchResult {
    pub subject_kind: String,
    pub subject_id: String,
    pub score: f32,
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

/// The machine-wide global memory store: a separate SQLite index and Markdown
/// root under the per-user home, shared by every project on the machine. Opened
/// alongside the project store only when the project opts in to `GlobalUser`
/// scope, so a global lesson is never written or read without consent.
struct GlobalStore {
    /// The global index/FTS/vector database, distinct from the project database.
    /// The Markdown root is resolved from the project config's
    /// `global_memory_root()`, so it is not duplicated here.
    connection: Connection,
}

pub struct MemoryPersistence {
    config: ProjectConfig,
    connection: Connection,
    global: Option<GlobalStore>,
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
        let global = Self::open_global(&config)?;
        let persistence = Self {
            config,
            connection,
            global,
        };
        persistence.migrate()?;
        Ok(persistence)
    }

    /// Open the machine-wide global store when the project opts in. The global
    /// database lives beside the global memory root's parent (`~/.localmind/`), so
    /// it is shared across projects and resolved independently of any project.
    fn open_global(config: &ProjectConfig) -> Result<Option<GlobalStore>, MemoryPersistenceError> {
        if !config.allows_global() {
            return Ok(None);
        }
        let Some(root) = config.global_memory_root() else {
            return Ok(None);
        };
        // The DB sits in the global root's parent (the `.localmind/` state dir),
        // mirroring the project layout (`project/.localmind/localmind.sqlite` with
        // memory under `project/.localmind/memory`).
        let state_dir = root.parent().unwrap_or(root.as_path()).to_path_buf();
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
        crate::schema::migrate(&connection).map_err(MemoryPersistenceError::Schema)?;
        Ok(Some(GlobalStore { connection }))
    }

    /// The connection that owns an entry of the given scope: the global store for
    /// `GlobalUser`, otherwise the project store. Errors when a global entry is
    /// requested but the project did not open a global store.
    fn connection_for(&self, scope: &MemoryScope) -> Result<&Connection, MemoryPersistenceError> {
        match scope {
            MemoryScope::GlobalUser => self
                .global
                .as_ref()
                .map(|store| &store.connection)
                .ok_or(MemoryPersistenceError::GlobalScopeDisabled),
            _ => Ok(&self.connection),
        }
    }

    pub fn migrate(&self) -> Result<(), MemoryPersistenceError> {
        crate::schema::migrate(&self.connection).map_err(MemoryPersistenceError::Schema)
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
        // A supersede decision retires its target: the new memory records the
        // target in `supersedes`, and the same transaction flips the target to
        // `Superseded` so retrieval (filtered to `status = 'active'`) stops
        // serving it.
        let target = item.supersede_target.clone();
        // Route the promoted memory to the project or the machine-wide global
        // store: an explicit `GlobalMemory` suggestion or the conservative
        // category classifier asks for global, but only when the project opts in
        // to the `GlobalUser` scope (otherwise it stays project — a safe
        // fallback, never an error). The store that owns the scope owns the
        // index, so a global lesson lands in the database every project reads.
        let wants_global = item.candidate.suggested_destination.is_global()
            || CandidateDestination::default_for_category(&item.candidate.category).is_global();
        let scope = if wants_global && self.config.allows_global() {
            MemoryScope::GlobalUser
        } else {
            MemoryScope::Project
        };
        let connection = self.connection_for(&scope)?;
        let entry = MemoryEntry {
            id: MemoryEntryId::new(item.candidate.id.as_str()),
            scope,
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
            supersedes: target.iter().cloned().collect(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        };
        // The Markdown file is written first; the single transaction that
        // indexes it and records the audit row is the point of truth. A crash
        // after the file write leaves an unindexed file that the next
        // promotion overwrites — never a half-indexed entry.
        // A lesson named only by idiom inherits the language of the workspace it
        // was learned in (the project this promotion runs against).
        let session_language =
            crate::language::detect_workspace_language(&self.config.project_root);
        let path = MemoryPathResolver::write_memory_file(&self.config, &entry)?;
        let tx = connection
            .unchecked_transaction()
            .map_err(MemoryPersistenceError::Sqlite)?;
        Self::index_memory_with(&tx, &entry, &path, session_language)?;
        if let Some(target) = &target {
            Self::supersede_memory_with(&tx, target)?;
            Self::write_audit_with(
                &tx,
                AuditEventKind::MemorySuperseded,
                item.reviewer.as_deref().unwrap_or("unknown"),
                target.as_str(),
                &serde_json::json!({
                    "superseded_by": entry.id.to_string(),
                    "review_item": item.id.to_string(),
                }),
            )?;
        }
        Self::write_audit_with(
            &tx,
            AuditEventKind::MemoryPromoted,
            item.reviewer.as_deref().unwrap_or("unknown"),
            entry.id.as_str(),
            &serde_json::json!({
                "review_item": item.id.to_string(),
                "session": item.session_id.to_string(),
            }),
        )?;
        tx.commit().map_err(MemoryPersistenceError::Sqlite)?;
        self.embed_memory_if_configured(connection, &entry)?;
        Ok(entry)
    }

    /// Flips a memory's index status to `Superseded` so retrieval (which filters
    /// to `status = 'active'`) stops returning it. The Markdown body and the
    /// index row are kept — supersession is reversible and provenance survives.
    fn supersede_memory_with(
        connection: &Connection,
        target: &MemoryEntryId,
    ) -> Result<(), MemoryPersistenceError> {
        // Lowercase to match the `'active'` literal the index is written with and
        // the `status = 'active'` retrieval filter.
        connection
            .execute(
                "UPDATE memory_index SET status = 'superseded' WHERE memory_id = ?1",
                params![target.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Ok(())
    }

    /// Persists an accepted memory entry: the Markdown file plus its search
    /// index row. Review-queue promotion goes through here; hosts accepting
    /// memory through their own review surfaces may use it directly.
    ///
    /// The index, FTS, and relationship rows commit in one transaction after
    /// the file write, so the database never sees a partially indexed entry.
    pub fn persist_memory_entry(
        &self,
        entry: &MemoryEntry,
    ) -> Result<PathBuf, MemoryPersistenceError> {
        // The Markdown path resolves to the global root for a `GlobalUser` entry
        // and the project root otherwise; the index transaction goes to the store
        // that owns the scope, so a global lesson lands in the machine-wide
        // database that every project reads.
        let connection = self.connection_for(&entry.scope)?;
        // A directly-persisted (seeded) entry carries no session context; its
        // language comes from the body alone (the body-wins branch).
        let path = MemoryPathResolver::write_memory_file(&self.config, entry)?;
        let tx = connection
            .unchecked_transaction()
            .map_err(MemoryPersistenceError::Sqlite)?;
        Self::index_memory_with(&tx, entry, &path, None)?;
        tx.commit().map_err(MemoryPersistenceError::Sqlite)?;
        self.embed_memory_if_configured(connection, entry)?;
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
            &serde_json::json!({
                "state": format!("{:?}", item.state),
                "session": item.session_id.to_string(),
                "action": item.reviewer_action.as_deref().unwrap_or_default(),
            }),
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
            &serde_json::json!({ "query": query, "target": target }),
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
            &serde_json::json!({ "name": draft.name, "disabled": true }),
        )
    }

    pub fn record_inference_call(
        &self,
        feature: &str,
        endpoint_kind: &str,
        model: &str,
        usage: Option<&TokenUsage>,
    ) -> Result<(), MemoryPersistenceError> {
        self.write_audit(
            AuditEventKind::InferenceCallCompleted,
            "localmind",
            feature,
            &serde_json::json!({
                "feature": feature,
                "endpoint_kind": endpoint_kind,
                "model": model,
                "prompt_tokens": usage.and_then(|value| value.prompt_tokens),
                "completion_tokens": usage.and_then(|value| value.completion_tokens),
                "total_tokens": usage.and_then(|value| value.total_tokens),
            }),
        )
    }

    pub fn list_memory(&self) -> Result<Vec<MemoryRecord>, MemoryPersistenceError> {
        // Project memory, then global memory when the global store is open, so
        // `memory list` shows every accepted memory the session can retrieve.
        let mut records = Self::list_in(&self.connection)?;
        if let Some(global) = &self.global {
            records.extend(Self::list_in(&global.connection)?);
        }
        Ok(records)
    }

    fn list_in(connection: &Connection) -> Result<Vec<MemoryRecord>, MemoryPersistenceError> {
        let mut statement = connection
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
        // The memory may live in the project store or the machine-wide global
        // store; resolve which one holds it (project first), along with that
        // store's connection and root for the containment check.
        let (connection, root, path) =
            if let Some(path) = Self::memory_path_in(&self.connection, memory_id)? {
                (&self.connection, self.config.memory_root(), path)
            } else if let (Some(global), Some(global_root)) =
                (&self.global, self.config.global_memory_root())
            {
                match Self::memory_path_in(&global.connection, memory_id)? {
                    Some(path) => (&global.connection, global_root, path),
                    None => return Ok(false),
                }
            } else {
                return Ok(false);
            };

        if !path_is_under_root(&root, &path) {
            return Err(MemoryPersistenceError::UnsafeIndexedMemoryPath { path });
        }

        // The file goes first: a crash between the file removal and the
        // transaction below leaves a stale index row pointing at a missing
        // file, and re-running the delete heals it (missing files are
        // tolerated). The reverse order would leave the body on disk with no
        // index row left to find it by.
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

        // Relationships, FTS, index, and the audit row commit atomically: no
        // crash point can leave the database referencing half a memory.
        let tx = connection
            .unchecked_transaction()
            .map_err(MemoryPersistenceError::Sqlite)?;
        tx.execute(
            "DELETE FROM memory_relationships WHERE memory_id = ?1",
            params![memory_id.as_str()],
        )
        .map_err(MemoryPersistenceError::Sqlite)?;
        tx.execute(
            "DELETE FROM memory_fts WHERE memory_id = ?1",
            params![memory_id.as_str()],
        )
        .map_err(MemoryPersistenceError::Sqlite)?;
        tx.execute(
            "DELETE FROM memory_index WHERE memory_id = ?1",
            params![memory_id.as_str()],
        )
        .map_err(MemoryPersistenceError::Sqlite)?;
        tx.execute(
            "DELETE FROM vector_index WHERE subject_kind = 'memory' AND subject_id = ?1",
            params![memory_id.as_str()],
        )
        .map_err(MemoryPersistenceError::Sqlite)?;
        Self::write_audit_with(
            &tx,
            AuditEventKind::MemoryDeleted,
            actor,
            memory_id.as_str(),
            &serde_json::json!({}),
        )?;
        tx.commit().map_err(MemoryPersistenceError::Sqlite)?;
        Ok(true)
    }

    pub fn upsert_memory_embedding(
        &self,
        memory_id: &MemoryEntryId,
        source_fingerprint: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<bool, MemoryPersistenceError> {
        Self::upsert_memory_embedding_with(
            &self.connection,
            memory_id,
            source_fingerprint,
            model,
            vector,
        )
    }

    /// Upsert a memory's embedding on the given connection (project or global),
    /// so a global memory's vector lands in the global database.
    fn upsert_memory_embedding_with(
        connection: &Connection,
        memory_id: &MemoryEntryId,
        source_fingerprint: &str,
        model: &str,
        vector: &[f32],
    ) -> Result<bool, MemoryPersistenceError> {
        if vector.is_empty() {
            return Err(MemoryPersistenceError::InvalidVector {
                detail: "vector must not be empty".to_string(),
            });
        }
        let existing: Option<String> = connection
            .query_row(
                "SELECT source_fingerprint FROM vector_index WHERE subject_kind = 'memory' AND subject_id = ?1",
                params![memory_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(MemoryPersistenceError::Sqlite)?;
        if existing.as_deref() == Some(source_fingerprint) {
            return Ok(false);
        }

        let blob = encode_vector(vector);
        connection
            .execute(
                r#"
                INSERT INTO vector_index
                (subject_kind, subject_id, source_fingerprint, model, dimensions, vector_blob, updated_at)
                VALUES('memory', ?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(subject_kind, subject_id) DO UPDATE SET
                    source_fingerprint = excluded.source_fingerprint,
                    model = excluded.model,
                    dimensions = excluded.dimensions,
                    vector_blob = excluded.vector_blob,
                    updated_at = excluded.updated_at
                "#,
                params![
                    memory_id.as_str(),
                    source_fingerprint,
                    model,
                    i64::try_from(vector.len()).map_err(|_| MemoryPersistenceError::InvalidVector {
                        detail: "vector has too many dimensions".to_string(),
                    })?,
                    blob,
                    OffsetDateTime::now_utc().to_string()
                ],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Self::write_audit_with(
            connection,
            AuditEventKind::VectorIndexUpdated,
            "localmind",
            memory_id.as_str(),
            &serde_json::json!({
                "subject_kind": "memory",
                "model": model,
                "dimensions": vector.len(),
            }),
        )?;
        Ok(true)
    }

    pub fn vector_search(
        &self,
        query_vector: &[f32],
        limit: usize,
    ) -> Result<Vec<VectorSearchResult>, MemoryPersistenceError> {
        if query_vector.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT subject_kind, subject_id, dimensions, vector_blob FROM vector_index ORDER BY subject_kind, subject_id",
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            })
            .map_err(MemoryPersistenceError::Sqlite)?;
        let mut scored = Vec::new();
        for row in rows {
            let (subject_kind, subject_id, dimensions, blob) =
                row.map_err(MemoryPersistenceError::Sqlite)?;
            let vector = decode_vector(&blob)?;
            if usize::try_from(dimensions).ok() != Some(vector.len())
                || vector.len() != query_vector.len()
            {
                continue;
            }
            scored.push(VectorSearchResult {
                subject_kind,
                subject_id,
                score: cosine_similarity(query_vector, &vector),
            });
        }
        scored.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.subject_id.cmp(&right.subject_id))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    /// Searches active memory through the FTS5 index (`memory_fts MATCH` with
    /// bm25 ranking). Whitespace-separated query terms are OR-ed as quoted
    /// prefix phrases, so FTS5 operators in user input are inert text, not
    /// syntax. Higher `score` is a better match.
    pub fn search(&self, query: &str) -> Result<Vec<MemorySearchResult>, MemoryPersistenceError> {
        self.search_lang(query, None)
    }

    /// Like [`search`](Self::search) but restricting matches to a programming
    /// language: when `language` is `Some`, a memory tagged with a *different*
    /// language is excluded, while a `NULL`-tagged (general / cross-cutting)
    /// memory always remains eligible. `None` applies no language filter. The
    /// filter runs inside the FTS query so retrieval returns rows that are
    /// already language-relevant rather than dropping them after ranking.
    pub fn search_lang(
        &self,
        query: &str,
        language: Option<&str>,
    ) -> Result<Vec<MemorySearchResult>, MemoryPersistenceError> {
        // Merge project + global results with **project precedence**: project
        // matches lead, then global matches that are not already present (deduped
        // by memory id and by body), so a project lesson always overrides a
        // global one on conflict while a global lesson still surfaces when no
        // project lesson applies. Provenance survives in each result's `path`
        // (a global path lives under the user-home store).
        let mut results = Self::search_in(&self.connection, query, language)?;
        if let Some(global) = &self.global {
            let project_ids: std::collections::HashSet<String> = results
                .iter()
                .map(|r| r.memory_id.as_str().to_string())
                .collect();
            let project_bodies: std::collections::HashSet<String> =
                results.iter().map(|r| r.snippet.clone()).collect();
            for result in Self::search_in(&global.connection, query, language)? {
                if !project_ids.contains(result.memory_id.as_str())
                    && !project_bodies.contains(&result.snippet)
                {
                    results.push(result);
                }
            }
        }
        Ok(results)
    }

    /// Run the FTS5 memory search against one connection (project or global).
    /// When `language` is `Some`, off-language memories are excluded in the query
    /// (`NULL`-tagged memories always pass).
    fn search_in(
        connection: &Connection,
        query: &str,
        language: Option<&str>,
    ) -> Result<Vec<MemorySearchResult>, MemoryPersistenceError> {
        let Some(match_expression) = fts_match_expression(query) else {
            return Ok(Vec::new());
        };
        // A single statement string keeps the prepared shape stable; the language
        // clause is appended only when filtering so the unfiltered path is byte
        // for byte what it was before.
        let language_clause = if language.is_some() {
            " AND (m.language = ?2 OR m.language IS NULL)"
        } else {
            ""
        };
        let statement_sql = format!(
            r#"
                SELECT m.memory_id, m.path, m.body, m.created_at, m.stale_candidate,
                       m.epistemic_status, m.contradicted, m.category, bm25(memory_fts) AS rank
                FROM memory_fts
                JOIN memory_index m ON m.memory_id = memory_fts.memory_id
                WHERE memory_fts MATCH ?1 AND m.status = 'active'{language_clause}
                ORDER BY rank, m.memory_id
                "#,
        );
        let mut statement = connection
            .prepare(&statement_sql)
            .map_err(MemoryPersistenceError::Sqlite)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)? != 0,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)? != 0,
                row.get::<_, String>(7)?,
                row.get::<_, f64>(8)?,
            ))
        };
        let rows = if let Some(language) = language {
            statement.query_map(params![match_expression, language], map_row)
        } else {
            statement.query_map(params![match_expression], map_row)
        }
        .map_err(MemoryPersistenceError::Sqlite)?;

        let mut results = Vec::new();
        for row in rows {
            let (
                memory_id,
                path,
                body,
                created_at,
                stale_candidate,
                epistemic,
                contradicted,
                category,
                rank,
            ) = row.map_err(MemoryPersistenceError::Sqlite)?;
            // bm25 returns a more-negative value for better matches; expose a
            // positive bigger-is-better integer to keep the result contract.
            #[allow(clippy::cast_possible_truncation)] // bounded: bm25 magnitudes are small
            let score = (-rank * 100.0).round() as i64;
            results.push(MemorySearchResult {
                memory_id: MemoryEntryId::new(memory_id),
                path: PathBuf::from(path),
                score: score.max(1),
                snippet: body.chars().take(160).collect(),
                category,
                created_at,
                stale_candidate,
                epistemic_status: EpistemicStatus::from_token(&epistemic),
                contradicted,
            });
        }
        Ok(results)
    }

    /// The provenance for a memory — source session, confidence, epistemic
    /// status, staleness, and the memories it contradicts — or `None` when the
    /// memory id is unknown. Answers "why do you think that?".
    ///
    /// # Errors
    /// Returns [`MemoryPersistenceError::Sqlite`] when the query fails.
    pub fn provenance(
        &self,
        memory_id: &MemoryEntryId,
    ) -> Result<Option<MemoryProvenance>, MemoryPersistenceError> {
        let row = self
            .connection
            .query_row(
                "SELECT source_session, status, epistemic_status, contradicted, confidence, \
                 stale_candidate FROM memory_index WHERE memory_id = ?1",
                params![memory_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)? != 0,
                        row.get::<_, f64>(4)?,
                        row.get::<_, i64>(5)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(MemoryPersistenceError::Sqlite)?;
        let Some((source_session, status, epistemic, _contradicted, confidence, stale_candidate)) =
            row
        else {
            return Ok(None);
        };

        let mut statement = self
            .connection
            .prepare(
                "SELECT target FROM memory_relationships \
                 WHERE memory_id = ?1 AND relation_kind = 'contradicts' ORDER BY target",
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let targets = statement
            .query_map(params![memory_id.as_str()], |row| row.get::<_, String>(0))
            .map_err(MemoryPersistenceError::Sqlite)?;
        let mut contradicts = Vec::new();
        for target in targets {
            contradicts.push(MemoryEntryId::new(
                target.map_err(MemoryPersistenceError::Sqlite)?,
            ));
        }

        Ok(Some(MemoryProvenance {
            memory_id: memory_id.clone(),
            source_session,
            #[allow(clippy::cast_possible_truncation)]
            confidence: confidence as f32,
            epistemic_status: EpistemicStatus::from_token(&epistemic),
            status,
            stale_candidate,
            contradicts,
        }))
    }

    /// Flag an active memory as a change-aware staleness candidate: the code it
    /// was anchored to changed, so it should be reviewed. The memory stays active
    /// and retrievable — this only sets the flag (and audits it). Returns whether
    /// an active memory matched.
    ///
    /// # Errors
    /// Returns [`MemoryPersistenceError::Sqlite`] when the update or audit fails.
    pub fn mark_stale_candidate(
        &self,
        memory_id: &MemoryEntryId,
    ) -> Result<bool, MemoryPersistenceError> {
        self.flag_for_review(memory_id, "anchored code changed")
    }

    /// Flag an active memory for review with a caller-supplied reason, without
    /// deleting it — the route-to-review path shared by change-aware invalidation
    /// (`mark_stale_candidate`) and outcome-aware down-weighting (a lesson that did
    /// not improve eval outcomes). The memory stays active and retrievable; this
    /// only sets the staleness flag and audits the reason. Returns whether an
    /// active memory matched.
    ///
    /// # Errors
    /// Returns [`MemoryPersistenceError::Sqlite`] when the update or audit fails.
    pub fn flag_for_review(
        &self,
        memory_id: &MemoryEntryId,
        reason: &str,
    ) -> Result<bool, MemoryPersistenceError> {
        let changed = self
            .connection
            .execute(
                "UPDATE memory_index SET stale_candidate = 1 \
                 WHERE memory_id = ?1 AND status = 'active' AND stale_candidate = 0",
                params![memory_id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        if changed > 0 {
            self.write_audit(
                AuditEventKind::MemoryFlaggedStale,
                "localmind",
                memory_id.as_str(),
                &serde_json::json!({ "reason": reason }),
            )?;
        }
        Ok(changed > 0)
    }

    /// Clear a memory's staleness flag (e.g. a reviewer confirmed it still holds).
    /// Returns whether a row changed.
    ///
    /// # Errors
    /// Returns [`MemoryPersistenceError::Sqlite`] when the update fails.
    pub fn clear_stale_candidate(
        &self,
        memory_id: &MemoryEntryId,
    ) -> Result<bool, MemoryPersistenceError> {
        let changed = self
            .connection
            .execute(
                "UPDATE memory_index SET stale_candidate = 0 WHERE memory_id = ?1",
                params![memory_id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Ok(changed > 0)
    }

    /// The active memories currently flagged as staleness candidates — the review
    /// list a reviewer or the inspector pulls.
    ///
    /// # Errors
    /// Returns [`MemoryPersistenceError::Sqlite`] when the query fails.
    pub fn list_stale_candidates(&self) -> Result<Vec<MemoryEntryId>, MemoryPersistenceError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT memory_id FROM memory_index \
                 WHERE status = 'active' AND stale_candidate = 1 ORDER BY memory_id",
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(MemoryPersistenceError::Sqlite)?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(MemoryEntryId::new(
                row.map_err(MemoryPersistenceError::Sqlite)?,
            ));
        }
        Ok(ids)
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

    /// The on-disk path of an active memory in the given store, or `None` when the
    /// id is unknown there — used to locate a memory across the project and global
    /// stores before deleting it.
    fn memory_path_in(
        connection: &Connection,
        memory_id: &MemoryEntryId,
    ) -> Result<Option<PathBuf>, MemoryPersistenceError> {
        let mut statement = connection
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

    /// Writes the index, FTS, and relationship rows for `entry` on the given
    /// connection. Callers run this inside a transaction so the rows appear
    /// atomically.
    fn index_memory_with(
        connection: &Connection,
        entry: &MemoryEntry,
        path: &Path,
        session_language: Option<&str>,
    ) -> Result<(), MemoryPersistenceError> {
        let epistemic_status = EpistemicStatus::from_category(&entry.category);
        // The single language this lesson is about (or NULL for a general /
        // cross-cutting one), resolved once here: the body wins when it names a
        // language, else a language-bound category inherits the session's, so a
        // lesson named only by idiom is still tagged and filtered in retrieval.
        let language = crate::language::resolve_memory_language(
            &entry.category,
            entry.body.as_str(),
            session_language,
        );
        connection
            .execute(
                r#"
                INSERT INTO memory_index
                (memory_id, path, scope, category, body, source_session, status, created_at,
                 epistemic_status, confidence, language)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?10)
                ON CONFLICT(memory_id) DO UPDATE SET
                    path = excluded.path,
                    scope = excluded.scope,
                    category = excluded.category,
                    body = excluded.body,
                    source_session = excluded.source_session,
                    status = excluded.status,
                    epistemic_status = excluded.epistemic_status,
                    confidence = excluded.confidence,
                    language = excluded.language,
                    -- Re-promoting a memory refreshes it, clearing any prior
                    -- change-aware staleness flag.
                    stale_candidate = 0
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
                    OffsetDateTime::now_utc().to_string(),
                    epistemic_status.as_str(),
                    entry.confidence.value(),
                    language,
                ],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        connection
            .execute(
                "DELETE FROM memory_fts WHERE memory_id = ?1",
                params![entry.id.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        connection
            .execute(
                "INSERT INTO memory_fts(memory_id, body) VALUES(?1, ?2)",
                params![entry.id.as_str(), entry.body.as_str()],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Self::record_relationships_with(connection, entry)?;
        Self::record_contradictions_with(connection, entry)?;
        Ok(())
    }

    /// Record `contradicts` relationships for a freshly-indexed memory: the
    /// entry's explicitly-declared contradictions, plus a deterministic
    /// auto-detection — an active memory that shares a topic (`related_entities`)
    /// but takes the opposite recommendation polarity (one prohibits what the
    /// other endorses). Each contradiction is stored both ways and flags both
    /// memories `contradicted`, so retrieval can surface the conflict. Nothing is
    /// removed — a contradiction is a *signal*, not a deletion (D-LM-0008).
    fn record_contradictions_with(
        connection: &Connection,
        entry: &MemoryEntry,
    ) -> Result<(), MemoryPersistenceError> {
        let mut targets: std::collections::BTreeSet<String> = entry
            .contradicts
            .iter()
            .map(|id| id.as_str().to_string())
            .collect();

        if !entry.related_entities.is_empty() {
            let entry_prohibits = body_prohibits(&entry.body);
            let placeholders = std::iter::repeat_n("?", entry.related_entities.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT DISTINCT r.memory_id, m.body FROM memory_relationships r \
                 JOIN memory_index m ON m.memory_id = r.memory_id \
                 WHERE r.relation_kind = 'entity' AND r.target IN ({placeholders}) \
                 AND m.status = 'active' AND m.memory_id != ?{self_param}",
                self_param = entry.related_entities.len() + 1
            );
            let mut statement = connection
                .prepare(&sql)
                .map_err(MemoryPersistenceError::Sqlite)?;
            let mut bound: Vec<String> = entry.related_entities.clone();
            bound.push(entry.id.as_str().to_string());
            let rows = statement
                .query_map(rusqlite::params_from_iter(bound), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(MemoryPersistenceError::Sqlite)?;
            for row in rows {
                let (other_id, other_body) = row.map_err(MemoryPersistenceError::Sqlite)?;
                if body_prohibits(&other_body) != entry_prohibits {
                    targets.insert(other_id);
                }
            }
        }

        for target in targets {
            // Store the relationship both directions, idempotently.
            for (memory_id, other) in [
                (entry.id.as_str(), target.as_str()),
                (target.as_str(), entry.id.as_str()),
            ] {
                connection
                    .execute(
                        "INSERT OR IGNORE INTO memory_relationships(memory_id, relation_kind, target) \
                         VALUES(?1, 'contradicts', ?2)",
                        params![memory_id, other],
                    )
                    .map_err(MemoryPersistenceError::Sqlite)?;
            }
            // Flag both memories so retrieval surfaces the conflict.
            connection
                .execute(
                    "UPDATE memory_index SET contradicted = 1 \
                     WHERE memory_id IN (?1, ?2) AND status = 'active'",
                    params![entry.id.as_str(), target],
                )
                .map_err(MemoryPersistenceError::Sqlite)?;
        }
        Ok(())
    }

    fn record_relationships_with(
        connection: &Connection,
        entry: &MemoryEntry,
    ) -> Result<(), MemoryPersistenceError> {
        Self::relationship_with(
            connection,
            entry,
            "category",
            &format!("{:?}", entry.category),
        )?;
        if let Some(session_id) = &entry.source_session {
            Self::relationship_with(connection, entry, "session", session_id.as_str())?;
        }
        for file in &entry.related_files {
            Self::relationship_with(connection, entry, "file", file)?;
        }
        for entity in &entry.related_entities {
            Self::relationship_with(connection, entry, "entity", entity)?;
        }
        Ok(())
    }

    fn relationship_with(
        connection: &Connection,
        entry: &MemoryEntry,
        kind: &str,
        target: &str,
    ) -> Result<(), MemoryPersistenceError> {
        connection
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
        metadata: &serde_json::Value,
    ) -> Result<(), MemoryPersistenceError> {
        Self::write_audit_with(&self.connection, kind, actor, subject, metadata)
    }

    fn embed_memory_if_configured(
        &self,
        connection: &Connection,
        entry: &MemoryEntry,
    ) -> Result<(), MemoryPersistenceError> {
        let capability = InferenceCapability::from_settings(self.config.config.inference.as_ref())?;
        let Some(endpoint) = capability.embeddings() else {
            return Ok(());
        };
        // Embedding is a best-effort addendum to an already-committed memory: the
        // Markdown file and the index row are durable before we get here, so a
        // down/slow embedding endpoint (or a transient vector-store write) must
        // never fail the promotion or the caller's turn. On any failure, record a
        // skip in the audit trail and fall back to the lexical contract — the
        // memory is still retrievable, just without a vector until re-embedded.
        if let Err(error) = Self::embed_and_store_memory(connection, endpoint, entry) {
            let _ = Self::write_audit_with(
                connection,
                AuditEventKind::InferenceCallFailed,
                "localmind",
                "embeddings",
                &serde_json::json!({
                    "feature": "embeddings",
                    "endpoint_kind": "embedding",
                    "model": endpoint.model(),
                    "outcome": "skipped",
                    "error": error.to_string(),
                }),
            );
        }
        Ok(())
    }

    /// Embed `entry`'s body against `endpoint` and upsert the vector into
    /// `vector_index` (with the inference audit), on the connection that owns the
    /// memory's scope. Returns `Err` on an endpoint/vector-store failure; the
    /// caller decides whether that is fatal — for promotion it is **not** (see
    /// [`embed_memory_if_configured`](Self::embed_memory_if_configured)).
    fn embed_and_store_memory(
        connection: &Connection,
        endpoint: &localmind_inference::EmbeddingEndpoint,
        entry: &MemoryEntry,
    ) -> Result<(), MemoryPersistenceError> {
        let vectors = endpoint.embed(std::slice::from_ref(&entry.body))?;
        let Some(vector) = vectors.first() else {
            return Err(MemoryPersistenceError::InvalidVector {
                detail: "embedding endpoint returned no vectors".to_string(),
            });
        };
        // The inference audit and the vector both land in the same store that owns
        // the memory (global vs project), so a global memory's vector is in the
        // global database where global retrieval can find it.
        Self::write_audit_with(
            connection,
            AuditEventKind::InferenceCallCompleted,
            "localmind",
            "embeddings",
            &serde_json::json!({
                "feature": "embeddings",
                "endpoint_kind": "embedding",
                "model": endpoint.model(),
            }),
        )?;
        Self::upsert_memory_embedding_with(
            connection,
            &entry.id,
            &content_fingerprint(entry.body.as_str()),
            endpoint.model(),
            vector,
        )?;
        Ok(())
    }

    pub fn record_custom_audit(
        &self,
        kind: AuditEventKind,
        actor: &str,
        subject: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), MemoryPersistenceError> {
        self.write_audit(kind, actor, subject, metadata)
    }

    /// Inserts one audit row on the given connection. Metadata is a
    /// `serde_json::Value` so callers cannot hand-build malformed JSON.
    fn write_audit_with(
        connection: &Connection,
        kind: AuditEventKind,
        actor: &str,
        subject: &str,
        metadata: &serde_json::Value,
    ) -> Result<(), MemoryPersistenceError> {
        connection
            .execute(
                "INSERT INTO audit_events(kind, actor, subject, metadata_json, happened_at) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    format!("{kind:?}"),
                    actor,
                    subject,
                    metadata.to_string(),
                    OffsetDateTime::now_utc().to_string()
                ],
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        Ok(())
    }
}

/// Turns free-text user input into an FTS5 MATCH expression: each
/// whitespace-separated term becomes a quoted prefix phrase (`"term"*`),
/// OR-ed together. Quoting neutralizes FTS5 query syntax (`OR`, `-`, `NEAR`,
/// parentheses) in user input; embedded double quotes are doubled per FTS5
/// string rules. Returns `None` for an empty query.
fn fts_match_expression(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" OR "))
}

/// Whether a memory body recommends *against* something — a prohibition. Used to
/// detect contradictions: two memories about the same topic with opposite
/// polarity (one prohibits what the other endorses) conflict.
fn body_prohibits(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "do not",
        "don't",
        "never ",
        "avoid ",
        "stop ",
        "no longer",
        "instead of",
        "must not",
        "should not",
        "shouldn't",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn path_is_under_root(root: &Path, path: &Path) -> bool {
    let root = canonicalize_lenient(root);
    let path = canonicalize_lenient(path);
    path.starts_with(root)
}

/// Canonicalizes a path that may no longer exist: a deleted memory file must
/// still compare correctly against its canonicalized root (on Windows,
/// `canonicalize` adds a `\\?\` prefix that a raw fallback path lacks), so
/// fall back to canonicalizing the parent and re-joining the file name.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(parent) = fs::canonicalize(parent) {
            return parent.join(name);
        }
    }
    path.to_path_buf()
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

fn decode_vector(blob: &[u8]) -> Result<Vec<f32>, MemoryPersistenceError> {
    let chunks = blob.chunks_exact(4);
    if !chunks.remainder().is_empty() {
        return Err(MemoryPersistenceError::InvalidVector {
            detail: "vector blob length is not divisible by four".to_string(),
        });
    }
    Ok(chunks
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (left, right) in left.iter().zip(right) {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
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
    #[error(transparent)]
    Inference(#[from] InferenceError),
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
    #[error("global-scope memory requires the project to allow the GlobalUser scope")]
    GlobalScopeDisabled,
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
    Schema(#[from] crate::schema::SchemaError),
    #[error("invalid vector index data: {detail}")]
    InvalidVector { detail: String },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
