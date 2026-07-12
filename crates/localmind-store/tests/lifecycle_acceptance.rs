//! Memory lifecycle acceptance invariants that span the whole feature (the
//! per-feature behaviour is pinned in `usage_tracking.rs`, `freshness_pass.rs`,
//! and `source_revalidation.rs`):
//!
//! - retrieval is **read-only** — a search never bumps a usage count;
//! - the **empty store** is inert — every lifecycle pass is a safe no-op;
//! - nothing here deletes (never-auto-delete is pinned in the per-feature suites).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope,
    MemoryStatus, SessionId,
};
use localmind_store::{FreshnessScope, FreshnessThresholds, MemoryPersistence, RevalidationConfig};

fn project_only() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

fn entry(id: &str, body: &str) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
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
fn retrieval_is_read_only_a_search_never_bumps_usage() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();
    persistence
        .persist_memory_entry(&entry("m", "use ripgrep for fast recursive search"))
        .unwrap();

    // Many searches that all match the memory.
    for _ in 0..5 {
        let hits = persistence.search("ripgrep").unwrap();
        assert!(hits.iter().any(|h| h.memory_id.as_str() == "m"));
    }

    // Usage stays zero: retrieval is a pure read. Only the explicit
    // post-turn record_memory_usage bumps it.
    let record = &persistence.list_memory().unwrap()[0];
    assert_eq!(record.hit_count, 0, "a search must not increment usage");
    assert_eq!(record.last_used_at, None);

    // The explicit post-turn path is the only thing that bumps it.
    persistence
        .record_memory_usage(&[MemoryEntryId::new("m")])
        .unwrap();
    assert_eq!(persistence.list_memory().unwrap()[0].hit_count, 1);
}

#[test]
fn an_empty_store_is_inert_for_every_lifecycle_pass() {
    let project = project_only();
    let persistence = MemoryPersistence::open_project(project.path()).unwrap();

    // No accepted memory yet: every pass is a safe no-op, never an error.
    assert_eq!(persistence.record_memory_usage(&[]).unwrap(), 0);
    assert_eq!(
        persistence
            .record_memory_usage(&[MemoryEntryId::new("nope")])
            .unwrap(),
        0
    );
    assert!(persistence.list_never_retrieved().unwrap().is_empty());
    assert!(persistence.list_most_used(10).unwrap().is_empty());
    assert!(persistence.list_stale_candidates().unwrap().is_empty());

    let freshness = persistence
        .freshness_pass(&FreshnessThresholds::default(), FreshnessScope::Both, false)
        .unwrap();
    assert_eq!(freshness.scanned, 0);
    assert!(freshness.flagged.is_empty());

    // No model configured -> re-validation is unavailable, not an error.
    assert!(persistence
        .revalidate_with_model(&RevalidationConfig::default(), false)
        .unwrap()
        .is_none());
}
