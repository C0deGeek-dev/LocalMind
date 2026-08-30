//! Re-embedding the rows whose vectors are missing or out of date.
//!
//! Embedding happens once, at promotion or ingest time, and it is best-effort:
//! when the endpoint is down the failure is audited and the row simply carries
//! no vector. Nothing ever retried. A comment promising the memory is
//! "retrievable, just without a vector until re-embedded" described a repair
//! that did not exist, so the first outage was permanent — on the authoring
//! machine that left 276 active memories and 1,802 doc chunks unvectorised.
//!
//! **The sweep set is defined by a query, never by a cursor.** A row is in the
//! set when it has no vector, when its stored content fingerprint no longer
//! matches its body, or when it was embedded under a different model. That
//! definition makes the job idempotent, resumable after any interruption, and
//! correct across content edits and model changes without persisting a scrap of
//! progress state.

use rusqlite::Connection;

use crate::memory_persistence::{MemoryPersistence, MemoryPersistenceError};

/// Why a row is in the sweep set. Reported so an operator can see whether a
/// sweep is repairing an outage, catching up on edits, or re-embedding after a
/// model change — three very different situations with the same remedy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackfillReasons {
    /// No vector row at all.
    pub absent: i64,
    /// A vector exists but the body has changed since it was embedded.
    pub stale_fingerprint: i64,
    /// A vector exists and matches the body, but was embedded under a different
    /// model than the one now configured.
    pub model_mismatch: i64,
}

impl BackfillReasons {
    #[must_use]
    pub fn total(&self) -> i64 {
        self.absent + self.stale_fingerprint + self.model_mismatch
    }
}

/// What a sweep would do, or did, to one store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillPlan {
    pub memories: BackfillReasons,
    pub doc_chunks: BackfillReasons,
    /// Vectors whose memory row is no longer active. Reported separately: they
    /// are not re-embedded, and whether they are pruned is the caller's call.
    pub stale_memory_vectors: i64,
}

impl BackfillPlan {
    #[must_use]
    pub fn total(&self) -> i64 {
        self.memories.total() + self.doc_chunks.total()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// One row to re-embed: what to embed, and where the vector belongs.
pub(crate) struct BackfillItem {
    pub(crate) subject_kind: &'static str,
    pub(crate) subject_id: String,
    pub(crate) body: String,
}

/// The SQL fragment selecting rows whose vector is missing or out of date.
///
/// `model` is compared against the configured embedding model. The fingerprint
/// is compared in Rust rather than SQL because it is a Rust hash of the body,
/// so the query returns candidates and the caller filters.
fn candidate_sql(kind: &str, id_column: &str, table: &str, extra_where: &str) -> String {
    format!(
        "SELECT m.{id_column}, m.body, v.source_fingerprint, v.model \
         FROM {table} m \
         LEFT JOIN vector_index v \
           ON v.subject_kind = '{kind}' AND v.subject_id = m.{id_column} \
         {extra_where}"
    )
}

/// Scan one connection for rows needing a vector.
pub(crate) fn scan_connection(
    connection: &Connection,
    active_model: &str,
) -> Result<(BackfillPlan, Vec<BackfillItem>), MemoryPersistenceError> {
    let mut plan = BackfillPlan::default();
    let mut items = Vec::new();

    let collect = |kind: &'static str,
                   sql: String,
                   reasons: &mut BackfillReasons,
                   items: &mut Vec<BackfillItem>|
     -> Result<(), MemoryPersistenceError> {
        let mut statement = connection
            .prepare(&sql)
            .map_err(MemoryPersistenceError::Sqlite)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(MemoryPersistenceError::Sqlite)?;
        for row in rows {
            let (id, body, fingerprint, model) = row.map_err(MemoryPersistenceError::Sqlite)?;
            let expected = localmind_core::content_fingerprint(&body);
            match (fingerprint, model) {
                (None, _) => reasons.absent += 1,
                (Some(stored), _) if stored != expected => reasons.stale_fingerprint += 1,
                (Some(_), Some(stored_model)) if stored_model != active_model => {
                    reasons.model_mismatch += 1;
                }
                // Current: matching fingerprint under the active model.
                _ => continue,
            }
            items.push(BackfillItem {
                subject_kind: kind,
                subject_id: id,
                body,
            });
        }
        Ok(())
    };

    collect(
        "memory",
        candidate_sql(
            "memory",
            "memory_id",
            "memory_index",
            "WHERE m.status = 'active'",
        ),
        &mut plan.memories,
        &mut items,
    )?;
    collect(
        "doc",
        candidate_sql("doc", "chunk_id", "doc_chunk", ""),
        &mut plan.doc_chunks,
        &mut items,
    )?;

    plan.stale_memory_vectors = connection
        .query_row(
            "SELECT COUNT(*) FROM vector_index v WHERE v.subject_kind = 'memory' \
             AND NOT EXISTS (SELECT 1 FROM memory_index m \
             WHERE m.memory_id = v.subject_id AND m.status = 'active')",
            [],
            |row| row.get(0),
        )
        .map_err(MemoryPersistenceError::Sqlite)?;

    Ok((plan, items))
}

/// What a completed sweep did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// What the sweep set looked like before any writing.
    pub planned: BackfillPlan,
    /// Rows whose vector was written.
    pub embedded: i64,
    /// Rows the endpoint refused or returned nothing for. A partial sweep is
    /// reported, never silently treated as a complete one.
    pub failed: i64,
    /// Vectors pruned because their memory row is no longer active.
    pub pruned: i64,
}

impl MemoryPersistence {
    /// What a sweep would re-embed, without writing anything.
    ///
    /// # Errors
    /// Returns an error if a store query fails, or if no embedding model is
    /// configured — the sweep set is defined relative to the active model, so
    /// there is no meaningful answer without one.
    pub fn backfill_plan(&self) -> Result<BackfillPlan, MemoryPersistenceError> {
        let model = self.active_embedding_model()?;
        let (mut plan, _) = scan_connection(self.connection(), &model)?;
        if let Some(global) = self.global_connection() {
            let (global_plan, _) = scan_connection(global, &model)?;
            plan.memories.absent += global_plan.memories.absent;
            plan.memories.stale_fingerprint += global_plan.memories.stale_fingerprint;
            plan.memories.model_mismatch += global_plan.memories.model_mismatch;
            plan.doc_chunks.absent += global_plan.doc_chunks.absent;
            plan.doc_chunks.stale_fingerprint += global_plan.doc_chunks.stale_fingerprint;
            plan.doc_chunks.model_mismatch += global_plan.doc_chunks.model_mismatch;
            plan.stale_memory_vectors += global_plan.stale_memory_vectors;
        }
        Ok(plan)
    }

