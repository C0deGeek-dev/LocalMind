//! Candidate identity is content-bound, and attaching evidence changes nothing
//! about who decides.
//!
//! Two separate guarantees, both easy to lose in the same place. The first: a
//! lesson re-derived from new evidence usually keeps its wording, so a queue
//! that dedups on wording alone keeps the stale record and drops the new one.
//! The second: making a candidate better-evidenced must not make it harder to
//! accept. Review mode is a user setting, and a plan does not get to quietly
//! revoke it by attaching a draft.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, CausalHypothesis, Confidence, EvidenceKind, EvidenceRef, HindsightDraft,
    LessonCategory, LessonId, ReviewState, SessionId, SuggestedAction,
};
use localmind_store::{ReviewModeProcessor, ReviewQueue};

const SUMMARY: &str = "Run the database migration before starting the server.";

fn project(mode: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".localmind.toml"),
        format!(
            "[learning]\nenabled = true\nallowed_scopes = [\"project\"]\n\n\
             [review]\nmode = \"{mode}\"\ntrusted_threshold = 0.5\n",
        ),
    )
    .unwrap();
    dir
}

fn fact(locator: &str, content: &str) -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::TestOutput,
        "migration output",
        "session:one",
        locator,
        content,
    )
}

fn candidate(facts: &[EvidenceRef]) -> CandidateLesson {
    let mut candidate = CandidateLesson::new(
        LessonId::new("lesson-1"),
        SUMMARY,
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    for fact in facts {
        candidate = candidate.with_evidence(fact.clone());
    }
    candidate
}

fn draft(facts: &[EvidenceRef]) -> HindsightDraft {
    HindsightDraft::new(
        "the server starts",
        "the server started against an old schema",
    )
    .with_hypothesis(CausalHypothesis {
        claim: "the server booted before the migration had run".to_string(),
        evidence_ids: facts.iter().map(|fact| fact.id.clone()).collect(),
        confidence: Confidence::new(0.7).unwrap(),
    })
    .with_proposed_lesson(SUMMARY)
}

#[test]
fn a_revision_replaces_the_pending_row_instead_of_collapsing_into_it() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let session = SessionId::new("session");

    let first_facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    let original = candidate(&first_facts);
    let original_identity = original.content_identity();
    queue.enqueue_candidates(&session, &[original]).unwrap();

    // Same sentence, different substance: a second observation and the
    // hindsight draft that explains it.
    let second_facts = vec![
        fact("repo@aaa#migrate.log", "sha256:01"),
        fact("repo@bbb#boot.log", "sha256:02"),
    ];
    let revision = candidate(&second_facts).with_hindsight(draft(&second_facts));
    let revision_identity = revision.content_identity();
    assert_ne!(
        original_identity, revision_identity,
        "the wording is identical, so identity must come from somewhere else"
    );
    queue.enqueue_candidates(&session, &[revision]).unwrap();

    let items = queue.list().unwrap();
    assert_eq!(items.len(), 1, "the queue still dedups to one row");

    let stored = &items[0].candidate;
    assert_eq!(
        stored.evidence().len(),
        2,
        "the row holds the revision, not the record it superseded"
    );
    assert!(stored.hindsight.is_some());
    assert_eq!(
        stored.revises.as_deref(),
        Some(original_identity.as_str()),
        "lineage names what was replaced rather than leaving it to be inferred"
    );
    assert_eq!(stored.content_identity(), revision_identity);
}

#[test]
fn a_genuine_restatement_still_merges_and_claims_no_lineage() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let session = SessionId::new("session");

    let facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    queue
        .enqueue_candidates(&session, &[candidate(&facts)])
        .unwrap();
    queue
        .enqueue_candidates(&session, &[candidate(&facts)])
        .unwrap();

    let items = queue.list().unwrap();
    assert_eq!(items.len(), 1);
    assert!(
        items[0].candidate.revises.is_none(),
        "restating a lesson is not revising it"
    );
    assert!(items[0].candidate.hindsight.is_none());
}

#[test]
fn identity_ignores_review_annotation_so_annotating_a_row_does_not_rewrite_it() {
    let facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    let bare = candidate(&facts);
    let mut annotated = bare.clone();
    annotated.review_annotation = Some(localmind_core::ReviewAnnotation {
        score: Confidence::new(0.9).unwrap(),
        duplicate_of: Some("mem-1".to_string()),
        conflict: true,
        notes: "a review note".to_string(),
    });

    assert_eq!(bare.content_identity(), annotated.content_identity());
}

#[test]
fn identity_ignores_lineage_so_the_same_revision_derived_twice_agrees_with_itself() {
    let facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    let plain = candidate(&facts);
    let with_lineage = candidate(&facts).revising("cnd-0123456789abcdef0123456789abcdef");

    assert_eq!(plain.content_identity(), with_lineage.content_identity());
}

/// The D020 gate. `ReviewModeProcessor::apply_project` is not modified and must
/// not become sensitive to lab output: a candidate carrying a full hindsight
/// draft and one carrying none reach the same decision on the same inputs.
///
/// The rejected alternative was to gate every lab-carrying candidate the way a
/// direct agent proposal is gated. That would mean a candidate which
/// auto-accepts today needs human review the moment evidence is attached —
/// penalising exactly the best-evidenced candidates, and quietly removing a
/// setting the user chose.
#[test]
fn attaching_hindsight_changes_no_automatic_review_decision() {
    let facts = vec![
        fact("repo@aaa#migrate.log", "sha256:01"),
        fact("repo@bbb#boot.log", "sha256:02"),
    ];

    let outcome = |with_hindsight: bool| {
        let dir = project("automatic");
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        let candidate = if with_hindsight {
            candidate(&facts).with_hindsight(draft(&facts))
        } else {
            candidate(&facts)
        };
        queue
            .enqueue_candidates(&SessionId::new("session"), &[candidate])
            .unwrap();

        let report = ReviewModeProcessor::apply_project(dir.path()).unwrap();
        let items = queue.list().unwrap();
        let states: Vec<ReviewState> = items.iter().map(|item| item.state.clone()).collect();
        let annotations: Vec<Option<String>> = items
            .iter()
            .map(|item| {
                item.candidate
                    .review_annotation
                    .as_ref()
                    .map(|annotation| annotation.notes.clone())
            })
            .collect();
        (
            report.annotated,
            report.accepted,
            report.manual,
            states,
            annotations,
        )
    };

    assert_eq!(
        outcome(true),
        outcome(false),
        "evidence may inform a reviewer; it may never be what decides"
    );
}

#[test]
fn a_hindsight_carrying_candidate_survives_the_store_round_trip_intact() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let facts = vec![
        fact("repo@aaa#migrate.log", "sha256:01"),
        fact("repo@bbb#boot.log", "sha256:02"),
    ];
    let candidate = candidate(&facts).with_hindsight(draft(&facts));

    queue
        .enqueue_candidates(&SessionId::new("session"), &[candidate.clone()])
        .unwrap();

    let items = queue.list().unwrap();
    let stored = &items[0].candidate;

    assert_eq!(stored, &candidate);
    assert!(
        stored.validate_hindsight().is_ok(),
        "a draft that validated before storage must still validate after it"
    );
}
