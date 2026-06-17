//! The primer flows through the review queue like any other candidate, and a
//! re-distilled primer supersedes the prior accepted one.

use localmind_codegraph::{
    distill_primer, ArchitectureOverview, LanguageStat, PackageStat, SymbolStat,
};
use localmind_core::{MemoryEntryId, ReviewAction, ReviewDecision, ReviewItemId, SessionId};
use localmind_store::{MemoryPersistence, ReviewQueue};
use std::fs;
use std::path::Path;

fn project(root: &Path) {
    if fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n").is_err() {
        unreachable!("config write must succeed");
    }
}

fn overview(file_count: usize, hub_callers: usize) -> ArchitectureOverview {
    ArchitectureOverview {
        file_count,
        symbol_count: file_count * 2,
        languages: vec![LanguageStat {
            language: "rust".to_string(),
            file_count,
        }],
        top_packages: vec![PackageStat {
            path: "src".to_string(),
            file_count,
        }],
        entry_points: vec![SymbolStat {
            qualified_name: "src/x.rs::run".to_string(),
            kind: "function".to_string(),
            in_degree: 0,
            out_degree: 2,
        }],
        hotspots: vec![SymbolStat {
            qualified_name: "src/x.rs::hub".to_string(),
            kind: "function".to_string(),
            in_degree: hub_callers,
            out_degree: 0,
        }],
    }
}

#[test]
fn primer_is_enqueued_for_review_not_accepted_directly() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    project(dir.path());

    let primer = distill_primer(&overview(3, 2), "demo", "abc123")?;
    let queue = ReviewQueue::open_project(dir.path())?;
    let session = SessionId::new("session-1");
    queue.enqueue_candidates(&session, std::slice::from_ref(&primer))?;

    let summary = queue.summary()?;
    assert_eq!(
        summary.pending, 1,
        "primer must be a pending review candidate"
    );
    assert_eq!(summary.accepted, 0, "nothing is accepted without review");

    // No accepted memory exists until the candidate is reviewed and promoted.
    let memory = MemoryPersistence::open_project(dir.path())?;
    assert!(
        memory.list_memory()?.is_empty(),
        "distillation must not write accepted memory directly"
    );
    Ok(())
}

#[test]
fn re_distillation_supersedes_the_prior_primer() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    project(dir.path());
    let queue = ReviewQueue::open_project(dir.path())?;
    let memory = MemoryPersistence::open_project(dir.path())?;
    let session = SessionId::new("session-1");

    // First primer: enqueue, accept, promote.
    let first = distill_primer(&overview(3, 2), "demo", "abc123")?;
    queue.enqueue_candidates(&session, std::slice::from_ref(&first))?;
    let first_item = ReviewItemId::new(first.id.as_str());
    queue.decide(ReviewDecision {
        item_id: first_item.clone(),
        action: ReviewAction::Accept,
        reviewer: "tester".to_string(),
        decided_at: None,
        note: None,
        replacement_summary: None,
        evidence: Vec::new(),
    })?;
    let first_entry = memory.promote_review_item(&first_item)?;

    // Repo drifts → a new primer with a distinct id; accept it as the supersede
    // of the first (the shipped D-LM-0008 path).
    let second = distill_primer(&overview(5, 4), "demo", "def456")?;
    assert_ne!(
        second.id, first.id,
        "a drifted repo yields a distinct primer"
    );
    queue.enqueue_candidates(&session, std::slice::from_ref(&second))?;
    let second_item = ReviewItemId::new(second.id.as_str());
    queue.decide(ReviewDecision {
        item_id: second_item.clone(),
        action: ReviewAction::Supersede(MemoryEntryId::new(first_entry.id.as_str())),
        reviewer: "tester".to_string(),
        decided_at: None,
        note: None,
        replacement_summary: None,
        evidence: Vec::new(),
    })?;
    let second_entry = memory.promote_review_item(&second_item)?;

    assert!(
        second_entry
            .supersedes
            .iter()
            .any(|id| id == &first_entry.id),
        "the new primer must record the prior one in supersedes"
    );

    // `list_memory` returns only active memory: the prior primer is retired
    // (absent), the new one is active.
    let active: Vec<String> = memory
        .list_memory()?
        .into_iter()
        .map(|record| record.memory_id.as_str().to_string())
        .collect();
    assert!(
        !active.contains(&first_entry.id.as_str().to_string()),
        "the superseded primer must no longer be active"
    );
    assert!(
        active.contains(&second_entry.id.as_str().to_string()),
        "the new primer must be active"
    );
    Ok(())
}
