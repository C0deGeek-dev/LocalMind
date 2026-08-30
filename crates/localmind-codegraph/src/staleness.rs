//! Change-aware lesson invalidation.
//!
//! When code changes, the memories anchored to the changed (and dependent)
//! symbols may no longer hold. This joins the change-impact walk to the
//! memory↔code anchor edges to find the affected memories, and — above a
//! conservative relationship-strength threshold — flags each as a *staleness
//! candidate* and enqueues a review item. Nothing is auto-deleted or
//! auto-superseded: a flagged memory stays active and retrievable (just marked),
//! and a human decides whether to refresh, supersede, or keep it.

use crate::impact::{reachable_dependents, risk_for_hop, ChangedSpan, ImpactOptions, RiskTier};
use crate::CodeGraphError;
use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, GraphEndpoint, GraphNodeId,
    LessonCategory, LessonId, MemoryEntryId, SessionId, SuggestedAction,
};
use localmind_store::{GraphStore, MemoryPersistence, ReviewQueue};
use std::collections::BTreeMap;

/// A memory whose anchored code changed, with the strength of the join and the
/// risk tier of the closest changed symbol.
#[derive(Clone, Debug, PartialEq)]
pub struct AffectedMemory {
    pub memory_id: MemoryEntryId,
    /// The strongest anchor confidence linking this memory to a changed symbol.
    pub strength: f32,
    /// Risk of the closest changed symbol (hop distance from the edit).
    pub risk: RiskTier,
    pub hops: u32,
}

/// Knobs for change-aware invalidation. The threshold is deliberately
/// conservative by default so a weak (name-resolved, distant) link does not
/// flood the review queue.
#[derive(Clone, Copy, Debug)]
pub struct StalenessConfig {
    /// Minimum anchor strength for a memory to be flagged. Below this, the link
    /// is too weak to act on.
    pub min_strength: f32,
    pub impact: ImpactOptions,
}

impl Default for StalenessConfig {
    fn default() -> Self {
        Self {
            // A qualified-name anchor is 0.9 and a plain-name heuristic is 0.6;
            // 0.6 admits both but still rejects anything weaker.
            min_strength: 0.6,
            impact: ImpactOptions::default(),
        }
    }
}

/// What flagging produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StalenessReport {
    /// Memories newly flagged as staleness candidates.
    pub flagged: Vec<MemoryEntryId>,
    /// Review items enqueued for the flagged memories.
    pub enqueued: usize,
}

/// The memories affected by `spans`: those anchored to a changed symbol or to a
/// symbol that depends on one, within the impact bounds. Aggregated per memory
/// by strongest anchor and closest changed symbol, strongest first.
///
/// # Errors
/// Returns [`CodeGraphError`] when the graph cannot be read.
pub fn change_affected_memories(
    store: &GraphStore,
    spans: &[ChangedSpan],
    options: ImpactOptions,
) -> Result<Vec<AffectedMemory>, CodeGraphError> {
    let nodes = store.active_nodes()?;
    let visited = reachable_dependents(store, &nodes, spans, options)?;

    // memory id → (strongest anchor strength, closest changed-symbol hops).
    let mut best: BTreeMap<String, (f32, u32)> = BTreeMap::new();
    for (node_id, (hops, _heuristic)) in &visited {
        let graph_id = GraphNodeId::new(node_id.clone());
        for edge in store.memories_anchored_to(&graph_id)? {
            let GraphEndpoint::Memory(memory_id) = &edge.from else {
                continue;
            };
            let strength = edge.confidence.value();
            let entry = best
                .entry(memory_id.as_str().to_string())
                .or_insert((0.0, u32::MAX));
            if strength > entry.0 {
                entry.0 = strength;
            }
            if *hops < entry.1 {
                entry.1 = *hops;
            }
        }
    }

    let mut affected: Vec<AffectedMemory> = best
        .into_iter()
        .map(|(id, (strength, hops))| AffectedMemory {
            memory_id: MemoryEntryId::new(id),
            strength,
            risk: risk_for_hop(hops),
            hops,
        })
        .collect();
    affected.sort_by(|a, b| {
        b.strength
            .partial_cmp(&a.strength)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.memory_id.as_str().cmp(b.memory_id.as_str()))
    });
    Ok(affected)
}

