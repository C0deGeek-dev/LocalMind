//! Experiment evidence in the review queue.
//!
//! The queue is where evidence would quietly go wrong: a revision that sheds its
//! test history, a re-submission that drops a new result, a corrupt row read as
//! an empty one, or a verdict that starts steering review. Each has a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, CausalHypothesis, Confidence, EvidenceKind, EvidenceRef, EvidenceTier,
    ExperimentEvidence, ExperimentInputs, ExperimentProvenance, ExperimentViolation, FixtureRef,
    HindsightDraft, ImportedReceipt, InjectionMode, InjectionProof, LabVerdict, LessonAssignment,
    LessonCategory, LessonId, LessonRevision, OracleOrigin, OracleRef, ReviewAction,
    ReviewDecision, ReviewItemId, ReviewState, RevisionError, Sensitivity, SessionId,
    SuggestedAction, VerdictReason, VerifierRef, EXPERIMENT_EVIDENCE_VERSION,
    LESSON_ASSIGNMENT_VERSION,
};
use localmind_store::{
    MemoryPersistence, MemoryPersistenceError, ReviewModeProcessor, ReviewQueue, ReviewQueueError,
};
use serde::{Deserialize, Serialize};

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

fn result(
    candidate: &CandidateLesson,
    tier: EvidenceTier,
    verdict: LabVerdict,
) -> ExperimentEvidence {
    let assignment = LessonAssignment {
        version: LESSON_ASSIGNMENT_VERSION,
        candidate_identity: candidate.content_identity(),
        task: "start the server against a migrated schema".to_string(),
        task_evidence: Vec::new(),
        oracle: OracleRef {
            locator: "repo@aaa#tests/boot.rs".to_string(),
            content_hash: "sha256:oracle".to_string(),
            origin: OracleOrigin::Preexisting,
        },
        fixture: FixtureRef {
            locator: "repo@aaa".to_string(),
            content_hash: "sha256:fixture".to_string(),
        },
        initial_state: "unmigrated database".to_string(),
        allowed_tools: Vec::new(),
        success_observations: Vec::new(),
        failure_observations: Vec::new(),
        verifier: VerifierRef {
            name: "cargo-test".to_string(),
            version: "1".to_string(),
        },
        cleanup: "drop the temporary database".to_string(),
        sensitivity: Sensitivity::LocalOnly,
        source: None,
        preconditions: Vec::new(),
        counterfactual: None,
    };
    let uplift = tier == EvidenceTier::Uplift;
    ExperimentEvidence {
        version: EXPERIMENT_EVIDENCE_VERSION,
        tier,
        inputs: ExperimentInputs {
            candidate_identity: candidate.content_identity(),
            assignment_identity: Some(assignment.identity()),
            source_revision: "aaa".to_string(),
            model: uplift.then(|| "ornith-1-5-35b-a3b-gguf".to_string()),
            runtime: None,
            settings_digest: None,
            seed: uplift.then_some(7),
            budgets_digest: None,
            tool_versions: Default::default(),
            validation_profile: None,
            verifier: None,
        },
        assignment: Some(assignment),
        verdict,
        reasons: if verdict.requires_reason() {
            vec![VerdictReason::FixtureUnavailable]
        } else {
            Vec::new()
        },
        injection: uplift.then_some(InjectionProof {
            mode: InjectionMode::Forced,
            assertion_passed: verdict != LabVerdict::InvalidExperiment,
        }),
        receipt: uplift.then(|| ImportedReceipt::new("localbench-uplift-v2", "{}")),
        arms: Vec::new(),
        provenance: ExperimentProvenance {
            producer: "lab".to_string(),
            produced_at: 1_789_000_000,
        },
        limitations: Vec::new(),
    }
}

