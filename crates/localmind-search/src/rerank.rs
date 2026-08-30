//! Optional embedding rerank — an additive stage after the deterministic blend.
//!
//! The deterministic blend (`workspace::search_workspace`) is always the floor:
//! reproducible, offline, byte-stable. This stage *optionally* reorders the top
//! of that list by cosine similarity between the query embedding and each hit's
//! embedding, so a semantically-closer hit can climb above a lexically-stronger
//! one. It is strictly additive: with the flag off, or with no embedder, it
//! returns the blend order unchanged.

use crate::workspace::{RankedHit, SearchHitKind};
use thiserror::Error;

/// A source of embeddings for rerank. The local inference endpoint implements
/// this in the host wiring; tests pass a deterministic stub. Each returned
/// vector lines up with the corresponding input, in order.
pub trait RerankEmbedder {
    fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, RerankError>;
}

#[derive(Debug, Error)]
pub enum RerankError {
    #[error("rerank embedder failed: {detail}")]
    Embedder { detail: String },
    #[error("rerank embedder returned {got} vectors for {expected} inputs")]
    VectorCount { expected: usize, got: usize },
}

/// How the rerank stage runs for one search.
pub struct RerankOptions<'a> {
    /// Master flag. Off → identity (the determinism floor).
    pub enabled: bool,
    /// Embedding source. `None` → identity even when `enabled`.
    pub embedder: Option<&'a dyn RerankEmbedder>,
    /// Rerank only the top `window` blended hits; the tail keeps blend order.
    pub window: usize,
}

impl Default for RerankOptions<'_> {
    fn default() -> Self {
        Self {
            enabled: false,
            embedder: None,
            window: 20,
        }
    }
}

/// Reorder the top-`window` hits by cosine similarity between the query
/// embedding and each hit's embedding, preserving the blend order as the
/// tie-breaker and for the untouched tail. Disabled, missing an embedder, or
/// fewer than two reorderable hits → returns `hits` unchanged (the determinism
/// floor); the legacy ordering is therefore byte-identical when off.
pub fn rerank_hits(
    hits: Vec<RankedHit>,
    query: &str,
    options: &RerankOptions<'_>,
) -> Result<Vec<RankedHit>, RerankError> {
    if !options.enabled {
        return Ok(hits);
    }
    let Some(embedder) = options.embedder else {
        return Ok(hits);
    };
    if hits.len() < 2 || options.window < 2 {
        return Ok(hits);
    }
    let window = options.window.min(hits.len());

    // Inputs: the query first, then each window hit's representative text.
    let mut inputs = Vec::with_capacity(window + 1);
    inputs.push(query.to_string());
    inputs.extend(hits[..window].iter().map(hit_text));

    let vectors = embedder.embed_batch(&inputs)?;
    if vectors.len() != inputs.len() {
        return Err(RerankError::VectorCount {
            expected: inputs.len(),
            got: vectors.len(),
        });
    }
    let query_vector = &vectors[0];

    // Score each window hit by cosine(query, hit); keep its original index so
    // equal scores fall back to the deterministic blend order.
    let mut scored: Vec<(usize, f32)> = (0..window)
        .map(|index| (index, cosine_similarity(query_vector, &vectors[index + 1])))
        .collect();
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });

    // Rebuild: reranked window first, then the untouched tail.
    let mut window_hits: Vec<Option<RankedHit>> = hits.into_iter().map(Some).collect();
    let tail = window_hits.split_off(window);
    let mut reordered = Vec::with_capacity(window_hits.len() + tail.len());
    for (index, _score) in scored {
        if let Some(hit) = window_hits[index].take() {
            reordered.push(hit);
        }
    }
    reordered.extend(tail.into_iter().flatten());
    Ok(reordered)
}

