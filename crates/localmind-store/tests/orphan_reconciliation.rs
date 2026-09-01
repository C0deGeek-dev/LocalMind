//! End-to-end: a memory file written but never indexed (the crash window
//! between subject 01's atomic file write and the SQLite indexing
//! transaction) is found and repaired by the reconciliation sweep — unless
//! its id already carries a retirement event, in which case it is reported
//! for manual review and never reindexed, even under apply.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId, SyncMeta,
};
use localmind_store::{MemoryPathResolver, MemoryPersistence, ProjectConfig};

fn enabled_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )
    .unwrap();
    dir
}

fn seed_memory(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: body.to_string(),
        category: LessonCategory::ProjectConvention,
        confidence: Confidence::new(0.9).unwrap(),
        source_session: Some(SessionId::new("seed")),
        evidence: vec![EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted()],
        tags: vec!["accepted".to_string()],
        related_files: Vec::new(),
        related_entities: Vec::new(),
        created_at: None,
        updated_at: None,
        supersedes: Vec::new(),
        contradicts: Vec::new(),
        status: MemoryStatus::Active,
        sync_meta: SyncMeta::default(),
    }
}

/// Writes a memory's Markdown file directly through the same path
/// `persist_memory_entry`/`promote_review_item` use — but skips the
/// indexing transaction entirely, reproducing exactly the gap a crash
/// between the two would leave: a fully-written, permanently un-indexed
/// file.
fn write_unindexed(root: &std::path::Path, entry: &MemoryEntry) {
    let config = ProjectConfig::discover(root).unwrap();
    MemoryPathResolver::write_memory_file(&config, entry).unwrap();
}

#[test]
fn an_unindexed_but_fully_written_file_is_found_and_reindexed_under_apply() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    write_unindexed(
        root,
        &seed_memory("orphan1", "prefer rebasing over merge commits here"),
    );

    // Invisible to the store until reconciled: no index row means no
    // search hit and no listing entry, even though the file is fully on
    // disk (this is not the crash-injection scenario itself — subject 01
    // already proved the file write side is atomic — it is the *next* gap,
    // between a fully-written file and its indexing transaction).
    assert!(persistence.search("rebasing").unwrap().is_empty());
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "orphan1"));

    // Dry-run finds it, does nothing.
    let plan = persistence.orphan_sweep_plan().unwrap();
    assert_eq!(plan.reindexable.len(), 1);
    assert_eq!(plan.reindexable[0].memory_id, "orphan1");
    assert!(plan.flagged_for_review.is_empty());
    assert!(persistence.search("rebasing").unwrap().is_empty());

    // Apply reindexes it.
    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 1);
    assert_eq!(report.found.reindexable.len(), 1);

    // Now visible and searchable, exactly like a normally-promoted memory.
    let hits = persistence.search("rebasing").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory_id.as_str(), "orphan1");

    // An OrphanReconciled audit row records the recovery.
    let audit = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .find(|row| row.kind == "OrphanReconciled")
        .expect("an OrphanReconciled audit row");
    assert_eq!(audit.subject, "orphan1");
    assert!(audit.metadata_json.contains("orphan1.md"));

    // Idempotent: nothing left to find on a second sweep.
    let second_plan = persistence.orphan_sweep_plan().unwrap();
    assert!(second_plan.is_empty());
}

#[test]
fn a_file_whose_id_carries_a_retirement_event_is_flagged_but_never_reindexed() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    write_unindexed(
        root,
        &seed_memory("retired1", "this lesson was already superseded elsewhere"),
    );

    // Simulate a retirement event on record for this id without a
    // memory_index row — the belt-and-suspenders case the plan's own risk
    // table names (no code path in this crate produces it today, since
    // supersede/delete never actually remove or skip the index row; this
    // proves the sweep still refuses to resurrect it if that ever changes).
    let db_path = root.join(".localmind").join("localmind.sqlite");
    let raw = rusqlite::Connection::open(&db_path).unwrap();
    raw.execute(
        "INSERT INTO audit_events(kind, actor, subject, metadata_json, happened_at) \
         VALUES('MemorySuperseded', 'tester', 'retired1', '{}', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(raw);

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert!(plan.reindexable.is_empty());
    assert_eq!(plan.flagged_for_review.len(), 1);
    assert_eq!(plan.flagged_for_review[0].memory_id, "retired1");

    // Apply must not touch it either.
    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 0);
    assert_eq!(report.found.flagged_for_review.len(), 1);
    assert!(persistence
        .search("superseded elsewhere")
        .unwrap()
        .is_empty());
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "retired1"));
    assert!(persistence
        .audit_records()
        .unwrap()
        .iter()
        .all(|row| row.kind != "OrphanReconciled"));
}
