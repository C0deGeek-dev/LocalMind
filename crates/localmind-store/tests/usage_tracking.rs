//! Per-memory usage tracking (schema v8): `record_memory_usage` bumps a hit
//! count and stamps `last_used_at` across the project **and** global stores, and
//! the never-retrieved / most-used queries surface dead weight and high-value
//! lessons. The bump is best-effort and idempotent-per-call; a non-matching id
//! is a no-op (the synthetic primer / ingest ids fall through harmlessly).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::MemoryPersistence;

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
    }
}

#[test]
fn record_memory_usage_bumps_count_and_stamps_last_used() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry("p1", "use ripgrep to search", MemoryScope::Project))
        .unwrap();

    // Fresh memory starts never-retrieved.
    let before = &persistence.list_memory().unwrap()[0];
    assert_eq!(before.hit_count, 0);
    assert_eq!(before.last_used_at, None);

    let updated = persistence
        .record_memory_usage(&[MemoryEntryId::new("p1")])
        .unwrap();
    assert_eq!(updated, 1, "one active memory matched the bump");

    let after = &persistence.list_memory().unwrap()[0];
    assert_eq!(after.hit_count, 1);
    assert!(
        after.last_used_at.is_some(),
        "a used memory stamps last_used_at"
    );

    // A second turn bumps it again.
    persistence
        .record_memory_usage(&[MemoryEntryId::new("p1")])
        .unwrap();
    assert_eq!(persistence.list_memory().unwrap()[0].hit_count, 2);
}

#[test]
fn record_memory_usage_is_idempotent_per_call_and_ignores_unknown_ids() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry("p1", "use ripgrep", MemoryScope::Project))
        .unwrap();

    // The same id twice in one call counts once; unknown ids (the synthetic
    // primer id, an ingest chunk id) are silently ignored, never an error.
    let updated = persistence
        .record_memory_usage(&[
            MemoryEntryId::new("p1"),
            MemoryEntryId::new("p1"),
            MemoryEntryId::new("<repository-primer>"),
            MemoryEntryId::new("does-not-exist"),
        ])
        .unwrap();
    assert_eq!(updated, 1, "deduped to one matched row");
    assert_eq!(persistence.list_memory().unwrap()[0].hit_count, 1);

    // An empty set is a no-op.
    assert_eq!(persistence.record_memory_usage(&[]).unwrap(), 0);
}

#[test]
fn record_memory_usage_spans_the_global_store() {
    let global = tempfile::tempdir().unwrap();
    let global_root = global.path().join("memory");
    let project = project_with_global(&global_root);
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    persistence
        .persist_memory_entry(&entry("p1", "project lesson body", MemoryScope::Project))
        .unwrap();
    persistence
        .persist_memory_entry(&entry("g1", "global lesson body", MemoryScope::GlobalUser))
        .unwrap();

    // One call bumps a memory in each store (the dead-weight target lives in the
    // global store, so usage must reach it).
    let updated = persistence
        .record_memory_usage(&[MemoryEntryId::new("p1"), MemoryEntryId::new("g1")])
        .unwrap();
    assert_eq!(updated, 2, "both the project and the global row are bumped");

    let records = persistence.list_memory().unwrap();
    for id in ["p1", "g1"] {
        let record = records
            .iter()
            .find(|r| r.memory_id.as_str() == id)
            .unwrap_or_else(|| panic!("{id} present"));
        assert_eq!(record.hit_count, 1, "{id} bumped");
    }
}

#[test]
fn never_retrieved_and_most_used_surface_the_extremes() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    for id in ["hot", "warm", "cold"] {
        persistence
            .persist_memory_entry(&entry(
                id,
                &format!("{id} lesson body"),
                MemoryScope::Project,
            ))
            .unwrap();
    }
    // hot used 3x, warm 1x, cold never.
    for _ in 0..3 {
        persistence
            .record_memory_usage(&[MemoryEntryId::new("hot")])
            .unwrap();
    }
    persistence
        .record_memory_usage(&[MemoryEntryId::new("warm")])
        .unwrap();

    let never: Vec<String> = persistence
        .list_never_retrieved()
        .unwrap()
        .into_iter()
        .map(|r| r.memory_id.to_string())
        .collect();
    assert_eq!(never, vec!["cold".to_string()], "only the unused lesson");

    let most = persistence.list_most_used(2).unwrap();
    assert_eq!(most.len(), 2, "capped at the limit");
    assert_eq!(most[0].memory_id.as_str(), "hot");
    assert_eq!(most[0].hit_count, 3);
    assert_eq!(most[1].memory_id.as_str(), "warm");
}

#[test]
fn search_results_expose_the_usage_count() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry(
            "p1",
            "use ripgrep for fast recursive search",
            MemoryScope::Project,
        ))
        .unwrap();
    persistence
        .record_memory_usage(&[MemoryEntryId::new("p1")])
        .unwrap();

    let hits = persistence.search("ripgrep").unwrap();
    let hit = hits
        .iter()
        .find(|h| h.memory_id.as_str() == "p1")
        .expect("the lesson matches");
    assert_eq!(
        hit.hit_count, 1,
        "retrieval exposes the usage count read-only"
    );
}