/// The text embedded to represent a hit: a code node's qualified name plus its
/// skeleton, or a memory's snippet.
fn hit_text(hit: &RankedHit) -> String {
    match &hit.kind {
        SearchHitKind::Code(node) => {
            let skeleton = node.skeleton.as_deref().unwrap_or("");
            format!("{} {}", node.qualified_name, skeleton)
                .trim()
                .to_string()
        }
        SearchHitKind::Memory { snippet, .. } => snippet.clone(),
    }
}

/// Rerank an already-ranked hit list by **precomputed** query-to-hit cosine
/// scores — the stored-vector sibling of [`rerank_hits`] for a host that has
/// already scored its candidates against the `vector_index` (no re-embedding
/// of hit texts). Same contract as [`rerank_hits`]: only the top `window`
/// hits may move, the tail keeps its order, and `window < 2` or fewer than
/// two hits is the identity. A hit without a cosine keeps its exact slot
/// (unknown relevance never demotes a hit below where the deterministic
/// blend put it — the conservative direction); the scored hits redistribute
/// among the scored slots by cosine, original order breaking ties.
pub fn rerank_scored<T>(
    mut hits: Vec<T>,
    window: usize,
    cosine_of: impl Fn(&T) -> Option<f32>,
) -> Vec<T> {
    if hits.len() < 2 || window < 2 {
        return hits;
    }
    let window = window.min(hits.len());
    let mut scored: Vec<(usize, f32)> = hits[..window]
        .iter()
        .enumerate()
        .filter_map(|(index, hit)| cosine_of(hit).map(|cosine| (index, cosine)))
        .collect();
    if scored.len() < 2 {
        return hits;
    }
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    // Redistribute the scored hits over the same slots, best cosine first;
    // unscored hits (and the tail) never move.
    let mut taken: Vec<Option<T>> = hits.drain(..).map(Some).collect();
    let mut picked = std::collections::VecDeque::with_capacity(scored.len());
    for (index, _) in scored {
        if let Some(hit) = taken[index].take() {
            picked.push_back(hit);
        }
    }
    let mut reordered: Vec<T> = Vec::with_capacity(taken.len());
    for slot in taken {
        match slot {
            Some(hit) => reordered.push(hit),
            None => {
                if let Some(hit) = picked.pop_front() {
                    reordered.push(hit);
                }
            }
        }
    }
    reordered
}