#[test]
fn a_revision_keeps_the_superseded_versions_results_and_marks_them_stale() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let session = SessionId::new("session");

    let first = candidate(&[fact("repo@aaa#migrate.log", "sha256:01")]);
    let tested =
        first
            .clone()
            .with_experiment(result(&first, EvidenceTier::Logic, LabVerdict::Valid));
    queue.enqueue_candidates(&session, &[tested]).unwrap();

    // The lesson is re-derived from a second observation. Same sentence.
    let revision = candidate(&[
        fact("repo@aaa#migrate.log", "sha256:01"),
        fact("repo@bbb#boot.log", "sha256:02"),
    ]);
    queue.enqueue_candidates(&session, &[revision]).unwrap();

    let items = queue.list().unwrap();
    assert_eq!(items.len(), 1);
    let stored = &items[0].candidate;

    assert_eq!(stored.evidence().len(), 2, "the row holds the revision");
    assert_eq!(
        stored.experiments.len(),
        1,
        "the earlier result is kept rather than erased"
    );
    let carried = &stored.experiments[0];
    assert!(
        carried.is_stale_for(stored),
        "and it no longer describes the row"
    );
    assert!(carried
        .validate(stored)
        .unwrap_err()
        .contains(&ExperimentViolation::StaleCandidate));
}

#[test]
fn resubmitting_a_candidate_with_a_new_result_keeps_the_result() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let session = SessionId::new("session");
    let bare = candidate(&[fact("repo@aaa#migrate.log", "sha256:01")]);

    queue.enqueue_candidates(&session, &[bare.clone()]).unwrap();

    // Results are not identity, so this is a restatement of the same lesson —
    // which must not be the reason the new result is lost.
    let with_result =
        bare.clone()
            .with_experiment(result(&bare, EvidenceTier::Replay, LabVerdict::Valid));
    queue.enqueue_candidates(&session, &[with_result]).unwrap();

    let items = queue.list().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].candidate.experiments.len(), 1);
    assert!(items[0].candidate.revises.is_none(), "still not a revision");
    assert_eq!(
        items[0].candidate.experiments[0].validate(&items[0].candidate),
        Ok(())
    );

    // Submitting the identical result again adds nothing.
    let again =
        bare.clone()
            .with_experiment(result(&bare, EvidenceTier::Replay, LabVerdict::Valid));
    queue.enqueue_candidates(&session, &[again]).unwrap();
    assert_eq!(queue.list().unwrap()[0].candidate.experiments.len(), 1);
}

/// Review mode reads no lab result (D-LM-0048), for every verdict a lab can
/// produce. A candidate carrying all of them — including `Supported` — reaches
/// the same automatic decision as one carrying none. Evidence informs a
/// reviewer; it never becomes the thing that decides.
#[test]
fn no_verdict_changes_an_automatic_review_decision() {
    let facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    let base = candidate(&facts);
    let every_verdict = [
        (EvidenceTier::Logic, LabVerdict::Valid),
        (EvidenceTier::Logic, LabVerdict::Invalid),
        (EvidenceTier::Replay, LabVerdict::NotExecutable),
        (EvidenceTier::Uplift, LabVerdict::Supported),
        (EvidenceTier::Uplift, LabVerdict::Inconclusive),
        (EvidenceTier::Uplift, LabVerdict::InvalidExperiment),
    ];
    let mut loaded = base.clone();
    for (tier, verdict) in every_verdict {
        let evidence = result(&base, tier, verdict);
        assert_eq!(
            evidence.validate(&base),
            Ok(()),
            "{verdict:?} fixture is sound"
        );
        loaded = loaded.with_experiment(evidence);
    }

    for mode in ["trusted", "automatic"] {
        let outcome = |candidate: &CandidateLesson| {
            let dir = project(mode);
            let queue = ReviewQueue::open_project(dir.path()).unwrap();
            queue
                .enqueue_candidates(&SessionId::new("session"), &[candidate.clone()])
                .unwrap();
            let report = ReviewModeProcessor::apply_project(dir.path()).unwrap();
            let items = queue.list().unwrap();
            let states: Vec<ReviewState> = items.iter().map(|item| item.state.clone()).collect();
            let notes: Vec<Option<String>> = items
                .iter()
                .map(|item| {
                    item.candidate
                        .review_annotation
                        .as_ref()
                        .map(|a| a.notes.clone())
                })
                .collect();
            (
                report.annotated,
                report.accepted,
                report.manual,
                states,
                notes,
            )
        };

        let bare_outcome = outcome(&base);
        // Guard against a vacuous pass: if the bare candidate were never
        // auto-accepted, "the same decision" would hold without testing anything.
        assert_eq!(
            bare_outcome.1, 1,
            "{mode}: the bare candidate must auto-accept for this comparison to mean anything"
        );
        assert_eq!(
            outcome(&loaded),
            bare_outcome,
            "{mode}: a supported result must not be able to promote, nor a neutral one block"
        );
    }
}

