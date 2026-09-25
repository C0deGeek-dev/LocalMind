//! Experiment evidence in the review queue.
//!
//! The queue is where evidence would quietly go wrong: a revision that sheds its
//! test history, a re-submission that drops a new result, a corrupt row read as
//! an empty one, or a verdict that starts steering review. Each has a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, Confidence, EvidenceKind, EvidenceRef, EvidenceTier, ExperimentEvidence,
    ExperimentInputs, ExperimentProvenance, ExperimentViolation, FixtureRef, ImportedReceipt,
    InjectionMode, InjectionProof, LabVerdict, LessonAssignment, LessonCategory, LessonId,
    OracleOrigin, OracleRef, ReviewState, Sensitivity, SessionId, SuggestedAction, VerdictReason,
    VerifierRef, EXPERIMENT_EVIDENCE_VERSION, LESSON_ASSIGNMENT_VERSION,
};
use localmind_store::{ReviewModeProcessor, ReviewQueue};
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

/// D020, for every verdict a lab can produce. A candidate carrying all of them
/// — including `Supported` — reaches the same automatic decision as one carrying
/// none. Evidence informs a reviewer; it never becomes the thing that decides.
#[test]
fn no_verdict_changes_an_automatic_review_decision() {
    let facts = vec![fact("repo@aaa#migrate.log", "sha256:01")];
    let base = candidate(&facts);
    let every_verdict = [
        (EvidenceTier::Logic, LabVerdict::Valid),
        (EvidenceTier::Logic, LabVerdict::Invalid),
        (EvidenceTier::Replay, LabVerdict::NotExecutable),
        (EvidenceTier::Uplift, LabVerdict::Supported),
        (EvidenceTier::Uplift, LabVerdict::Contradicted),
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
            "{mode}: a supported result must not be able to promote, nor any result block"
        );
    }
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
