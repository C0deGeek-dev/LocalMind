//! One readiness snapshot of a project store, shared by the CLI `status`
//! command and the MCP `memory_status` tool so the two can never drift.
//!
//! [`StatusSnapshot::read`] is deliberately **best-effort and total**: a missing
//! or invalid `.localmind.toml`, or a store that will not open, yields
//! `ready = false` (with the reason captured in `notes`) rather than an error —
//! so a caller can always report *something* structured (the MCP read in
//! particular must never turn a not-ready project into a tool error).

use std::path::Path;

/// What the embedding endpoint can actually do for this project, right now.
///
/// A bare "configured" flag conflated three states that need different answers
/// from a user: no endpoint set, an endpoint set but unreachable, and a healthy
/// one. The middle state is the one that matters — it is the one that leaves the
/// semantic paths degraded while every other readiness signal stays green.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingCapability {
    /// No `[inference]` embedding endpoint configured. Retrieval is lexical by
    /// choice, not by failure.
    NotConfigured,
    /// An endpoint is configured but did not answer. Semantic retrieval and
    /// embed-on-promotion are degraded; the lexical paths are unaffected.
    Unreachable { endpoint: String, error: String },
    /// Configured and answering.
    Healthy { endpoint: String },
}

impl EmbeddingCapability {
    /// A one-line report for a human-facing surface.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::NotConfigured => "not configured (retrieval is lexical)".to_string(),
            Self::Unreachable { endpoint, error } => {
                format!("DEGRADED - {endpoint} is configured but unreachable: {error}")
            }
            Self::Healthy { endpoint } => format!("healthy ({endpoint})"),
        }
    }
}

/// A point-in-time readiness snapshot of one project's LocalMind store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// Config discovered, store opened, review queue read, and all counts read —
    /// the same readiness the CLI `status` command reports. Any failure flips
    /// this false and records a line in [`notes`](Self::notes).
    pub ready: bool,
    /// `[learning].enabled` in the resolved config.
    pub learning_enabled: bool,
    /// An `[inference]` block is configured (model-backed paths available).
    ///
    /// Kept as the coarse flag existing callers read; prefer
    /// [`embedding`](Self::embedding), which distinguishes an unreachable
    /// endpoint from an absent one.
    pub inference_configured: bool,
    /// What the embedding endpoint can actually do, probed rather than assumed.
    pub embedding: EmbeddingCapability,
    /// How much accepted memory carries an embedding, and how many vectors are
    /// left over from rows that are no longer active.
    pub memory_vectors: crate::MemoryVectorCoverage,
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
    /// One line per component that failed to read (empty when healthy). Preserves
    /// the diagnostic detail a bare `ready = false` would otherwise discard.
    pub notes: Vec<String>,
}

/// Whether a persisted scope label names the machine-wide global store. Memory
/// rows persist their scope as the `Debug` label (`"GlobalUser"` / `"Project"`,
/// see `memory_persistence`), so this matches that exact label — a lowercase
/// substring check silently misclassifies every global row as project.
#[must_use]
fn is_global_scope(scope: &str) -> bool {
    scope == "GlobalUser"
}

impl StatusSnapshot {
    /// Read a best-effort snapshot for `project`. Never errors.
    #[must_use]
    pub fn read(project: &Path) -> Self {
        let mut snapshot = Self {
            ready: true,
            learning_enabled: false,
            inference_configured: false,
            embedding: EmbeddingCapability::NotConfigured,
            memory_vectors: crate::MemoryVectorCoverage::default(),
            accepted_project: 0,
            accepted_global: 0,
            pending_review: 0,
            doc_chunks: 0,
            doc_vectors: 0,
            schema_version: crate::schema::DB_SCHEMA_VERSION,
            notes: Vec::new(),
        };

        match crate::ProjectConfig::discover(project) {
            Ok(config) => {
                snapshot.learning_enabled = config.config.learning.enabled;
                snapshot.inference_configured = config.config.inference.is_some();
            }
            Err(error) => {
                snapshot.ready = false;
                snapshot.notes.push(format!("config: {error}"));
            }
        }

        match crate::MemoryPersistence::open_project(project) {
            Ok(store) => {
                match store.list_memory() {
                    Ok(memories) => {
                        for record in &memories {
                            if is_global_scope(&record.scope) {
                                snapshot.accepted_global += 1;
                            } else {
                                snapshot.accepted_project += 1;
                            }
                        }
                    }
                    Err(error) => {
                        snapshot.ready = false;
                        snapshot.notes.push(format!("memory list: {error}"));
                    }
                }
                match store.doc_chunk_count() {
                    Ok(count) => snapshot.doc_chunks = count,
                    Err(error) => {
                        snapshot.ready = false;
                        snapshot.notes.push(format!("doc chunks: {error}"));
                    }
                }
                match store.doc_vector_count() {
                    Ok(count) => snapshot.doc_vectors = count,
                    Err(error) => {
                        snapshot.ready = false;
                        snapshot.notes.push(format!("doc vectors: {error}"));
                    }
                }
                match store.memory_vector_coverage() {
                    Ok(coverage) => snapshot.memory_vectors = coverage,
                    Err(error) => {
                        snapshot.ready = false;
                        snapshot.notes.push(format!("memory vectors: {error}"));
                    }
                }
                // Probing is what separates "configured" from "working". A failure
                // here is a degraded capability, never a not-ready store: the
                // lexical paths are untouched and must keep reporting healthy.
                snapshot.embedding = store.embedding_capability();
            }
            Err(error) => {
                snapshot.ready = false;
                snapshot.notes.push(format!("store: {error}"));
            }
        }

        match crate::ReviewQueue::open_project(project) {
            Ok(queue) => match queue.list() {
                Ok(items) => {
                    snapshot.pending_review = items
                        .iter()
                        .filter(|item| item.state == localmind_core::ReviewState::Pending)
                        .count();
                }
                Err(error) => {
                    snapshot.ready = false;
                    snapshot.notes.push(format!("review queue: {error}"));
                }
            },
            Err(error) => {
                snapshot.ready = false;
                snapshot.notes.push(format!("review queue: {error}"));
            }
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
        assert!(!snapshot.notes.is_empty(), "a failure records a note");
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
        assert!(snapshot.notes.is_empty());
        assert!(snapshot.learning_enabled);
        assert!(!snapshot.inference_configured);
        assert_eq!(snapshot.pending_review, 0);
    }

    #[test]
    fn the_persisted_scope_labels_classify_correctly() {
        // Rows persist the Debug label; the classifier must match it exactly.
        assert!(is_global_scope("GlobalUser"));
        assert!(!is_global_scope("Project"));
        assert!(
            !is_global_scope("global_user"),
            "lowercase is not the label"
        );
    }
}
