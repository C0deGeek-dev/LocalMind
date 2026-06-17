use crate::{
    MemoryPathResolver, ProjectConfig, ReviewQueue, ReviewQueueError, ReviewQueueItem,
    StoreConfigError,
};
use localmind_core::{
    content_fingerprint, AuditEventKind, Confidence, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, ReviewItemId, ReviewState, SkillDraft,
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
    /// Index timestamp of the entry, as stored (RFC-ish text).
    pub created_at: String,
    /// Flagged by change-aware invalidation: the code this memory was anchored to
    /// changed, so it may be stale and is awaiting review. Still served — callers
    /// down-rank or mark it rather than dropping it.
    pub stale_candidate: bool,
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
            supersedes: target.iter().cloned().collect(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        };
        // The Markdown file is written first; the single transaction that
        // indexes it and records the audit row is the point of truth. A crash
        // after the file write leaves an unindexed file that the next
        // promotion overwrites — never a half-indexed entry.
        let path = MemoryPathResolver::write_memory_file(&self.config, &entry)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(MemoryPersistenceError::Sqlite)?;
        Self::index_memory_with(&tx, &entry, &path)?;
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
        self.embed_memory_if_configured(&entry)?;
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
        let path = MemoryPathResolver::write_memory_file(&self.config, entry)?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(MemoryPersistenceError::Sqlite)?;
        Self::index_memory_with(&tx, entry, &path)?;
        tx.commit().map_err(MemoryPersistenceError::Sqlite)?;
        self.embed_memory_if_configured(entry)?;
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
        let tx = self
            .connection
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
        if vector.is_empty() {
            return Err(MemoryPersistenceError::InvalidVector {
                detail: "vector must not be empty".to_string(),
            });
        }
        let existing: Option<String> = self
            .connection
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
        self.connection
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
        self.write_audit(
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
        let Some(match_expression) = fts_match_expression(query) else {
            return Ok(Vec::new());
        };
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT m.memory_id, m.path, m.body, m.created_at, m.stale_candidate,
                       bm25(memory_fts) AS rank
                FROM memory_fts
                JOIN memory_index m ON m.memory_id = memory_fts.memory_id
                WHERE memory_fts MATCH ?1 AND m.status = 'active'
                ORDER BY rank, m.memory_id
                "#,
            )
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map(params![match_expression], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)? != 0,
                    row.get::<_, f64>(5)?,
                ))
            })
            .map_err(MemoryPersistenceError::Sqlite)?;

        let mut results = Vec::new();
        for row in rows {
            let (memory_id, path, body, created_at, stale_candidate, rank) =
                row.map_err(MemoryPersistenceError::Sqlite)?;
            // bm25 returns a more-negative value for better matches; expose a
            // positive bigger-is-better integer to keep the result contract.
            #[allow(clippy::cast_possible_truncation)] // bounded: bm25 magnitudes are small
            let score = (-rank * 100.0).round() as i64;
            results.push(MemorySearchResult {
                memory_id: MemoryEntryId::new(memory_id),
                path: PathBuf::from(path),
                score: score.max(1),
                snippet: body.chars().take(160).collect(),
                created_at,
                stale_candidate,
            });
        }
        Ok(results)
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
                &serde_json::json!({ "reason": "anchored code changed" }),
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

    /// Writes the index, FTS, and relationship rows for `entry` on the given
    /// connection. Callers run this inside a transaction so the rows appear
    /// atomically.
    fn index_memory_with(
        connection: &Connection,
        entry: &MemoryEntry,
        path: &Path,
    ) -> Result<(), MemoryPersistenceError> {
        connection
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
                    status = excluded.status,
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
                    OffsetDateTime::now_utc().to_string()
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
        entry: &MemoryEntry,
    ) -> Result<(), MemoryPersistenceError> {
        let capability = InferenceCapability::from_settings(self.config.config.inference.as_ref())?;
        let Some(endpoint) = capability.embeddings() else {
            return Ok(());
        };
        let vectors = endpoint.embed(std::slice::from_ref(&entry.body))?;
        let Some(vector) = vectors.first() else {
            return Err(MemoryPersistenceError::InvalidVector {
                detail: "embedding endpoint returned no vectors".to_string(),
            });
        };
        self.record_inference_call("embeddings", "embedding", endpoint.model(), None)?;
        self.upsert_memory_embedding(
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
