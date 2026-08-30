//! Contradiction detection and the provenance answer.
//!
//! Two accepted memories about the same topic with opposite recommendation
//! polarity must produce a `contradicts` relationship that surfaces in ranked
//! results, and the provenance answer must report source, confidence, epistemic
//! status, and contradictions for a memory.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    Confidence, EpistemicStatus, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, SessionId,
};
use localmind_store::MemoryPersistence;
use std::fs;
use std::path::Path;
use time::OffsetDateTime;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".localmind.toml"),
        "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n",
    )
    .unwrap();
    dir
}

#[allow(clippy::too_many_arguments)]
fn memory(
    id: &str,
    body: &str,
    category: LessonCategory,
    entity: &str,
    session: &str,
    confidence: f32,
) -> MemoryEntry {
    MemoryEntry {
        id: MemoryEntryId::new(id),
        scope: MemoryScope::Project,
        body: body.to_string(),
        category,
        confidence: Confidence::new(confidence).unwrap(),
        source_session: Some(SessionId::new(session)),
        evidence: vec![EvidenceRef::new(EvidenceKind::ManualNote, "seeded")],
        tags: Vec::new(),
        related_files: Vec::new(),
        related_entities: vec![entity.to_string()],
        created_at: Some(OffsetDateTime::now_utc()),
        updated_at: None,
        supersedes: Vec::new(),
        contradicts: Vec::new(),
        status: MemoryStatus::Active,
        sync_meta: localmind_core::SyncMeta::default(),
    }
}

fn persist(root: &Path, entry: &MemoryEntry) {
    let persistence = MemoryPersistence::open_project(root).unwrap();
    persistence.persist_memory_entry(entry).unwrap();
}

#[test]
fn opposing_memories_on_one_topic_contradict_and_surface_in_search() {
    let dir = project();
    let root = dir.path();

    // A endorses the raw claim query; B prohibits it — same topic, opposite
    // polarity. B is promoted second, so detection runs against A.
    persist(
        root,
        &memory(
            "mem-endorse",
            "Always keep the raw SQL claim query for the outbox.",
            LessonCategory::ProjectConvention,
            "src/outbox.rs::claim",
            "session-1",
            0.8,
        ),
    );
    persist(
        root,
        &memory(
            "mem-prohibit",
            "Do not use the raw SQL claim query; use the ORM instead.",
            LessonCategory::AntiPattern,
            "src/outbox.rs::claim",
            "session-2",
            0.7,
        ),
    );

    let persistence = MemoryPersistence::open_project(root).unwrap();
    let results = persistence.search("claim query").unwrap();
    let endorse = results
        .iter()
        .find(|r| r.memory_id.as_str() == "mem-endorse")
        .expect("endorsing memory should be retrievable");
    let prohibit = results
        .iter()
        .find(|r| r.memory_id.as_str() == "mem-prohibit")
        .expect("prohibiting memory should be retrievable");

    assert!(
        endorse.contradicted && prohibit.contradicted,
        "both conflicting memories must be flagged contradicted"
    );
    // Epistemic status is classified from category.
    assert_eq!(endorse.epistemic_status, EpistemicStatus::Decision);
    assert_eq!(prohibit.epistemic_status, EpistemicStatus::Decision);
}

#[test]
fn unrelated_memories_do_not_contradict() {
    let dir = project();
    let root = dir.path();
    persist(
        root,
        &memory(
            "mem-a",
            "Do not block the audio thread.",
            LessonCategory::AntiPattern,
            "src/audio.rs::play",
            "s1",
            0.8,
        ),
    );
    persist(
        root,
        &memory(
            "mem-b",
            "Prefer the squared distance form.",
            LessonCategory::CodePattern,
            "src/geometry.rs::norm",
            "s2",
            0.8,
        ),
    );
    let persistence = MemoryPersistence::open_project(root).unwrap();
    for result in persistence.search("audio distance").unwrap() {
        assert!(
            !result.contradicted,
            "memories on different topics must not contradict: {:?}",
            result.memory_id
        );
    }
}

#[test]
fn provenance_answers_why_do_you_think_that() {
    let dir = project();
    let root = dir.path();
    persist(
        root,
        &memory(
            "mem-endorse",
            "Always keep the raw SQL claim query.",
            LessonCategory::ProjectConvention,
            "src/outbox.rs::claim",
            "session-1",
            0.8,
        ),
    );
    persist(
        root,
        &memory(
            "mem-prohibit",
            "Do not use the raw SQL claim query.",
            LessonCategory::AntiPattern,
            "src/outbox.rs::claim",
            "session-2",
            0.7,
        ),
    );

    let persistence = MemoryPersistence::open_project(root).unwrap();
    let provenance = persistence
        .provenance(&MemoryEntryId::new("mem-prohibit"))
        .unwrap()
        .expect("known memory has provenance");

    assert_eq!(provenance.source_session.as_deref(), Some("session-2"));
    assert!((provenance.confidence - 0.7).abs() < 1e-3);
    assert_eq!(provenance.epistemic_status, EpistemicStatus::Decision);
    assert_eq!(provenance.status, "active");
    assert_eq!(
        provenance.contradicts,
        vec![MemoryEntryId::new("mem-endorse")],
        "provenance must name the memory it contradicts"
    );

    // An unknown memory has no provenance.
    assert!(persistence
        .provenance(&MemoryEntryId::new("nope"))
        .unwrap()
        .is_none());
}
