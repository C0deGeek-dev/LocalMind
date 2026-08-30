//! One search over code and learned knowledge together.
//!
//! Candidates come from two directions and meet in one ranked list: graph
//! nodes matched by query terms (or sitting in the focus node's
//! neighborhood), and accepted memory matched by keyword search **or pulled
//! in through its anchor edges** — which is what makes a module-scoped query
//! surface the lessons learned about that module even when their text never
//! mentions it.

use crate::rank::{combined_score, proximity_score, temporal_score, RankingConfig};
use crate::SearchError;
use localmind_core::{GraphNode, GraphNodeId, MemoryEntryId, NodeKind};
use localmind_store::{GraphStore, MemoryPersistence};
use std::collections::BTreeMap;
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub enum SearchHitKind {
    /// Boxed: a graph node is an order of magnitude larger than the memory
    /// variant, and hit lists move by value through ranking and sorting.
    Code(Box<GraphNode>),
    Memory {
        id: MemoryEntryId,
        snippet: String,
    },
}

/// One ranked result with its signal breakdown, so a caller (and the user)
/// can see *why* something surfaced.
#[derive(Clone, Debug)]
pub struct RankedHit {
    pub kind: SearchHitKind,
    pub score: f32,
    pub structural: f32,
    pub temporal: f32,
    pub intent: f32,
}

#[derive(Clone, Debug)]
pub struct WorkspaceQuery {
    pub text: String,
    /// When set, results are boosted by graph proximity to this node.
    pub focus: Option<GraphNodeId>,
}

const SEARCHED_KINDS: [NodeKind; 6] = [
    NodeKind::File,
    NodeKind::Module,
    NodeKind::Type,
    NodeKind::Function,
    NodeKind::Test,
    NodeKind::Dependency,
];

/// Searches code structure and accepted memory behind one API, ranked by the
/// weighted structural/temporal/intent blend. Deterministic and offline.
pub fn search_workspace(
    graph: &GraphStore,
    memory: &MemoryPersistence,
    query: &WorkspaceQuery,
    config: &RankingConfig,
) -> Result<Vec<RankedHit>, SearchError> {
    let now = OffsetDateTime::now_utc();
    let terms: Vec<String> = query
        .text
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .filter(|term| !term.is_empty())
        .collect();

    let hops = focus_distances(graph, query.focus.as_ref(), config.max_hops)?;

    let mut hits = Vec::new();
    let mut anchored_neighborhood: Vec<(MemoryEntryId, f32)> = Vec::new();

    for kind in SEARCHED_KINDS {
        for node in graph.nodes_by_kind(kind)? {
            let intent = term_match(&terms, &node);
            let structural = match query.focus {
                Some(_) => {
                    proximity_score(hops.get(node.id.as_str()).copied(), config.neighbor_falloff)
                }
                None => node.confidence.value(),
            };
            // Scope: a node is in play when the query text matches it, or
            // when it sits in the focus neighborhood. Without a focus,
            // structural alone must not pull in unrelated nodes.
            let in_scope = intent > 0.0 || (query.focus.is_some() && structural > 0.0);
            if !in_scope {
                continue;
            }

            // Anything in scope drags its anchored knowledge into scope too.
            for anchor in graph.memories_anchored_to(&node.id)? {
                if let localmind_core::GraphEndpoint::Memory(memory_id) = &anchor.from {
                    let strength = anchor.confidence.value()
                        * if query.focus.is_some() {
                            proximity_score(
                                hops.get(node.id.as_str()).copied(),
                                config.neighbor_falloff,
                            )
                        } else {
                            1.0
                        };
                    anchored_neighborhood.push((memory_id.clone(), strength));
                }
            }

            let temporal = temporal_score(age_days(node.created_at, now), config.half_life_days);
            let score = combined_score(structural, temporal, intent, config.weights);
            if score > 0.0 {
                hits.push(RankedHit {
                    kind: SearchHitKind::Code(Box::new(node)),
                    score,
                    structural,
                    temporal,
                    intent,
                });
            }
        }
    }

    let mut memory_hits: BTreeMap<String, RankedHit> = BTreeMap::new();
    for result in memory.search(&query.text)? {
        let intent = 1.0 - 1.0 / (1.0 + result.score as f32);
        let structural = anchor_strength(&anchored_neighborhood, &result.memory_id);
        let temporal = temporal_score(
            age_days_from_text(&result.created_at, now),
            config.half_life_days,
        );
        let score = combined_score(structural, temporal, intent, config.weights);
        if score > 0.0 {
            memory_hits.insert(
                result.memory_id.as_str().to_string(),
                RankedHit {
                    kind: SearchHitKind::Memory {
                        id: result.memory_id,
                        snippet: result.snippet,
                    },
                    score,
                    structural,
                    temporal,
                    intent,
                },
            );
        }
    }

    // Memory pulled in purely through the graph: anchored to an in-scope code
    // node but missed by the keyword pass.
    for record in memory.list_memory()? {
        if memory_hits.contains_key(record.memory_id.as_str()) {
            continue;
        }
        let structural = anchor_strength(&anchored_neighborhood, &record.memory_id);
        if structural <= 0.0 {
            continue;
        }
        let score = combined_score(structural, 0.0, 0.0, config.weights);
        if score > 0.0 {
            memory_hits.insert(
                record.memory_id.as_str().to_string(),
                RankedHit {
                    kind: SearchHitKind::Memory {
                        id: record.memory_id,
                        snippet: record.body.chars().take(160).collect(),
                    },
                    score,
                    structural,
                    temporal: 0.0,
                    intent: 0.0,
                },
            );
        }
    }
    hits.extend(memory_hits.into_values());

    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| hit_id(left).cmp(hit_id(right)))
    });
    Ok(hits)
}