/// Cosine similarity in `[-1, 1]`; `0.0` for mismatched, empty, or zero vectors.
fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut left_norm = 0.0_f32;
    let mut right_norm = 0.0_f32;
    for (l, r) in left.iter().zip(right.iter()) {
        dot += l * r;
        left_norm += l * l;
        right_norm += r * r;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

#[cfg(test)]
mod tests {
    use super::{rerank_hits, RerankEmbedder, RerankError, RerankOptions};
    use crate::workspace::{RankedHit, SearchHitKind};
    use localmind_core::MemoryEntryId;

    /// Deterministic stub: the query and any snippet containing `match` embed to
    /// `[1, 0]`; everything else to `[0, 1]`. So a hit whose snippet says
    /// `match` is maximally similar to the query and should climb to the top.
    struct MarkerEmbedder;

    impl RerankEmbedder for MarkerEmbedder {
        fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, RerankError> {
            Ok(inputs
                .iter()
                .map(|text| {
                    if text == "query" || text.contains("match") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
    }

    fn memory_hit(id: &str, snippet: &str, score: f32) -> RankedHit {
        RankedHit {
            kind: SearchHitKind::Memory {
                id: MemoryEntryId::new(id),
                snippet: snippet.to_string(),
            },
            score,
            structural: score,
            temporal: 0.0,
            intent: 0.0,
        }
    }

    fn ids(hits: &[RankedHit]) -> Vec<String> {
        hits.iter()
            .map(|hit| match &hit.kind {
                SearchHitKind::Memory { id, .. } => id.as_str().to_string(),
                SearchHitKind::Code(node) => node.id.as_str().to_string(),
            })
            .collect()
    }

    fn fixture() -> Vec<RankedHit> {
        // Blend order a > b > c; the semantic match sits last.
        vec![
            memory_hit("a", "alpha note", 0.9),
            memory_hit("b", "beta note", 0.6),
            memory_hit("c", "gamma match note", 0.3),
        ]
    }

    #[test]
    fn disabled_rerank_is_byte_identical_to_the_blend() -> Result<(), RerankError> {
        let hits = fixture();
        let before = ids(&hits);
        let options = RerankOptions {
            enabled: false,
            embedder: Some(&MarkerEmbedder),
            window: 10,
        };
        let after = rerank_hits(hits, "query", &options)?;
        assert_eq!(before, ids(&after));
        Ok(())
    }

    #[test]
    fn no_embedder_is_a_no_op_even_when_enabled() -> Result<(), RerankError> {
        let hits = fixture();
        let before = ids(&hits);
        let options = RerankOptions {
            enabled: true,
            embedder: None,
            window: 10,
        };
        let after = rerank_hits(hits, "query", &options)?;
        assert_eq!(before, ids(&after));
        Ok(())
    }

    #[test]
    fn a_stub_embedder_reorders_the_window() -> Result<(), RerankError> {
        let hits = fixture();
        let options = RerankOptions {
            enabled: true,
            embedder: Some(&MarkerEmbedder),
            window: 10,
        };
        let after = rerank_hits(hits, "query", &options)?;
        // The semantic match climbs from last to first; the rest keep blend order.
        assert_eq!(ids(&after), vec!["c", "a", "b"]);
        Ok(())
    }

    #[test]
    fn the_tail_outside_the_window_keeps_blend_order() -> Result<(), RerankError> {
        let hits = fixture();
        let options = RerankOptions {
            enabled: true,
            embedder: Some(&MarkerEmbedder),
            // Only the first two hits are reorderable; the matching hit is in the tail.
            window: 2,
        };
        let after = rerank_hits(hits, "query", &options)?;
        // Window {a,b} both score 0 → keep order; c stays in the tail.
        assert_eq!(ids(&after), vec!["a", "b", "c"]);
        Ok(())
    }

    #[test]
    fn precomputed_scores_reorder_only_the_scored_slots() {
        // ("id", Option<cosine>) — b has no stored vector and must keep its
        // exact slot; a and c redistribute over slots 0 and 2 by cosine.
        let hits = vec![("a", Some(0.1_f32)), ("b", None), ("c", Some(0.9))];
        let after = super::rerank_scored(hits, 10, |hit| hit.1);
        let order: Vec<&str> = after.iter().map(|hit| hit.0).collect();
        assert_eq!(order, vec!["c", "b", "a"]);
    }

    #[test]
    fn precomputed_scores_respect_the_window() {
        // The best cosine sits outside the window and must not move.
        let hits = vec![("a", Some(0.2_f32)), ("b", Some(0.5)), ("c", Some(0.9))];
        let after = super::rerank_scored(hits, 2, |hit| hit.1);
        let order: Vec<&str> = after.iter().map(|hit| hit.0).collect();
        assert_eq!(order, vec!["b", "a", "c"]);
    }

    #[test]
    fn precomputed_scores_are_the_identity_when_there_is_nothing_to_rerank() {
        // Fewer than two scored hits, a sub-2 window, or a short list — all
        // identity (the determinism floor for the stored-vector path).
        let single_scored = vec![("a", Some(0.9_f32)), ("b", None), ("c", None)];
        let after = super::rerank_scored(single_scored, 10, |hit| hit.1);
        assert_eq!(
            after.iter().map(|h| h.0).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );

        let tiny_window = vec![("a", Some(0.1_f32)), ("b", Some(0.9))];
        let after = super::rerank_scored(tiny_window, 1, |hit| hit.1);
        assert_eq!(
            after.iter().map(|h| h.0).collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        let short: Vec<(&str, Option<f32>)> = vec![("a", Some(0.1))];
        let after = super::rerank_scored(short, 10, |hit| hit.1);
        assert_eq!(after.len(), 1);
    }
}