    /// Re-embed every row whose vector is missing or out of date.
    ///
    /// Idempotent by construction: the sweep set is a query, so a completed
    /// sweep leaves nothing to do and a re-run is a no-op. An interrupted sweep
    /// simply leaves a smaller set behind — there is no progress state to
    /// corrupt. A row the endpoint refuses is counted as failed and the sweep
    /// continues; a partial repair is reported, never presented as a whole one.
    ///
    /// `prune_stale` removes vectors whose memory row is no longer active.
    ///
    /// # Errors
    /// Returns an error if a store query fails or no embedding model is
    /// configured. Individual embed failures are counted, not returned.
    pub fn backfill_run(
        &self,
        prune_stale: bool,
    ) -> Result<BackfillReport, MemoryPersistenceError> {
        let model = self.active_embedding_model()?;
        let mut report = BackfillReport::default();

        let (plan, items) = scan_connection(self.connection(), &model)?;
        report.planned = plan;
        self.embed_batch(self.connection(), &items, &mut report)?;

        if let Some(global) = self.global_connection() {
            let (global_plan, global_items) = scan_connection(global, &model)?;
            report.planned.memories.absent += global_plan.memories.absent;
            report.planned.memories.stale_fingerprint += global_plan.memories.stale_fingerprint;
            report.planned.memories.model_mismatch += global_plan.memories.model_mismatch;
            report.planned.doc_chunks.absent += global_plan.doc_chunks.absent;
            report.planned.doc_chunks.stale_fingerprint += global_plan.doc_chunks.stale_fingerprint;
            report.planned.doc_chunks.model_mismatch += global_plan.doc_chunks.model_mismatch;
            report.planned.stale_memory_vectors += global_plan.stale_memory_vectors;
            self.embed_batch(global, &global_items, &mut report)?;
        }

        if prune_stale {
            report.pruned += prune_stale_vectors(self.connection())?;
            if let Some(global) = self.global_connection() {
                report.pruned += prune_stale_vectors(global)?;
            }
        }
        Ok(report)
    }

    fn embed_batch(
        &self,
        connection: &Connection,
        items: &[BackfillItem],
        report: &mut BackfillReport,
    ) -> Result<(), MemoryPersistenceError> {
        for item in items {
            match self.reembed_subject(connection, item.subject_kind, &item.subject_id, &item.body)
            {
                Ok(()) => report.embedded += 1,
                // One row the endpoint refused must not abandon the rest: the
                // sweep is the repair path, and stopping at the first failure
                // would make it useless on a flaky endpoint.
                Err(_) => report.failed += 1,
            }
        }
        Ok(())
    }
}

/// Delete memory vectors whose row is no longer active.
fn prune_stale_vectors(connection: &Connection) -> Result<i64, MemoryPersistenceError> {
    let removed = connection
        .execute(
            "DELETE FROM vector_index WHERE subject_kind = 'memory'              AND NOT EXISTS (SELECT 1 FROM memory_index m              WHERE m.memory_id = vector_index.subject_id AND m.status = 'active')",
            [],
        )
        .map_err(MemoryPersistenceError::Sqlite)?;
    i64::try_from(removed).map_err(|_| MemoryPersistenceError::InvalidVector {
        detail: "implausible prune count".to_string(),
    })
}