/// Hop distances from the focus node, computed by widening depth-bounded
/// neighborhood queries (the focus itself is hop 0).
fn focus_distances(
    graph: &GraphStore,
    focus: Option<&GraphNodeId>,
    max_hops: u32,
) -> Result<BTreeMap<String, u32>, SearchError> {
    let mut hops = BTreeMap::new();
    let Some(focus) = focus else {
        return Ok(hops);
    };
    hops.insert(focus.as_str().to_string(), 0);
    for depth in 1..=max_hops {
        for node in graph.neighbors(focus, depth)? {
            hops.entry(node.id.as_str().to_string()).or_insert(depth);
        }
    }
    Ok(hops)
}

/// Fraction of query terms found in the node's name, qualified name, or
/// skeleton.
fn term_match(terms: &[String], node: &GraphNode) -> f32 {
    if terms.is_empty() {
        return 0.0;
    }
    let haystack = format!(
        "{} {} {}",
        node.name.to_ascii_lowercase(),
        node.qualified_name.to_ascii_lowercase(),
        node.skeleton.as_deref().unwrap_or("").to_ascii_lowercase()
    );
    let matched = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    matched as f32 / terms.len() as f32
}

fn anchor_strength(anchors: &[(MemoryEntryId, f32)], memory_id: &MemoryEntryId) -> f32 {
    anchors
        .iter()
        .filter(|(id, _)| id == memory_id)
        .map(|(_, strength)| *strength)
        .fold(0.0, f32::max)
}

fn age_days(created_at: Option<OffsetDateTime>, now: OffsetDateTime) -> Option<f32> {
    created_at.map(|stamp| ((now - stamp).whole_seconds() as f32 / 86_400.0).max(0.0))
}

/// Ages a stored `created_at` text by its `YYYY-MM-DD` prefix — enough for
/// day-granularity recency without depending on the full stored format.
fn age_days_from_text(created_at: &str, now: OffsetDateTime) -> Option<f32> {
    let prefix = created_at.get(0..10)?;
    let mut parts = prefix.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    let month = time::Month::try_from(month).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let days = (now.date() - date).whole_days() as f32;
    Some(days.max(0.0))
}

fn hit_id(hit: &RankedHit) -> &str {
    match &hit.kind {
        SearchHitKind::Code(node) => node.id.as_str(),
        SearchHitKind::Memory { id, .. } => id.as_str(),
    }
}
