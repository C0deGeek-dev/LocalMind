//! End-to-end: a memory file written but never indexed (a fully-written file
//! with no matching `memory_index` row) is found and repaired by the
//! reconciliation sweep — unless its id already carries a retirement event,
//! or its front matter disagrees with its own filename/directory, in which
//! case it is reported for manual review and never reindexed, even under
//! apply.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId, SyncMeta,
};
use localmind_store::{MarkdownMemoryFormat, MemoryPathResolver, MemoryPersistence, ProjectConfig};

fn enabled_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\n",
    )
    .unwrap();
    dir
}

fn seed_memory(id: &str, scope: MemoryScope, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope,
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
/// indexing transaction entirely, reproducing exactly the gap an
/// interruption between the two would leave: a fully-written, permanently
/// un-indexed file.
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
        &seed_memory(
            "orphan1",
            MemoryScope::Project,
            "prefer rebasing over merge commits here",
        ),
    );

    // Invisible to the store until reconciled: no index row means no
    // search hit and no listing entry, even though the file is fully on
    // disk.
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
    assert!(report.stale.is_empty());

    // Now visible and searchable, exactly like a normally-promoted memory.
    let hits = persistence.search("rebasing").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory_id.as_str(), "orphan1");

    // An OrphanReconciled audit row records the recovery, tagged with a
    // sweep-run identifier.
    let audit = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .find(|row| row.kind == "OrphanReconciled")
        .expect("an OrphanReconciled audit row");
    assert_eq!(audit.subject, "orphan1");
    assert!(audit.metadata_json.contains("orphan1.md"));
    assert!(audit.metadata_json.contains("\"sweep_run\""));

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
        &seed_memory(
            "retired1",
            MemoryScope::Project,
            "this lesson was already superseded elsewhere",
        ),
    );

    // Simulate a retirement event on record for this id without a
    // memory_index row — the belt-and-suspenders case a supersede/delete
    // producer would need to create for this to happen naturally (neither
    // does today; this proves the sweep still refuses to resurrect it if
    // that ever changes).
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
    assert_eq!(plan.flagged_for_review[0].entry.memory_id, "retired1");

    // Apply must not touch it either.
    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 0);
    assert_eq!(report.found.flagged_for_review.len(), 1);
    assert!(report.stale.is_empty());
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

#[test]
fn a_file_whose_front_matter_id_disagrees_with_its_filename_is_flagged_not_guessed_at() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    // The file is named `mismatch1.md`, but its own front matter claims a
    // different id — e.g. a copy/paste or a hand-edited file. Written
    // directly (not through `write_memory_file`, which always names the
    // file after the entry's own id) to construct exactly this
    // inconsistency.
    let inner = seed_memory(
        "actually-a-different-id",
        MemoryScope::Project,
        "id disagrees with filename",
    );
    let project_dir = root.join(".localmind").join("memory").join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("mismatch1.md"),
        MarkdownMemoryFormat::serialize(&inner),
    )
    .unwrap();

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert!(plan.reindexable.is_empty());
    assert_eq!(plan.flagged_for_review.len(), 1);
    assert_eq!(plan.flagged_for_review[0].entry.memory_id, "mismatch1");

    // Apply must not index it under either id.
    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 0);
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "mismatch1"
            || record.memory_id.as_str() == "actually-a-different-id"));
}

#[test]
fn a_file_whose_front_matter_scope_disagrees_with_its_directory_is_flagged_not_guessed_at() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    // Filed under `project/`, but its own front matter claims `GlobalUser`
    // scope.
    let inner = seed_memory(
        "scopemismatch1",
        MemoryScope::GlobalUser,
        "scope disagrees with directory",
    );
    let project_dir = root.join(".localmind").join("memory").join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("scopemismatch1.md"),
        MarkdownMemoryFormat::serialize(&inner),
    )
    .unwrap();

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert!(plan.reindexable.is_empty());
    assert_eq!(plan.flagged_for_review.len(), 1);
    assert_eq!(plan.flagged_for_review[0].entry.memory_id, "scopemismatch1");

    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 0);
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "scopemismatch1"));
}
