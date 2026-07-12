//! Retrieval and search boundary.
//!
//! One search API over the code-structure graph and accepted memory,
//! ranked deterministically (graph proximity, half-life recency, query-term
//! match) with no model and no network.

mod rank;
mod rerank;
mod workspace;

pub use rank::{combined_score, proximity_score, temporal_score, RankingConfig, SearchWeights};
pub use rerank::{rerank_hits, rerank_scored, RerankEmbedder, RerankError, RerankOptions};
pub use workspace::{search_workspace, RankedHit, SearchHitKind, WorkspaceQuery};

use localmind_core::ContextQuery;
use localmind_store::{GraphStore, MemoryPersistence, MemorySearchResult};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("invalid ranking weights: {detail}")]
    InvalidWeights { detail: String },
    #[error(transparent)]
    Graph(#[from] localmind_store::GraphStoreError),
    #[error(transparent)]
    Memory(#[from] localmind_store::MemoryPersistenceError),
    #[error(transparent)]
    Rerank(#[from] RerankError),
}

/// The ranked search path with the optional rerank stage wired in. Runs the
/// deterministic blend, then applies `rerank` — which is identity unless its
/// flag is on *and* an embedder is supplied. With `RerankOptions::default()`
/// (flag off, no embedder) the result is byte-identical to [`search_workspace`],
/// so the determinism floor holds through this entry point too.
pub fn search_workspace_reranked(
    graph: &GraphStore,
    memory: &MemoryPersistence,
    query: &WorkspaceQuery,
    config: &RankingConfig,
    rerank: &RerankOptions<'_>,
) -> Result<Vec<RankedHit>, SearchError> {
    let hits = search_workspace(graph, memory, query, config)?;
    Ok(rerank_hits(hits, &query.text, rerank)?)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCapabilities {
    pub keyword: bool,
    pub sqlite_fts: bool,
    pub vector: bool,
    pub graph: bool,
}

impl SearchCapabilities {
    #[must_use]
    pub fn mvp() -> Self {
        Self {
            keyword: true,
            sqlite_fts: true,
            vector: true,
            graph: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HybridMemoryResult {
    pub memory: MemorySearchResult,
    pub keyword_score: f32,
    pub vector_score: f32,
    pub combined_score: f32,
}

pub fn hybrid_memory_search(
    persistence: &MemoryPersistence,
    query: &str,
    query_vector: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<HybridMemoryResult>, SearchError> {
    let keyword_results = persistence.search(query)?;
    if keyword_results.is_empty() && query_vector.is_none() {
        return Ok(Vec::new());
    }

    let mut by_id = BTreeMap::new();
    for result in keyword_results {
        let keyword_score = result.score as f32;
        by_id.insert(
            result.memory_id.to_string(),
            HybridMemoryResult {
                memory: result,
                keyword_score,
                vector_score: 0.0,
                combined_score: keyword_score,
            },
        );
    }

    if let Some(vector) = query_vector {
        for result in persistence.vector_search(vector, limit.max(20))? {
            if result.subject_kind != "memory" {
                continue;
            }
            if let Some(existing) = by_id.get_mut(&result.subject_id) {
                existing.vector_score = result.score.max(0.0) * 100.0;
            }
        }
    }

    for result in by_id.values_mut() {
        result.combined_score = result.keyword_score + result.vector_score;
    }
    let mut results: Vec<_> = by_id.into_values().collect();
    results.sort_by(|left, right| {
        right
            .combined_score
            .partial_cmp(&left.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory.memory_id.cmp(&right.memory.memory_id))
    });
    results.truncate(limit);
    Ok(results)
}

/// Down-weight — never drop — synced lessons whose origin machine differs from
/// the one retrieving them, then re-sort. A machine-specific lesson (a local
/// path, a GPU/driver quirk) should rank below an equally-relevant same-machine
/// lesson on another box, but must stay retrievable. `current_device` is this
/// machine's label; `weight` in `(0, 1)` is the penalty factor (a value `>= 1`,
/// or an empty `current_device`, is a no-op). A hit with no recorded origin, or
/// one from this machine, is untouched.
pub fn downweight_foreign_env(hits: &mut [HybridMemoryResult], current_device: &str, weight: f32) {
    if current_device.is_empty() || !(0.0..1.0).contains(&weight) {
        return;
    }
    for hit in hits.iter_mut() {
        if let Some(origin) = hit.memory.origin_device.as_deref() {
            if !origin.is_empty() && origin != current_device {
                hit.combined_score *= weight;
            }
        }
    }
    hits.sort_by(|left, right| {
        right
            .combined_score
            .partial_cmp(&left.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.memory.memory_id.cmp(&right.memory.memory_id))
    });
}

pub fn query_needs_project_scope(query: &ContextQuery) -> bool {
    query.project_uri.is_some()
}

#[cfg(test)]
mod tests {
    use super::{hybrid_memory_search, query_needs_project_scope, SearchCapabilities};
    use localmind_core::ContextQuery;
    use localmind_store::MemoryPersistence;
    use std::fs;

    #[test]
    fn mvp_search_runs_on_local_indexes_and_the_graph() {
        let capabilities = SearchCapabilities::mvp();

        assert!(capabilities.keyword);
        assert!(capabilities.sqlite_fts);
        assert!(capabilities.graph);
        assert!(capabilities.vector);
    }

    #[test]
    fn context_queries_can_be_scoped_to_a_project() {
        let query = ContextQuery {
            text: "testing strategy".to_string(),
            project_uri: Some("file:///repo".to_string()),
            token_budget: Some(1_000),
        };

        assert!(query_needs_project_scope(&query));
    }

    #[test]
    fn foreign_env_downweight_reorders_but_never_drops() {
        use super::{downweight_foreign_env, HybridMemoryResult};
        use localmind_core::{EpistemicStatus, MemoryEntryId};
        use localmind_store::MemorySearchResult;
        use std::path::PathBuf;

        fn hit(id: &str, score: f32, origin: Option<&str>) -> HybridMemoryResult {
            HybridMemoryResult {
                memory: MemorySearchResult {
                    memory_id: MemoryEntryId::new(id),
                    path: PathBuf::new(),
                    score: score as i64,
                    snippet: String::new(),
                    category: "CodePattern".to_string(),
                    created_at: String::new(),
                    stale_candidate: false,
                    epistemic_status: EpistemicStatus::Procedure,
                    contradicted: false,
                    hit_count: 0,
                    origin_device: origin.map(str::to_string),
                },
                keyword_score: score,
                vector_score: 0.0,
                combined_score: score,
            }
        }

        // A foreign-machine hit narrowly outranks a same-machine one before the
        // down-weight; after it, the same-machine hit leads — but both remain.
        let mut hits = vec![
            hit("foreign", 100.0, Some("Laptop")),
            hit("local", 95.0, Some("PC")),
            hit("unstamped", 90.0, None),
        ];
        downweight_foreign_env(&mut hits, "PC", 0.85);
        assert_eq!(hits.len(), 3, "nothing is filtered out");
        assert_eq!(hits[0].memory.memory_id.as_str(), "local");
        // foreign: 100 * 0.85 = 85 < 90 (unstamped, untouched) < 95 (local).
        assert_eq!(hits[1].memory.memory_id.as_str(), "unstamped");
        assert_eq!(hits[2].memory.memory_id.as_str(), "foreign");

        // A weight of 1.0 (disabled) leaves the original order.
        let mut disabled = vec![
            hit("foreign", 100.0, Some("Laptop")),
            hit("local", 95.0, Some("PC")),
        ];
        downweight_foreign_env(&mut disabled, "PC", 1.0);
        assert_eq!(disabled[0].memory.memory_id.as_str(), "foreign");
    }

    #[test]
    fn hybrid_search_uses_vectors_without_requiring_them() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let persistence = MemoryPersistence::open_project(temp_dir.path())?;

        assert!(hybrid_memory_search(&persistence, "anything", None, 5)?.is_empty());
        Ok(())
    }
}
