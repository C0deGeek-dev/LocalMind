//! Lifting a staleness flag — the "keep this" side of the freshness pass.
//!
//! Until this was wired, `clear_stale_candidate` had **no caller in either
//! repository** and reached **only the project store**, while the freshness pass
//! flags both. So a pass could flag scores of global memories and nothing shipped
//! could undo it — and a flagged memory is excluded from the queryless context
//! primer (`primer.rs` skips `stale_candidate`), so the effect was silent as well
//! as irreversible.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::MemoryPersistence;

fn project_with_global(global: &tempfile::TempDir) -> tempfile::TempDir {
    // The global database lives beside the memory root, so keep the root one
    // level below the fixture directory. Passing the TempDir makes it impossible
    // for a caller to accidentally point the database at the shared temp root.
    let global_root = global.path().join("memory");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\", \"global_user\"]\nglobal_memory_root = '{}'\n",
            global_root.display()
        ),
    )
    .unwrap();
    dir
}

#[test]
fn global_database_is_scoped_to_the_fixture_tempdir() {
    let global = tempfile::tempdir().unwrap();
    let project = project_with_global(&global);
    let _store = MemoryPersistence::open_project(project.path()).unwrap();

    let fixture_database = global.path().join("localmind.sqlite");
    assert!(
        fixture_database.exists(),
        "the global database must be created inside the fixture: {fixture_database:?}"
    );
    assert!(fixture_database.starts_with(global.path()));
    assert_ne!(
        fixture_database,
        std::env::temp_dir().join("localmind.sqlite"),
        "the fixture must never resolve to the process-wide temporary database"
    );
}

fn entry(id: &str, body: &str, scope: MemoryScope) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope,
        body: body.to_string(),
        category: LessonCategory::DebuggingRecipe,
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
        sync_meta: localmind_core::SyncMeta::default(),
    }
}

#[test]
fn a_flag_is_cleared_in_whichever_store_holds_the_memory() {
    // The defect that mattered: flags land in the global store, and a clear that
    // only reached the project store could not undo most of what a pass had done.
    let global = tempfile::tempdir().unwrap();
    let project = project_with_global(&global);
    let store = MemoryPersistence::open_project(project.path()).unwrap();

    store
        .persist_memory_entry(&entry("proj", "a project lesson", MemoryScope::Project))
        .unwrap();
    store
        .persist_memory_entry(&entry("glob", "a global lesson", MemoryScope::GlobalUser))
        .unwrap();

    let project_id = MemoryEntryId::new("proj");
    let global_id = MemoryEntryId::new("glob");
    assert!(store.mark_stale_candidate(&project_id).unwrap());
    assert!(store.mark_stale_candidate(&global_id).unwrap());
    assert_eq!(store.list_stale_candidates().unwrap().len(), 2);

    assert!(
        store.clear_stale_candidate(&global_id).unwrap(),
        "a global flag must be reachable"
    );
    let left = store.list_stale_candidates().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].as_str(), "proj");

    assert!(store.clear_stale_candidate(&project_id).unwrap());
    assert!(store.list_stale_candidates().unwrap().is_empty());
}

#[test]
fn clearing_an_unflagged_memory_reports_false_rather_than_succeeding() {
    // A caller needs to tell "I lifted a flag" from "there was nothing to lift",
    // or a typo in an id looks exactly like success.
    let global = tempfile::tempdir().unwrap();
    let project = project_with_global(&global);
    let store = MemoryPersistence::open_project(project.path()).unwrap();
    store
        .persist_memory_entry(&entry("kept", "never flagged", MemoryScope::Project))
        .unwrap();

    assert!(!store
        .clear_stale_candidate(&MemoryEntryId::new("kept"))
        .unwrap());
    assert!(!store
        .clear_stale_candidate(&MemoryEntryId::new("no-such-memory"))
        .unwrap());
}

#[test]
fn clearing_is_audited_so_the_lifecycle_reads_in_both_directions() {
    // A flag lifted with no trace is indistinguishable from one never set.
    let global = tempfile::tempdir().unwrap();
    let project = project_with_global(&global);
    let store = MemoryPersistence::open_project(project.path()).unwrap();
    store
        .persist_memory_entry(&entry("both", "flag me then keep me", MemoryScope::Project))
        .unwrap();
    let id = MemoryEntryId::new("both");

    store.mark_stale_candidate(&id).unwrap();
    store.clear_stale_candidate(&id).unwrap();

    let kinds: Vec<String> = store
        .audit_records()
        .unwrap()
        .iter()
        .map(|record| format!("{:?}", record.kind))
        .collect();
    assert!(
        kinds.iter().any(|k| k.contains("FlaggedStale")),
        "expected the flag event: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k.contains("FlagCleared")),
        "expected the clear event: {kinds:?}"
    );
}

#[test]
fn clearing_twice_is_idempotent() {
    let global = tempfile::tempdir().unwrap();
    let project = project_with_global(&global);
    let store = MemoryPersistence::open_project(project.path()).unwrap();
    store
        .persist_memory_entry(&entry("twice", "clear me twice", MemoryScope::Project))
        .unwrap();
    let id = MemoryEntryId::new("twice");
    store.mark_stale_candidate(&id).unwrap();

    assert!(store.clear_stale_candidate(&id).unwrap());
    assert!(
        !store.clear_stale_candidate(&id).unwrap(),
        "a second clear changes nothing and says so"
    );
}
