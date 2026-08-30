//! Retrieval and search boundary.
//!
//! One search API over the code-structure graph and accepted memory. The
//! reference LocalPilot host consumes [`hybrid_memory_search`] for its live
//! accepted-memory injection path; keyword selection, optional dense RRF,
//! windowing, and cross-device down-weighting therefore have one owner here.
//! The graph/workspace and opt-in embedding-rerank entry points remain reusable
//! engine library surfaces rather than separate host implementations.

mod rank;
mod rerank;
mod workspace;

pub use rank::{combined_score, proximity_score, temporal_score, RankingConfig, SearchWeights};
pub use rerank::{rerank_hits, rerank_scored, RerankEmbedder, RerankError, RerankOptions};
pub use workspace::{search_workspace, RankedHit, SearchHitKind, WorkspaceQuery};

use localmind_core::ContextQuery;
use localmind_store::{
    CoverageGate, GraphStore, MemoryPersistence, MemoryScanStatus, MemorySearchResult,
    VectorSearchResult,
};
use std::collections::{BTreeMap, HashMap};
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
    /// Raw cosine similarity to the query, when this memory has a comparable
    /// stored vector. Kept optional so a genuine zero is not confused with an
    /// unavailable embedding.
    pub cosine: Option<f32>,
    pub combined_score: f32,
}

/// How the hybrid search obtains its semantic ranking.
#[derive(Clone, Copy, Debug)]
pub enum SemanticQuery<'a> {
    /// Embed the query with the configured endpoint and scan memory vectors.
    Configured,
    /// Use a vector the caller already computed. Primarily useful to engine
    /// integrations and deterministic tests that must avoid a second embed.
    Vector(&'a [f32]),
    /// Preserve the exact keyword-only path without attempting an embedding.
    Disabled,
}

/// Options for the single hybrid accepted-memory retrieval boundary.
#[derive(Clone, Copy, Debug)]
pub struct HybridMemorySearchOptions<'a> {
    /// Optional programming-language constraint. General memories remain
    /// eligible, matching [`MemoryPersistence::search_lang`].
    pub language: Option<&'a str>,
    /// Maximum number of final hits.
    pub limit: usize,
    /// How deep the dense ranked list reaches before RRF. The cosine lookup is
    /// intentionally untruncated so the relevance gate can score every keyword
    /// candidate.
    pub dense_rank_window: usize,
    pub semantic: SemanticQuery<'a>,
}

impl HybridMemorySearchOptions<'_> {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            language: None,
            limit,
            dense_rank_window: DEFAULT_DENSE_RANK_WINDOW,
            semantic: SemanticQuery::Configured,
        }
    }
}

/// Default depth of the dense memory ranking fused with the keyword ranking.
pub const DEFAULT_DENSE_RANK_WINDOW: usize = 64;

/// Search accepted memory through the engine-owned hybrid path.
///
/// Keyword/BM25 remains the candidate floor. When semantic retrieval is
/// available, stored-vector cosines are attached to those candidates and, only
/// when `[retrieval] rerank` is active, their keyword and dense positions are
/// fused with Reciprocal Rank Fusion. `rerank_window` bounds movement. Finally,
/// foreign-machine memories are down-weighted using the opened store's sync
/// configuration. Hosts should map these results into their own types rather
/// than rebuilding any part of this ranking path.
pub fn hybrid_memory_search(
    persistence: &MemoryPersistence,
    query: &str,
    options: HybridMemorySearchOptions<'_>,
) -> Result<Vec<HybridMemoryResult>, SearchError> {
    let keyword_results = persistence.search_lang(query, options.language)?;
    hybrid_memory_search_from_candidates(persistence, keyword_results, query, options)
}

/// Evaluation-only variant of [`hybrid_memory_search`] with an explicit term
/// coverage gate. No production host should pass anything but the store's
/// shipped gate; this seam exists so that gate can be measured in isolation.
pub fn hybrid_memory_search_gated_for_eval(
    persistence: &MemoryPersistence,
    query: &str,
    gate: CoverageGate,
    options: HybridMemorySearchOptions<'_>,
) -> Result<Vec<HybridMemoryResult>, SearchError> {
    let keyword_results = persistence.search_lang_gated(query, options.language, gate)?;
    hybrid_memory_search_from_candidates(persistence, keyword_results, query, options)
}