/// The one verdict automation reads, and only to withhold: a lesson a run
/// found harmful waits for a person in every mode.
#[test]
fn a_harmful_result_holds_a_lesson_for_a_person_in_every_mode() {
    let facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    let base = candidate(&facts);
    let harmful = base.clone().with_experiment(result(
        &base,
        EvidenceTier::Uplift,
        LabVerdict::Contradicted,
    ));
    assert!(harmful.has_harmful_result());
    assert!(!base.has_harmful_result());

    for mode in ["trusted", "automatic"] {
        let dir = project(mode);
        let queue = ReviewQueue::open_project(dir.path()).unwrap();
        queue
            .enqueue_candidates(&SessionId::new("session"), &[harmful.clone()])
            .unwrap();
        let report = ReviewModeProcessor::apply_project(dir.path()).unwrap();
        let item = queue.list().unwrap().remove(0);
        assert_eq!(report.accepted, 0, "{mode}");
        assert_eq!(report.manual, 1, "{mode}");
        assert_eq!(item.state, ReviewState::Pending, "{mode}");
        let notes = item.candidate.review_annotation.unwrap().notes;
        assert!(notes.contains("made results worse"), "{mode}: {notes}");
        assert!(MemoryPersistence::open_project(dir.path())
            .unwrap()
            .list_memory()
            .unwrap()
            .is_empty());
    }

    // A harmful result about an earlier version of the lesson is not about this
    // one: it is shown as stale and holds nothing.
    let other = CandidateLesson::new(
        LessonId::new("lesson-0"),
        "Start the server and let it migrate itself.",
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    let stale = base.clone().with_experiment(result(
        &other,
        EvidenceTier::Uplift,
        LabVerdict::Contradicted,
    ));
    assert!(!stale.has_harmful_result());
}

#[test]
fn a_corrupt_result_in_a_stored_row_is_an_error_not_an_empty_row() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let bare = candidate(&[fact("repo@aaa#migrate.log", "sha256:01")]);
    let tested =
        bare.clone()
            .with_experiment(result(&bare, EvidenceTier::Logic, LabVerdict::Valid));
    queue
        .enqueue_candidates(&SessionId::new("session"), &[tested])
        .unwrap();

    let database = dir.path().join(".localmind").join("localmind.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let json: String = connection
        .query_row("SELECT candidate_json FROM review_items", [], |row| {
            row.get(0)
        })
        .unwrap();
    connection
        .execute(
            "UPDATE review_items SET candidate_json = ?1",
            [json.replace("\"Valid\"", "\"Plausible\"")],
        )
        .unwrap();
    drop(connection);

    assert!(
        queue.list().is_err(),
        "a result that cannot be read must surface, not load as a candidate with no results"
    );
}

/// What an older LocalMind does with a record written by this one, pinned
/// rather than assumed. It reads the row — unknown fields are ignored — and if it
/// *rewrites* the row, the new fields are gone.
#[test]
fn an_older_binary_reads_new_records_but_a_rewrite_drops_their_new_fields() {
    /// The v5.0.0 on-disk shape of a candidate, before hindsight, lineage and
    /// experiment evidence existed.
    #[derive(Deserialize, Serialize)]
    struct CandidateBeforeLessonLab {
        id: String,
        summary: String,
        rationale: Option<String>,
        category: serde_json::Value,
        confidence: f32,
        evidence: Vec<serde_json::Value>,
        related_files: Vec<String>,
        related_entities: Vec<String>,
        suggested_destination: serde_json::Value,
        suggested_action: serde_json::Value,
        validation_status: serde_json::Value,
        #[serde(default)]
        review_annotation: Option<serde_json::Value>,
        #[serde(default)]
        tool_use: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evidence_text: Option<String>,
        #[serde(default)]
        requires_edit_before_promotion: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    }

    let bare = candidate(&[fact("repo@aaa#migrate.log", "sha256:01")]);
    let current = bare
        .clone()
        .revising("cnd-00000000000000000000000000000000")
        .with_experiment(result(&bare, EvidenceTier::Logic, LabVerdict::Valid));
    let written = serde_json::to_string(&current).unwrap();

    let read_by_older: CandidateBeforeLessonLab =
        serde_json::from_str(&written).expect("an older binary must still read a newer record");
    assert_eq!(read_by_older.summary, SUMMARY);

    let rewritten_by_older = serde_json::to_string(&read_by_older).unwrap();
    let back: CandidateLesson = serde_json::from_str(&rewritten_by_older).unwrap();
    assert!(
        back.experiments.is_empty(),
        "results do not survive a downgrade rewrite"
    );
    assert!(back.revises.is_none(), "nor does lineage");
    assert_eq!(back.summary(), current.summary(), "the lesson itself does");
}

// ---- review actions and lineage -------------------------------------------

fn tested_item(dir: &std::path::Path, verdict: LabVerdict) -> (ReviewQueue, CandidateLesson) {
    let queue = ReviewQueue::open_project(dir).unwrap();
    let bare = candidate(&[fact("repo@aaa#migrate.log", "sha256:01")]);
    let tested = bare
        .clone()
        .with_experiment(result(&bare, EvidenceTier::Uplift, verdict));
    queue
        .enqueue_candidates(&SessionId::new("session"), &[tested.clone()])
        .unwrap();
    (queue, tested)
}

fn id(text: &str) -> ReviewItemId {
    ReviewItemId::new(text)
}

fn decide(queue: &ReviewQueue, item: &str, action: ReviewAction) -> ReviewState {
    queue
        .decide(ReviewDecision {
            item_id: id(item),
            action,
            reviewer: "reviewer".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: None,
            evidence: Vec::new(),
        })
        .unwrap()
        .state
}

const REWRITE: &str = "Run the migration and check its exit code before starting the server.";

#[test]
fn a_rewrite_is_a_new_untested_lesson_and_the_original_stays_as_history() {
    let dir = project("manual");
    let (queue, tested) = tested_item(dir.path(), LabVerdict::Supported);

    let outcome = queue
        .rewrite(
            &id("lesson-1"),
            &LessonRevision::of_summary(REWRITE),
            "ada",
            Some("the exit code is the point".to_string()),
        )
        .unwrap();

    // The original: closed, never promotable, exactly as it was.
    let original = &outcome.original;
    assert_eq!(original.state, ReviewState::Merged);
    assert_eq!(original.reviewer_action.as_deref(), Some("revised_into"));
    assert_eq!(original.reviewer.as_deref(), Some("ada"));
    assert_eq!(
        original.candidate, tested,
        "the original draft is untouched"
    );
    assert_eq!(original.candidate.experiments.len(), 1);
    assert_eq!(original.replacement_summary.as_deref(), Some(REWRITE));
    assert_eq!(original.descendants, vec![id("lesson-1-r1")]);

    // The revision: accepted under the reviewer's name, linked back, untested.
    let revised = &outcome.revised;
    assert_eq!(revised.id, id("lesson-1-r1"));
    assert_eq!(revised.state, ReviewState::Edited);
    assert_eq!(revised.reviewer.as_deref(), Some("ada"));
    assert_eq!(revised.note.as_deref(), Some("the exit code is the point"));
    assert_eq!(revised.candidate.summary(), REWRITE);
    assert_eq!(
        revised.candidate.revises.as_deref(),
        Some(tested.content_identity().as_str())
    );
    assert!(
        revised.candidate.experiments.is_empty(),
        "a green result is never inherited"
    );
    assert!(tested.experiments[0].is_stale_for(&revised.candidate));
    assert_eq!(
        queue.parent_of(&revised.id).unwrap().unwrap().id,
        id("lesson-1")
    );

    // Promotion from either id writes the rewritten text, once.
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    let entry = persistence.promote_review_item(&id("lesson-1")).unwrap();
    assert_eq!(entry.id.as_str(), "lesson-1-r1");
    assert_eq!(entry.body, REWRITE);
    let memory = persistence.list_memory().unwrap();
    assert_eq!(memory.len(), 1);
    assert_eq!(memory[0].body, REWRITE);

    // The audit names both sides of the change.
    persistence.record_review_item_audit(original).unwrap();
    persistence.record_review_item_audit(revised).unwrap();
    let audit: Vec<String> = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .map(|record| record.metadata_json)
        .collect();
    assert!(audit
        .iter()
        .any(|row| row.contains("revised_into") && row.contains("lesson-1-r1")));
    assert!(audit
        .iter()
        .any(|row| row.contains(&tested.content_identity()) && row.contains("\"edit\"")));
}

#[test]
fn an_edit_decision_is_a_rewrite_whoever_sends_it() {
    let dir = project("manual");
    let (queue, _) = tested_item(dir.path(), LabVerdict::Supported);

    // The path every existing caller uses: CLI, web review and adapters all
    // send an `Edit` decision with replacement text.
    let item = queue
        .decide(ReviewDecision {
            item_id: id("lesson-1"),
            action: ReviewAction::Edit,
            reviewer: "ui".to_string(),
            decided_at: None,
            note: None,
            replacement_summary: Some(REWRITE.to_string()),
            evidence: Vec::new(),
        })
        .unwrap();

    assert_eq!(item.id, id("lesson-1-r1"));
    assert!(item.candidate.experiments.is_empty());
    let original = queue.get(&id("lesson-1")).unwrap().unwrap();
    assert_eq!(original.state, ReviewState::Merged);
    assert_eq!(original.candidate.summary(), SUMMARY);
}

#[test]
fn a_rewrite_can_correct_the_analysis_and_refuses_an_empty_change() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let facts = [fact("repo@aaa#migrate.log", "sha256:01")];
    let draft = HindsightDraft::new("Start the server", "It crashed on a missing table")
        .with_hypothesis(CausalHypothesis {
            claim: "the server started before the migration".to_string(),
            evidence_ids: vec![facts[0].id.clone()],
            confidence: Confidence::new(0.6).unwrap(),
        })
        .with_proposed_lesson(SUMMARY);
    let with_hindsight = candidate(&facts).with_hindsight(draft);
    queue
        .enqueue_candidates(&SessionId::new("session"), &[with_hindsight.clone()])
        .unwrap();

    let refuse =
        |revision: LessonRevision| match queue.rewrite(&id("lesson-1"), &revision, "ada", None) {
            Err(ReviewQueueError::Revision { source, .. }) => source,
            other => panic!("expected a refused revision, got {other:?}"),
        };
    assert_eq!(refuse(LessonRevision::default()), RevisionError::Unchanged);
    assert_eq!(
        refuse(LessonRevision::of_summary(SUMMARY)),
        RevisionError::Unchanged
    );
    assert_eq!(
        refuse(LessonRevision::of_summary("  ")),
        RevisionError::Empty { field: "summary" }
    );
    assert_eq!(queue.list().unwrap().len(), 1, "a refusal writes nothing");

    // The sentence stays; the diagnosis under it changes. That alone is a new
    // lesson with a new identity.
    let outcome = queue
        .rewrite(
            &id("lesson-1"),
            &LessonRevision {
                cause: Some("the migration failed silently and nothing checked it".to_string()),
                applicability: Some("services that migrate at boot".to_string()),
                intervention: Some("fail the boot when the migration exits non-zero".to_string()),
                ..LessonRevision::default()
            },
            "ada",
            None,
        )
        .unwrap();
    let hindsight = outcome.revised.candidate.hindsight.clone().unwrap();
    assert_eq!(outcome.revised.candidate.summary(), SUMMARY);
    assert_eq!(
        hindsight.hypotheses[0].claim,
        "the migration failed silently and nothing checked it"
    );
    assert_eq!(
        hindsight.hypotheses[0].evidence_ids,
        vec![facts[0].id.clone()],
        "the cited facts stay"
    );
    assert_eq!(
        hindsight.applicability.as_deref(),
        Some("services that migrate at boot")
    );
    assert_eq!(outcome.revised.candidate.validate_hindsight(), Ok(()));
    assert_ne!(
        outcome.revised.candidate.content_identity(),
        with_hindsight.content_identity()
    );
    assert_eq!(
        outcome.original.candidate.hindsight, with_hindsight.hindsight,
        "the original draft is preserved"
    );

    // A lesson with no hindsight has no analysis to correct.
    let bare = CandidateLesson::new(
        LessonId::new("lesson-2"),
        "Pin the toolchain in CI.",
        LessonCategory::Process,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    queue
        .enqueue_candidates(&SessionId::new("session"), &[bare])
        .unwrap();
    let refused = queue.rewrite(
        &id("lesson-2"),
        &LessonRevision {
            cause: Some("anything".to_string()),
            ..LessonRevision::default()
        },
        "ada",
        None,
    );
    assert!(matches!(
        refused,
        Err(ReviewQueueError::Revision {
            source: RevisionError::NoHindsight { field: "cause" },
            ..
        })
    ));
}

#[test]
fn a_split_makes_pending_untested_parts_and_closes_the_original() {
    let dir = project("manual");
    let (queue, tested) = tested_item(dir.path(), LabVerdict::Supported);
    let parts = [
        LessonRevision::of_summary("Run the database migration before the server starts."),
        LessonRevision::of_summary("Fail the boot when a migration exits non-zero."),
    ];

    for bad in [
        vec![parts[0].clone()],
        vec![parts[0].clone(), parts[0].clone()],
        vec![parts[0].clone(), LessonRevision::default()],
    ] {
        assert!(matches!(
            queue.split(&id("lesson-1"), &bad, "ada", None),
            Err(ReviewQueueError::InvalidSplit { .. })
        ));
    }
    assert_eq!(
        queue.list().unwrap().len(),
        1,
        "a refused split writes nothing"
    );

    let outcome = queue.split(&id("lesson-1"), &parts, "ada", None).unwrap();

    assert_eq!(outcome.original.state, ReviewState::Merged);
    assert_eq!(
        outcome.original.reviewer_action.as_deref(),
        Some("split_into")
    );
    assert_eq!(outcome.original.candidate, tested);
    assert_eq!(
        outcome.original.descendants,
        vec![id("lesson-1-s1"), id("lesson-1-s2")]
    );
    assert_eq!(outcome.parts.len(), 2);
    for part in &outcome.parts {
        assert_eq!(
            part.state,
            ReviewState::Pending,
            "each part gets its own decision"
        );
        assert!(part.reviewer.is_none());
        assert!(part.candidate.experiments.is_empty());
        assert_eq!(
            part.candidate.revises.as_deref(),
            Some(tested.content_identity().as_str())
        );
    }

    // Nothing was promoted by splitting, and the original never can be.
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    for item in ["lesson-1", "lesson-1-s1"] {
        assert!(matches!(
            persistence.promote_review_item(&id(item)),
            Err(MemoryPersistenceError::ReviewItemNotAccepted { .. })
        ));
    }
    assert!(persistence.list_memory().unwrap().is_empty());
}

#[test]
fn a_decided_item_is_history_for_every_action() {
    for (label, close) in [
        ("rejected", ReviewAction::Reject),
        ("ignored", ReviewAction::IgnoreSimilar),
    ] {
        let dir = project("manual");
        let (queue, tested) = tested_item(dir.path(), LabVerdict::Supported);
        decide(&queue, "lesson-1", close);

        assert!(
            matches!(
                queue.rewrite(
                    &id("lesson-1"),
                    &LessonRevision::of_summary(REWRITE),
                    "ada",
                    None
                ),
                Err(ReviewQueueError::NotOpen { .. })
            ),
            "{label}"
        );
        assert!(matches!(
            queue.split(
                &id("lesson-1"),
                &[
                    LessonRevision::of_summary("One part of it."),
                    LessonRevision::of_summary("Another part of it."),
                ],
                "ada",
                None
            ),
            Err(ReviewQueueError::NotOpen { .. })
        ));
        // Its content cannot be swapped underneath the decision either…
        let other = tested
            .revised(
                LessonId::new("lesson-1"),
                &LessonRevision::of_summary(REWRITE),
            )
            .unwrap();
        assert!(matches!(
            queue.replace_candidate(&id("lesson-1"), &other),
            Err(ReviewQueueError::NotOpen { .. })
        ));
        // …while a result about it can still be recorded on it.
        let more =
            tested
                .clone()
                .with_experiment(result(&tested, EvidenceTier::Logic, LabVerdict::Valid));
        queue.replace_candidate(&id("lesson-1"), &more).unwrap();
        assert_eq!(queue.list().unwrap().len(), 1);
    }

    // A rewritten item is decided too: it cannot be rewritten or split again,
    // and an accepted item cannot be split.
    let dir = project("manual");
    let (queue, _) = tested_item(dir.path(), LabVerdict::Supported);
    queue
        .rewrite(
            &id("lesson-1"),
            &LessonRevision::of_summary(REWRITE),
            "ada",
            None,
        )
        .unwrap();
    for item in ["lesson-1", "lesson-1-r1"] {
        assert!(matches!(
            queue.rewrite(
                &id(item),
                &LessonRevision::of_summary("Yet another wording of the lesson."),
                "ada",
                None
            ),
            Err(ReviewQueueError::NotOpen { .. })
        ));
    }

    // A deferred item is still open.
    let dir = project("manual");
    let (queue, _) = tested_item(dir.path(), LabVerdict::Supported);
    assert_eq!(
        decide(&queue, "lesson-1", ReviewAction::MarkTemporary),
        ReviewState::Deferred
    );
    queue
        .rewrite(
            &id("lesson-1"),
            &LessonRevision::of_summary(REWRITE),
            "ada",
            None,
        )
        .unwrap();
}

#[test]
fn rewriting_a_lesson_that_is_already_memory_retires_the_old_memory_visibly() {
    let dir = project("manual");
    let (queue, _) = tested_item(dir.path(), LabVerdict::Supported);
    let persistence = MemoryPersistence::open_project(dir.path()).unwrap();
    assert_eq!(
        decide(&queue, "lesson-1", ReviewAction::Accept),
        ReviewState::Accepted
    );
    persistence.promote_review_item(&id("lesson-1")).unwrap();

    let outcome = queue
        .rewrite(
            &id("lesson-1"),
            &LessonRevision::of_summary(REWRITE),
            "ada",
            None,
        )
        .unwrap();
    let entry = persistence
        .promote_review_item(&outcome.revised.id)
        .unwrap();

    assert_eq!(entry.supersedes.len(), 1);
    assert_eq!(entry.supersedes[0].as_str(), "lesson-1");
    // Retrieval serves only the rewrite; the old wording survives in the audit.
    let memory = persistence.list_memory().unwrap();
    assert_eq!(memory.len(), 1);
    assert_eq!(memory[0].memory_id.as_str(), "lesson-1-r1");
    assert_eq!(memory[0].body, REWRITE);
    let retired = persistence
        .audit_records()
        .unwrap()
        .into_iter()
        .find(|record| record.metadata_json.contains("superseded_by"))
        .expect("the retirement is audited");
    assert_eq!(retired.subject, "lesson-1");
    assert!(
        retired.metadata_json.contains(SUMMARY),
        "{}",
        retired.metadata_json
    );
}

/// A lab result is evidence for a reviewer. No result — not even a supported
/// one — accepts or promotes a lesson, through the queue, a review mode, or
/// promotion called directly.
#[test]
fn a_supported_result_cannot_promote_itself_through_any_entry_point() {
    for mode in ["manual", "assisted"] {
        let dir = project(mode);
        let (queue, tested) = tested_item(dir.path(), LabVerdict::Supported);
        let persistence = MemoryPersistence::open_project(dir.path()).unwrap();

        ReviewModeProcessor::apply_project(dir.path()).unwrap();
        assert_eq!(
            queue.get(&id("lesson-1")).unwrap().unwrap().state,
            ReviewState::Pending,
            "{mode}"
        );
        assert!(matches!(
            persistence.promote_review_item(&id("lesson-1")),
            Err(MemoryPersistenceError::ReviewItemNotAccepted { .. })
        ));

        // Recording another supported result on the row changes nothing either.
        let again = tested.clone().with_experiment(result(
            &tested,
            EvidenceTier::Replay,
            LabVerdict::Valid,
        ));
        queue
            .enqueue_candidates(&SessionId::new("session"), &[again])
            .unwrap();
        assert_eq!(
            queue.get(&id("lesson-1")).unwrap().unwrap().state,
            ReviewState::Pending
        );
        assert!(persistence.list_memory().unwrap().is_empty(), "{mode}");
    }

    // A descendant link cannot be forged onto an item that does not exist.
    let dir = project("manual");
    let (queue, _) = tested_item(dir.path(), LabVerdict::Supported);
    let forged = queue.decide(ReviewDecision {
        item_id: id("lesson-1"),
        action: ReviewAction::RevisedInto(id("nowhere")),
        reviewer: "x".to_string(),
        decided_at: None,
        note: None,
        replacement_summary: None,
        evidence: Vec::new(),
    });
    assert!(matches!(
        forged,
        Err(ReviewQueueError::MissingMergeTarget { .. })
    ));
}

/// A lesson nobody tested is the ordinary case and reviews exactly as before.
#[test]
fn an_untested_lesson_keeps_the_ordinary_review_flow() {
    let dir = project("manual");
    let queue = ReviewQueue::open_project(dir.path()).unwrap();
    let bare = candidate(&[fact("repo@aaa#migrate.log", "sha256:01")]);
    queue
        .enqueue_candidates(&SessionId::new("session"), &[bare])
        .unwrap();
    let item = queue.get(&id("lesson-1")).unwrap().unwrap();
    assert!(item.descendants.is_empty());
    assert!(item.candidate.revises.is_none());

    assert_eq!(
        decide(&queue, "lesson-1", ReviewAction::Accept),
        ReviewState::Accepted
    );
    let entry = MemoryPersistence::open_project(dir.path())
        .unwrap()
        .promote_review_item(&id("lesson-1"))
        .unwrap();
    assert_eq!(entry.body, SUMMARY);
    assert!(entry.supersedes.is_empty());
}
