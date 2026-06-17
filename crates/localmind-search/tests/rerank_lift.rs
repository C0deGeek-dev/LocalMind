//! Before/after relevance check for the optional rerank stage.
//!
//! Loads a small committed fixture of queries whose truly-relevant memory sits
//! below the top-k cut in deterministic-blend order, measures hit-rate@k with
//! and without rerank, and pins that rerank lifts it without regressing. The
//! fixture and this measurement are reused by the memory-strategy eval, which
//! formalizes the harness.

#![allow(clippy::unwrap_used)]

use localmind_core::MemoryEntryId;
use localmind_search::{rerank_hits, RankedHit, RerankEmbedder, RerankError, RerankOptions};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Fixture {
    k: usize,
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Query {
    query: String,
    query_vector: Vec<f32>,
    candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Candidate {
    id: String,
    snippet: String,
    vector: Vec<f32>,
    relevant: bool,
}

/// Returns the fixture vector for each input text (query or snippet); panics if
/// the harness ever asks for text not in the fixture, which would be a bug.
struct FixtureEmbedder {
    by_text: HashMap<String, Vec<f32>>,
}

impl RerankEmbedder for FixtureEmbedder {
    fn embed_batch(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, RerankError> {
        inputs
            .iter()
            .map(|text| {
                self.by_text
                    .get(text)
                    .cloned()
                    .ok_or_else(|| RerankError::Embedder {
                        detail: format!("no fixture vector for {text:?}"),
                    })
            })
            .collect()
    }
}

fn blend_hits(query: &Query) -> Vec<RankedHit> {
    // Candidates are listed in blend order; synthesize descending blend scores.
    query
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| RankedHit {
            kind: localmind_search::SearchHitKind::Memory {
                id: MemoryEntryId::new(&candidate.id),
                snippet: candidate.snippet.clone(),
            },
            score: 1.0 - index as f32 * 0.01,
            structural: 0.0,
            temporal: 0.0,
            intent: 0.0,
        })
        .collect()
}

fn top_k_has_relevant(hits: &[RankedHit], k: usize, relevant: &[&str]) -> bool {
    hits.iter().take(k).any(|hit| match &hit.kind {
        localmind_search::SearchHitKind::Memory { id, .. } => relevant.contains(&id.as_str()),
        localmind_search::SearchHitKind::Code(_) => false,
    })
}

#[test]
fn rerank_lifts_hit_rate_without_regressing() -> Result<(), Box<dyn std::error::Error>> {
    let fixture: Fixture = serde_json::from_str(include_str!("fixtures/retrieval-rerank.json"))?;
    let k = fixture.k;

    let mut baseline_hits = 0_usize;
    let mut reranked_hits = 0_usize;

    for query in &fixture.queries {
        let relevant: Vec<&str> = query
            .candidates
            .iter()
            .filter(|candidate| candidate.relevant)
            .map(|candidate| candidate.id.as_str())
            .collect();

        let mut by_text = HashMap::new();
        by_text.insert(query.query.clone(), query.query_vector.clone());
        for candidate in &query.candidates {
            by_text.insert(candidate.snippet.clone(), candidate.vector.clone());
        }
        let embedder = FixtureEmbedder { by_text };

        let blend = blend_hits(query);
        if top_k_has_relevant(&blend, k, &relevant) {
            baseline_hits += 1;
        }

        let options = RerankOptions {
            enabled: true,
            embedder: Some(&embedder),
            window: query.candidates.len(),
        };
        let reranked = rerank_hits(blend, &query.query, &options)?;
        if top_k_has_relevant(&reranked, k, &relevant) {
            reranked_hits += 1;
        }
    }

    let total = fixture.queries.len();
    let baseline_rate = baseline_hits as f32 / total as f32;
    let reranked_rate = reranked_hits as f32 / total as f32;
    // Recorded as the rerank relevance check and reused by the eval harness.
    println!(
        "hit-rate@{k}: baseline {baseline_rate:.2} ({baseline_hits}/{total}), \
         rerank {reranked_rate:.2} ({reranked_hits}/{total})"
    );

    assert!(
        reranked_rate >= baseline_rate,
        "rerank must never regress hit-rate"
    );
    assert!(
        reranked_rate > baseline_rate,
        "this fixture is built to show lift: {reranked_rate} vs {baseline_rate}"
    );
    assert_eq!(
        baseline_hits, 0,
        "fixture places relevant items below the cut"
    );
    assert_eq!(
        reranked_hits, total,
        "rerank should surface every relevant item"
    );
    Ok(())
}