/// Flag the affected memories above the strength threshold as staleness
/// candidates and enqueue one review item per newly-flagged memory. A memory
/// already flagged is not re-flagged or re-enqueued. Below-threshold memories
/// produce nothing. Never deletes or auto-supersedes.
///
/// # Errors
/// Returns [`CodeGraphError`] when the graph, memory index, or review queue
/// cannot be read or written.
pub fn flag_stale_candidates(
    store: &GraphStore,
    persistence: &MemoryPersistence,
    queue: &ReviewQueue,
    spans: &[ChangedSpan],
    config: &StalenessConfig,
) -> Result<StalenessReport, CodeGraphError> {
    let affected = change_affected_memories(store, spans, config.impact)?;
    let mut flagged = Vec::new();
    let mut candidates = Vec::new();
    for memory in affected {
        if memory.strength < config.min_strength {
            continue;
        }
        // mark_stale_candidate returns false when the memory is missing or
        // already flagged — only newly-flagged memories get a review item.
        if persistence.mark_stale_candidate(&memory.memory_id)? {
            candidates.push(staleness_candidate(&memory)?);
            flagged.push(memory.memory_id);
        }
    }
    let enqueued = if candidates.is_empty() {
        0
    } else {
        queue.enqueue_candidates(&SessionId::new("change-impact"), &candidates)?
    };
    Ok(StalenessReport { flagged, enqueued })
}

