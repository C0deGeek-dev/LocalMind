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

/// A project scoped to `["project"]` **only** — no global scope, ever. Every
/// test in this file that has no reason to touch the global store at all
/// uses this, so it can never accidentally open the real machine-wide
/// `~/.localmind` store: `allowed_scopes` defaults to
/// `["project", "global_user"]`, and a bare `enabled = true` config would
/// silently inherit that default.
fn enabled_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

/// A project with an **explicit, hermetic** `global_memory_root` under its
/// own tempdir, for the one test that specifically exercises global-scope
/// reconciliation. Never resolves to the real per-user `~/.localmind` store.
fn enabled_project_with_global() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let global_root = dir.path().join("hermetic-global-root");
    std::fs::create_dir_all(&global_root).unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\", \"global_user\"]\nglobal_memory_root = {global_root:?}\n"
        ),
    )
    .unwrap();
    (dir, global_root)
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

/// Global-scope coverage, fully hermetic: `enabled_project_with_global`
/// points `global_memory_root` at a tempdir under this test's own project,
/// never the real per-user `~/.localmind` store.
#[test]
fn a_global_scope_orphan_is_found_and_reindexed_without_touching_the_real_home_store() {
    let (dir, global_root) = enabled_project_with_global();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    write_unindexed(
        root,
        &seed_memory(
            "global-orphan1",
            MemoryScope::GlobalUser,
            "a cross-project lesson written but never indexed",
        ),
    );

    // Confirm the file actually landed under the hermetic global root, not
    // the real per-user store.
    assert!(global_root
        .join("global")
        .join("global-orphan1.md")
        .exists());

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert_eq!(plan.reindexable.len(), 1);
    assert_eq!(plan.reindexable[0].memory_id, "global-orphan1");
    assert_eq!(plan.reindexable[0].scope, MemoryScope::GlobalUser);

    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 1);
    assert!(persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "global-orphan1"));
}

/// Non-`Project` project-rooted scope coverage: a project that opts into
/// `session` scope (per `[learning] allowed_scopes`) gets that directory
/// swept too, not only `project/`.
#[test]
fn a_session_scope_orphan_is_found_and_reindexed_when_the_project_allows_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\", \"session\"]\n",
    )
    .unwrap();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    write_unindexed(
        root,
        &seed_memory(
            "session-orphan1",
            MemoryScope::Session,
            "a session-scoped lesson written but never indexed",
        ),
    );

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert_eq!(plan.reindexable.len(), 1);
    assert_eq!(plan.reindexable[0].memory_id, "session-orphan1");
    assert_eq!(plan.reindexable[0].scope, MemoryScope::Session);

    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 1);
    assert!(persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "session-orphan1"));
}

/// A scope the project's config does **not** allow is never scanned, even
/// if a stray file happens to sit in that directory (e.g. left over from a
/// config change that narrowed `allowed_scopes`).
#[test]
fn a_disallowed_scope_directory_is_never_scanned() {
    let dir = enabled_project(); // allowed_scopes = ["project"] only
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    // A file sitting in `skill/` even though this project's config never
    // allows the `skill` scope.
    let skill_dir = root.join(".localmind").join("memory").join("skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let stray = seed_memory(
        "stray-skill1",
        MemoryScope::Skill,
        "should never be scanned",
    );
    std::fs::write(
        skill_dir.join("stray-skill1.md"),
        MarkdownMemoryFormat::serialize(&stray),
    )
    .unwrap();

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert!(
        plan.is_empty(),
        "a disallowed scope directory must not be swept at all"
    );
}

/// A `.md` symlink inside the scope directory is never opened or followed —
/// it is flagged, not silently read (it could point outside the memory
/// root entirely). Unix-only: creating a file symlink without elevation is
/// unreliable on Windows CI runners; the guard itself
/// (`DirEntry::file_type`, which never follows links) is platform-uniform,
/// so this one platform's coverage stands for the mechanism.
#[cfg(unix)]
#[test]
fn a_symlinked_md_entry_is_flagged_and_never_read() {
    use localmind_store::FlagReason;

    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    // A real file living outside the memory root entirely.
    let outside_target = root.join("outside-memory-root.txt");
    std::fs::write(&outside_target, "not a memory file at all").unwrap();

    let project_dir = root.join(".localmind").join("memory").join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::os::unix::fs::symlink(&outside_target, project_dir.join("symlinked1.md")).unwrap();

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert!(plan.reindexable.is_empty());
    assert_eq!(plan.flagged_for_review.len(), 1);
    assert_eq!(plan.flagged_for_review[0].entry.memory_id, "symlinked1");
    assert_eq!(
        plan.flagged_for_review[0].reason,
        FlagReason::NotRegularFile
    );

    let report = persistence.orphan_sweep_apply().unwrap();
    assert_eq!(report.reindexed, 0);
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "symlinked1"));
}

/// An id that is *already indexed* is out of scope for this sweep
/// regardless of what its filename happens to be on disk — including a
/// symlink. The already-indexed check must run before the file-type check,
/// or an indexed id whose file happens to be a symlink would be
/// (incorrectly) flagged instead of silently skipped like any other
/// already-indexed file.
#[cfg(unix)]
#[test]
fn an_already_indexed_id_is_never_flagged_even_if_its_file_is_a_symlink() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    let db_path = root.join(".localmind").join("localmind.sqlite");
    let raw = rusqlite::Connection::open(&db_path).unwrap();
    raw.execute(
        "INSERT INTO memory_index \
         (memory_id, path, scope, category, body, source_session, status, created_at, \
          epistemic_status, confidence, language, origin_device) \
         VALUES ('already-indexed1', 'irrelevant.md', 'Project', 'ProjectConvention', \
          'already indexed elsewhere', NULL, 'active', '2026-01-01T00:00:00Z', \
          'observation', 0.9, NULL, NULL)",
        [],
    )
    .unwrap();
    drop(raw);

    let outside_target = root.join("outside-memory-root.txt");
    std::fs::write(&outside_target, "not a memory file at all").unwrap();
    let project_dir = root.join(".localmind").join("memory").join("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::os::unix::fs::symlink(&outside_target, project_dir.join("already-indexed1.md")).unwrap();

    let plan = persistence.orphan_sweep_plan().unwrap();
    assert!(
        plan.is_empty(),
        "an already-indexed id must be silently skipped, not flagged, regardless of file type"
    );
}
