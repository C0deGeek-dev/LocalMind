//! Machine-wide global-scope memory: a global lesson persists outside any
//! project, is shared across projects, and is routed there by the scope
//! classifier through the review gate. Project memory wins on conflict.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, ReviewAction, ReviewDecision, ReviewItemId,
    SessionId, SuggestedAction,
};
use localmind_store::{MemoryPersistence, MemoryPersistenceError, ReviewQueue};

/// A project that opts in to global scope, with the machine-wide store rooted at
/// `global_root` (a single-quoted TOML literal so a Windows path needs no
/// escaping).
fn project_with_global(global_root: &Path) -> tempfile::TempDir {
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
fn a_global_memory_persists_outside_the_project_and_survives_its_deletion() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = project_with_global(&global_root);

    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    let path = persistence
        .persist_memory_entry(&entry(
            "g1",
            "use ripgrep to search code",
            MemoryScope::GlobalUser,
        ))
        .unwrap();

    // The file lives under the machine-wide global root, not the project.
    assert!(
        path.starts_with(&global_root),
        "global memory must be under the global root: {path:?}"
    );
    assert!(!path.starts_with(project.path()));
    assert!(path.is_file());

    // It outlives the project workspace.
    drop(persistence);
    std::fs::remove_dir_all(project.path()).unwrap();
    assert!(
        path.is_file(),
        "global memory must survive a project-workspace deletion"
    );
}

#[test]
fn a_project_can_narrow_to_project_only_and_then_a_global_write_is_refused() {
    let project = tempfile::tempdir().unwrap();
    // Global is on by default; a project that wants project-only memory narrows
    // `allowed_scopes` to `["project"]`, after which a global write is refused.
    std::fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    let err = persistence
        .persist_memory_entry(&entry("g1", "x", MemoryScope::GlobalUser))
        .unwrap_err();
    assert!(
        matches!(err, MemoryPersistenceError::GlobalScopeDisabled),
        "a project narrowed to project-only must refuse a global write, got {err:?}"
    );
}

#[test]
fn global_scope_is_on_by_default_no_opt_in_needed() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = tempfile::tempdir().unwrap();
    // A bare config (only `enabled = true`) — global is allowed by default. The
    // global root is pinned to a temp dir so the test never touches the real home.
    std::fs::write(
        project.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nglobal_memory_root = '{}'\n",
            global_root.display()
        ),
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    let path = persistence
        .persist_memory_entry(&entry("g1", "use ripgrep", MemoryScope::GlobalUser))
        .expect("global write is allowed by default");
    assert!(path.starts_with(&global_root));
}

#[test]
fn global_memory_is_retrievable_across_projects_with_project_precedence() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");

    // Project A writes a global lesson, then closes.
    let project_a = project_with_global(&global_root);
    {
        let a = MemoryPersistence::open_project(project_a.path()).unwrap();
        a.persist_memory_entry(&entry(
            "g1",
            "use ripgrep for fast code search",
            MemoryScope::GlobalUser,
        ))
        .unwrap();
    }

    // Project B (a different workspace, the same global store) retrieves it.
    let project_b = project_with_global(&global_root);
    let b = MemoryPersistence::open_project(project_b.path()).unwrap();
    let hits = b.search("ripgrep").unwrap();
    assert!(
        hits.iter().any(|h| h.memory_id.as_str() == "g1"),
        "a global lesson written in another project must surface here: {hits:?}"
    );

    // A project lesson on the same query wins — it ranks ahead of the global one.
    b.persist_memory_entry(&entry(
        "p1",
        "use ripgrep but exclude the target directory",
        MemoryScope::Project,
    ))
    .unwrap();
    let hits = b.search("ripgrep").unwrap();
    let p1 = hits.iter().position(|h| h.memory_id.as_str() == "p1");
    let g1 = hits.iter().position(|h| h.memory_id.as_str() == "g1");
    assert!(
        p1.is_some() && g1.is_some() && p1 < g1,
        "the project lesson must override (rank ahead of) the global one: {hits:?}"
    );
}

#[test]
fn a_promoted_global_category_lesson_is_routed_to_the_global_store() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = project_with_global(&global_root);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    // A DebuggingRecipe candidate classifies global; accept and promote it.
    let queue = ReviewQueue::open_project(project.path()).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("s1"),
            &[CandidateLesson::new(
                LessonId::new("t1"),
                "prefer ripgrep over grep for recursive search speed",
                LessonCategory::DebuggingRecipe,
                Confidence::new(0.8).unwrap(),
                SuggestedAction::PromoteToMemory,
            )
            .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())],
        )
        .unwrap();
    queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new("t1"),
            action: ReviewAction::Accept,
            reviewer: "tester".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();

    let promoted = persistence
        .promote_review_item(&ReviewItemId::new("t1"))
        .unwrap();
    assert_eq!(
        promoted.scope,
        MemoryScope::GlobalUser,
        "a global-category lesson must promote into global scope"
    );

    // It landed in the global store: a different project retrieves it.
    drop(persistence);
    let other = project_with_global(&global_root);
    let o = MemoryPersistence::open_project(other.path()).unwrap();
    assert!(
        o.search("ripgrep")
            .unwrap()
            .iter()
            .any(|h| h.memory_id.as_str() == "t1"),
        "the promoted global lesson must be in the machine-wide store"
    );
}