/// A review candidate that prompts a human to revisit a possibly-stale memory.
/// It carries the join strength as confidence and routes to review only
/// (`KeepForSession`) — promotion/supersession stays the reviewer's call.
fn staleness_candidate(memory: &AffectedMemory) -> Result<CandidateLesson, CodeGraphError> {
    let summary = format!(
        "Review possibly-stale memory {}: code it was anchored to changed (risk {:?}).",
        memory.memory_id.as_str(),
        memory.risk
    );
    let candidate = CandidateLesson::new(
        LessonId::new(format!("stale-{}", memory.memory_id.as_str())),
        summary,
        LessonCategory::Process,
        Confidence::new(memory.strength)?,
        SuggestedAction::KeepForSession,
    )
    .with_evidence(
        EvidenceRef::new(
            EvidenceKind::CodeParse,
            format!("change-impact on {}", memory.memory_id.as_str()),
        )
        .redacted(),
    );
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{anchor_memory, IngestBoundary, Ingester};
    use localmind_core::{
        Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId,
        MemoryScope, MemoryStatus,
    };
    use localmind_store::{MemoryPersistence, ReviewQueue};
    use std::fs;
    use std::path::Path;
    use time::OffsetDateTime;

    fn project() -> (tempfile::TempDir, GraphStore) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
        )
        .unwrap();
        let store = GraphStore::open_project(dir.path()).unwrap();
        (dir, store)
    }

    fn ingest(root: &Path, store: &GraphStore) {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/geometry.rs"),
            "pub fn norm(x: f64) -> f64 { x.abs() }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/audio.rs"),
            "pub fn play() -> bool { true }\n",
        )
        .unwrap();
        let boundary = IngestBoundary::new(root, Vec::new()).unwrap();
        Ingester::new()
            .unwrap()
            .ingest(
                &boundary,
                &[root.join("src/geometry.rs"), root.join("src/audio.rs")],
                store,
            )
            .unwrap();
    }

    fn seed_memory(root: &Path, store: &GraphStore, id: &str, hint: &str) {
        let memory = MemoryPersistence::open_project(root).unwrap();
        let entry = MemoryEntry {
            id: MemoryEntryId::new(id),
            scope: MemoryScope::Project,
            body: format!("a lesson about {hint}"),
            category: LessonCategory::CodePattern,
            confidence: Confidence::new(0.8).unwrap(),
            source_session: None,
            evidence: vec![EvidenceRef::new(EvidenceKind::ManualNote, "seeded")],
            tags: Vec::new(),
            related_files: Vec::new(),
            related_entities: vec![hint.to_string()],
            created_at: Some(OffsetDateTime::now_utc()),
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
            sync_meta: localmind_core::SyncMeta::default(),
        };
        memory.persist_memory_entry(&entry).unwrap();
        anchor_memory(store, &entry.id, &entry.related_entities).unwrap();
    }

    fn changed_norm() -> Vec<ChangedSpan> {
        vec![ChangedSpan {
            path: "src/geometry.rs".to_string(),
            line_start: 1,
            line_end: 1,
        }]
    }

    #[test]
    fn affected_set_includes_anchored_memory_and_excludes_unrelated() {
        let (dir, store) = project();
        ingest(dir.path(), &store);
        seed_memory(dir.path(), &store, "memory-norm", "src/geometry.rs::norm");
        seed_memory(dir.path(), &store, "memory-audio", "src/audio.rs::play");

        let affected =
            change_affected_memories(&store, &changed_norm(), ImpactOptions::default()).unwrap();
        let ids: Vec<&str> = affected.iter().map(|a| a.memory_id.as_str()).collect();
        assert!(
            ids.contains(&"memory-norm"),
            "anchored memory missing: {ids:?}"
        );
        assert!(
            !ids.contains(&"memory-audio"),
            "unrelated memory must not be affected: {ids:?}"
        );
    }

    #[test]
    fn above_threshold_flags_and_enqueues_below_threshold_does_not() {
        let (dir, store) = project();
        ingest(dir.path(), &store);
        seed_memory(dir.path(), &store, "memory-norm", "src/geometry.rs::norm");
        let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
        let queue = ReviewQueue::open_project(dir.path()).unwrap();

        // Threshold above the 0.9 qualified anchor → nothing flagged.
        let strict = StalenessConfig {
            min_strength: 0.95,
            ..StalenessConfig::default()
        };
        let none =
            flag_stale_candidates(&store, &persistence, &queue, &changed_norm(), &strict).unwrap();
        assert!(none.flagged.is_empty(), "weak-link guard failed: {none:?}");
        assert_eq!(none.enqueued, 0);

        // Conservative default threshold → the anchored memory is flagged once.
        let report = flag_stale_candidates(
            &store,
            &persistence,
            &queue,
            &changed_norm(),
            &StalenessConfig::default(),
        )
        .unwrap();
        assert_eq!(report.flagged, vec![MemoryEntryId::new("memory-norm")]);
        assert_eq!(report.enqueued, 1);
        assert_eq!(
            persistence.list_stale_candidates().unwrap(),
            vec![MemoryEntryId::new("memory-norm")]
        );

        // Re-running does not double-flag or double-enqueue.
        let again = flag_stale_candidates(
            &store,
            &persistence,
            &queue,
            &changed_norm(),
            &StalenessConfig::default(),
        )
        .unwrap();
        assert!(again.flagged.is_empty());
        assert_eq!(again.enqueued, 0);
    }

    #[test]
    fn a_flagged_memory_is_marked_in_ranked_search_results_not_dropped() {
        let (dir, store) = project();
        ingest(dir.path(), &store);
        seed_memory(dir.path(), &store, "memory-norm", "src/geometry.rs::norm");
        let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        flag_stale_candidates(
            &store,
            &persistence,
            &queue,
            &changed_norm(),
            &StalenessConfig::default(),
        )
        .unwrap();

        let results = persistence.search("norm").unwrap();
        let hit = results
            .iter()
            .find(|r| r.memory_id.as_str() == "memory-norm")
            .expect("stale memory must still be served, not dropped");
        assert!(
            hit.stale_candidate,
            "result must be marked stale, not silent"
        );
    }
}