fn hybrid_memory_search_from_candidates(
    persistence: &MemoryPersistence,
    keyword_results: Vec<MemorySearchResult>,
    query: &str,
    options: HybridMemorySearchOptions<'_>,
) -> Result<Vec<HybridMemoryResult>, SearchError> {
    if keyword_results.is_empty() || options.limit == 0 {
        return Ok(Vec::new());
    }

    let mut by_id = BTreeMap::new();
    let mut keyword_ids = Vec::with_capacity(keyword_results.len());
    for result in keyword_results {
        let keyword_score = result.score as f32;
        let id = result.memory_id.to_string();
        keyword_ids.push(id.clone());
        by_id.insert(
            id,
            HybridMemoryResult {
                memory: result,
                keyword_score,
                vector_score: 0.0,
                cosine: None,
                combined_score: keyword_score,
            },
        );
    }

    let dense = semantic_scan(persistence, query, options.semantic)?;
    for result in &dense {
        if let Some(existing) = by_id.get_mut(&result.subject_id) {
            existing.cosine = Some(result.score);
            // Retain the public score scale used by the original additive
            // hybrid surface while exposing the raw cosine separately.
            existing.vector_score = result.score.max(0.0) * 100.0;
        }
    }

    if let Some(window) = persistence.active_memory_rerank_window() {
        let dense_ids: Vec<String> = dense
            .iter()
            .take(options.dense_rank_window)
            .map(|result| result.subject_id.clone())
            .collect();
        let cosines: HashMap<String, f32> = dense
            .iter()
            .map(|result| (result.subject_id.clone(), result.score))
            .collect();
        let (order, scores) = windowed_fusion_order(&keyword_ids, &dense_ids, &cosines, window);
        // The bounded order, not the unbounded RRF order, is the retrieval
        // contract. Make the effective scores strictly monotone in that order
        // before the foreign-environment penalty is applied. Otherwise the
        // penalty's final sort could incidentally reshuffle an untouched tail
        // by its raw RRF scores merely because one foreign hit was present.
        let mut previous_score: Option<f32> = None;
        for id in &order {
            let raw_score = scores.get(id).copied().unwrap_or(0.0) as f32;
            let effective_score = previous_score
                .map(|previous| raw_score.min(previous - f32::EPSILON))
                .unwrap_or(raw_score);
            if let Some(result) = by_id.get_mut(id) {
                result.combined_score = effective_score;
            }
            previous_score = Some(effective_score);
        }
        keyword_ids = order;
    }

    let mut results: Vec<_> = keyword_ids
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();
    downweight_foreign_env(
        &mut results,
        &persistence.retrieval_device_label(),
        persistence.foreign_env_weight(),
    );
    results.truncate(options.limit);
    Ok(results)
}

fn semantic_scan(
    persistence: &MemoryPersistence,
    query: &str,
    semantic: SemanticQuery<'_>,
) -> Result<Vec<VectorSearchResult>, SearchError> {
    match semantic {
        SemanticQuery::Configured => {
            let report = persistence.memory_vector_scan_diagnosed(query)?;
            Ok(if matches!(report.status, MemoryScanStatus::Scanned) {
                report.scored
            } else {
                Vec::new()
            })
        }
        SemanticQuery::Vector(vector) => {
            Ok(persistence.vector_scan_for_kind(vector, "memory")?.scored)
        }
        SemanticQuery::Disabled => Ok(Vec::new()),
    }
}

/// RRF damping constant from Cormack, Clarke & Büttcher (2009).
pub const RRF_K: f64 = 60.0;

/// Fuse ranked id lists by reciprocal rank. A single list is order-preserving;
/// equal scores break by id so results are deterministic.
#[must_use]
pub fn reciprocal_rank_fusion(ranked_lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut scores: HashMap<&str, f64> = HashMap::new();
    for list in ranked_lists {
        for (index, id) in list.iter().enumerate() {
            *scores.entry(id.as_str()).or_insert(0.0) += 1.0 / (k + index as f64 + 1.0);
        }
    }
    let mut fused: Vec<(String, f64)> = scores
        .into_iter()
        .map(|(id, score)| (id.to_string(), score))
        .collect();
    fused.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    fused
}

/// Apply RRF to the whole ranking while allowing only the leading `window`
/// keyword candidates to move. Returns both the bounded order and RRF scores.
#[must_use]
pub fn windowed_fusion_order(
    keyword_ids: &[String],
    dense_ids: &[String],
    cosines: &HashMap<String, f32>,
    window: usize,
) -> (Vec<String>, HashMap<String, f64>) {
    let fused = reciprocal_rank_fusion(&[keyword_ids.to_vec(), dense_ids.to_vec()], RRF_K);
    let scores: HashMap<String, f64> = fused.into_iter().collect();
    let mut order = keyword_ids.to_vec();
    let head = window.min(order.len());
    order[..head].sort_by(|left, right| {
        let left_score = scores.get(left).copied().unwrap_or(0.0);
        let right_score = scores.get(right).copied().unwrap_or(0.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let left_cosine = cosines.get(left).copied().unwrap_or(0.0);
                let right_cosine = cosines.get(right).copied().unwrap_or(0.0);
                right_cosine
                    .partial_cmp(&left_cosine)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.cmp(right))
    });
    (order, scores)
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
    let mut changed = false;
    for hit in hits.iter_mut() {
        if let Some(origin) = hit.memory.origin_device.as_deref() {
            if !origin.is_empty() && origin != current_device {
                hit.combined_score *= weight;
                changed = true;
            }
        }
    }
    if !changed {
        return;
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
                    // Vector search has no query terms to be rare in, so no
                    // subject is identified here; `false` is the reading that
                    // applies the relevance floor rather than exempting from it.
                    subject_matched: false,
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
                cosine: None,
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

        let mut options = super::HybridMemorySearchOptions::new(5);
        options.semantic = super::SemanticQuery::Disabled;
        assert!(hybrid_memory_search(&persistence, "anything", options)?.is_empty());
        Ok(())
    }

    #[test]
    fn rrf_is_window_bounded_and_uses_cosine_as_the_tiebreak() {
        use std::collections::HashMap;

        let keyword = ["a", "b", "c", "d"].map(str::to_string);
        let dense = ["b", "a", "d", "c"].map(str::to_string);
        let cosines = HashMap::from([
            ("a".to_string(), 0.2),
            ("b".to_string(), 0.9),
            ("c".to_string(), 0.3),
            ("d".to_string(), 0.8),
        ]);

        let (order, _) = super::windowed_fusion_order(&keyword, &dense, &cosines, 2);
        assert_eq!(order, ["b", "a", "c", "d"]);
    }

    #[test]
    fn rrf_of_one_list_preserves_its_order() {
        let keyword = ["a", "b", "c"].map(str::to_string).to_vec();
        let fused = super::reciprocal_rank_fusion(std::slice::from_ref(&keyword), super::RRF_K);
        assert_eq!(
            fused.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            keyword
        );
    }
}
