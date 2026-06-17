//! End-to-end: a supersede review decision retires the prior memory so retrieval
//! stops serving it, records the link, and audits the change.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, LessonCategory, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, MemoryStatus, ReviewAction, ReviewDecision, ReviewItemId,
    SessionId, SuggestedAction,
};
use localmind_store::{MemoryPersistence, ReviewQueue};

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
    }
}

fn candidate(id: &str, summary: &str) -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new(id),
        summary,
        LessonCategory::ProjectConvention,
        Confidence::new(0.8).unwrap(),
        SuggestedAction::SupersedeExisting,
    )
    .with_evidence(EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted())
}

#[test]
fn supersede_retires_the_target_records_the_link_and_audits_it() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    // An accepted memory M1 that retrieval currently serves.
    persistence
        .persist_memory_entry(&seed_memory(
            "m1",
            "use tabs for indentation in this project",
        ))
        .unwrap();
    assert_eq!(persistence.search("tabs").unwrap().len(), 1);
    assert!(persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "m1"));

    // A corrective candidate is decided as a supersede of M1.
    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("s2"),
            &[candidate(
                "m2",
                "do not use tabs for indentation; use spaces instead",
            )],
        )
        .unwrap();
    let decided = queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new("m2"),
            action: ReviewAction::Supersede(MemoryEntryId::new("m1")),
            reviewer: "tester".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();
    // The decision is accepted and carries the supersede target.
    assert_eq!(decided.state, localmind_core::ReviewState::Accepted);
    assert_eq!(
        decided.supersede_target.as_ref().map(MemoryEntryId::as_str),
        Some("m1")
    );

    // Promote the supersede.
    let new_entry = persistence
        .promote_review_item(&ReviewItemId::new("m2"))
        .unwrap();

    // The new memory records the link; the target leaves the active set.
    assert_eq!(new_entry.supersedes, vec![MemoryEntryId::new("m1")]);
    assert!(
        !persistence
            .list_memory()
            .unwrap()
            .iter()
            .any(|record| record.memory_id.as_str() == "m1"),
        "the superseded memory must drop out of the active set"
    );

    // Retrieval returns only the replacement, never the retired memory.
    let hits = persistence.search("tabs").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory_id.as_str(), "m2");

    // A MemorySuperseded audit row links both memories and the reviewer.
    let audit = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .find(|row| row.kind == "MemorySuperseded")
        .expect("a MemorySuperseded audit row");
    assert_eq!(audit.subject, "m1");
    assert_eq!(audit.actor, "tester");
    assert!(audit.metadata_json.contains("\"superseded_by\":\"m2\""));
}
