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
        sync_meta: localmind_core::SyncMeta::default(),
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

/// Subject 04's `ExistingItemDecision::Merge`, confirmed via a real
/// `review merge`-shaped decision against an *accepted* memory (not another
/// pending item — `MergeInto`'s existing target type): bookkeeping only,
/// exactly like `MergeInto` — it never mutates the target and is never
/// itself promoted, only a distinct `reviewer_action` in the audit trail so
/// "the reviewer recognized a near-duplicate of this accepted memory" is
/// not indistinguishable from a bare rejection.
#[test]
fn merge_into_memory_is_bookkeeping_only_and_never_mutates_or_promotes_anything() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    persistence
        .persist_memory_entry(&seed_memory(
            "m1",
            "run the integration suite after every exporter change",
        ))
        .unwrap();

    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("s2"),
            &[candidate(
                "m2",
                "after an exporter change, run the integration suite",
            )],
        )
        .unwrap();
    let decided = queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new("m2"),
            action: ReviewAction::MergeIntoMemory(MemoryEntryId::new("m1")),
            reviewer: "tester".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();
    // Merged, exactly like MergeInto — not Accepted/Edited, so
    // promote_review_item refuses it below.
    assert_eq!(decided.state, localmind_core::ReviewState::Merged);
    assert_eq!(
        decided
            .merge_memory_target
            .as_ref()
            .map(MemoryEntryId::as_str),
        Some("m1")
    );
    assert!(decided.supersede_target.is_none());
    assert_eq!(
        decided.reviewer_action.as_deref(),
        Some("merge_into_memory")
    );

    // Never promotable: a Merged item is not in the Accepted|Edited set
    // promote_review_item requires.
    assert!(persistence
        .promote_review_item(&ReviewItemId::new("m2"))
        .is_err());

    // The target memory is completely untouched: no status flip, no
    // MemorySuperseded audit row, no new memory for "m2".
    assert!(
        persistence
            .list_memory()
            .unwrap()
            .iter()
            .any(|record| record.memory_id.as_str() == "m1"),
        "the target memory must remain active and untouched"
    );
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "m2"));
    assert!(persistence
        .audit_records()
        .unwrap()
        .iter()
        .all(|row| row.kind != "MemorySuperseded"));
}

/// Subject 04's `ExistingItemDecision::Delete`: the existing memory is
/// removed outright and the candidate is **not** promoted as its
/// replacement — the CLI-layer sequence `review delete-existing` runs
/// (`decide` then `delete_memory`), proven here at the store-API level
/// directly.
#[test]
fn delete_existing_rejects_the_candidate_and_removes_the_target_without_promoting_it() {
    let dir = enabled_project();
    let root = dir.path();
    let persistence = MemoryPersistence::open_project(root).unwrap();

    persistence
        .persist_memory_entry(&seed_memory(
            "m1",
            "this tooling note is stale and not worth keeping",
        ))
        .unwrap();

    let queue = ReviewQueue::open_project(root).unwrap();
    queue
        .enqueue_candidates(
            &SessionId::new("s2"),
            &[candidate(
                "m2",
                "do not follow that stale tooling note anymore",
            )],
        )
        .unwrap();
    let decided = queue
        .decide(ReviewDecision {
            item_id: ReviewItemId::new("m2"),
            action: ReviewAction::DeleteExisting(MemoryEntryId::new("m1")),
            reviewer: "tester".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap();
    // The candidate is rejected, not accepted — DeleteExisting never
    // promotes it.
    assert_eq!(decided.state, localmind_core::ReviewState::Rejected);
    assert!(decided.supersede_target.is_none());
    // Subject 04 round 2: the target must be durable on the row the moment
    // decide() returns — this is the crash-window fix. Simulated here by
    // asserting it before the separate delete_memory() call below ever runs.
    assert_eq!(
        decided
            .delete_existing_target
            .as_ref()
            .map(|id| id.as_str()),
        Some("m1"),
        "the delete target must be durable even if delete_memory() never runs"
    );

    // The CLI-layer sequence: decide() closes the item, a separate
    // delete_memory() call actually removes the target.
    assert!(persistence
        .delete_memory(&MemoryEntryId::new("m1"), "tester")
        .unwrap());

    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "m1"));
    // The candidate was never promoted either — it stays rejected, no "m2"
    // memory exists.
    assert!(!persistence
        .list_memory()
        .unwrap()
        .iter()
        .any(|record| record.memory_id.as_str() == "m2"));

    let audit = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .find(|row| row.kind == "MemoryDeleted")
        .expect("a MemoryDeleted audit row");
    assert_eq!(audit.subject, "m1");
    assert!(audit
        .metadata_json
        .contains("\"before_body\":\"this tooling note is stale and not worth keeping\""));
}
