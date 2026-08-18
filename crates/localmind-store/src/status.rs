//! One readiness snapshot of a project store, shared by the CLI `status`
//! command and the MCP `memory_status` tool so the two can never drift.
//!
//! [`StatusSnapshot::read`] is deliberately **best-effort and total**: a missing
//! or invalid `.localmind.toml`, or a store that will not open, yields
//! `ready = false` with zeroed counts rather than an error — so a caller can
//! always report *something* structured (the MCP read in particular must never
//! turn a not-ready project into a tool error).

use std::path::Path;

/// A point-in-time readiness snapshot of one project's LocalMind store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// Config discovered, store opened, and review queue opened — the same
    /// readiness the CLI `status` command reports.
    pub ready: bool,
    /// `[learning].enabled` in the resolved config.
    pub learning_enabled: bool,
    /// An `[inference]` block is configured (model-backed paths available).
    pub inference_configured: bool,
    /// Accepted memory rows in the project store.
    pub accepted_project: usize,
    /// Accepted memory rows in the machine-wide global store.
    pub accepted_global: usize,
    /// Candidates awaiting human review.
    pub pending_review: usize,
    /// Ingested documentation chunks.
    pub doc_chunks: i64,
    /// Documentation chunks that carry an embedding.
    pub doc_vectors: i64,
    /// The database schema version this build understands.
    pub schema_version: i32,
}

impl StatusSnapshot {
    /// Read a best-effort snapshot for `project`. Never errors.
    #[must_use]
    pub fn read(project: &Path) -> Self {
        let mut snapshot = Self {
            ready: true,
            learning_enabled: false,
            inference_configured: false,
            accepted_project: 0,
            accepted_global: 0,
            pending_review: 0,
            doc_chunks: 0,
            doc_vectors: 0,
            schema_version: crate::schema::DB_SCHEMA_VERSION,
        };

        match crate::ProjectConfig::discover(project) {
            Ok(config) => {
                snapshot.learning_enabled = config.config.learning.enabled;
                snapshot.inference_configured = config.config.inference.is_some();
            }
            Err(_) => snapshot.ready = false,
        }

        match crate::MemoryPersistence::open_project(project) {
            Ok(store) => {
                if let Ok(memories) = store.list_memory() {
                    for record in &memories {
                        // Scope is serialized snake_case (`global_user`); match on
                        // the stem so the split survives a serialization tweak.
                        if record.scope.contains("global") {
                            snapshot.accepted_global += 1;
                        } else {
                            snapshot.accepted_project += 1;
                        }
                    }
                } else {
                    snapshot.ready = false;
                }
                snapshot.doc_chunks = store.doc_chunk_count().unwrap_or(0);
                snapshot.doc_vectors = store.doc_vector_count().unwrap_or(0);
            }
            Err(_) => snapshot.ready = false,
        }

        match crate::ReviewQueue::open_project(project) {
            Ok(queue) => match queue.list() {
                Ok(items) => {
                    snapshot.pending_review = items
                        .iter()
                        .filter(|item| item.state == localmind_core::ReviewState::Pending)
                        .count();
                }
                Err(_) => snapshot.ready = false,
            },
            Err(_) => snapshot.ready = false,
        }

        snapshot
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_config_is_not_ready_but_still_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot = StatusSnapshot::read(dir.path());
        assert!(!snapshot.ready, "no .localmind.toml ⇒ not ready");
        assert!(!snapshot.learning_enabled);
        assert_eq!(snapshot.accepted_project, 0);
        assert_eq!(snapshot.accepted_global, 0);
        assert_eq!(snapshot.pending_review, 0);
        assert_eq!(snapshot.schema_version, crate::schema::DB_SCHEMA_VERSION);
    }

    #[test]
    fn a_healthy_project_reports_ready_with_its_counts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .unwrap();
        let snapshot = StatusSnapshot::read(dir.path());
        assert!(snapshot.ready);
        assert!(snapshot.learning_enabled);
        assert!(!snapshot.inference_configured);
        assert_eq!(snapshot.pending_review, 0);
    }
}
