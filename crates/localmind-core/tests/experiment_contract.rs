//! The experiment-evidence contract as a consumer sees it.
//!
//! What is being defended: a result is bound to exactly the inputs that
//! produced it, changes to those inputs make it stale rather than silently
//! wrong, and no tier can award a lesson support it could not have measured.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    ArmRecord, CandidateLesson, Confidence, EvidenceKind, EvidenceRef, EvidenceTier,
    ExperimentEvidence, ExperimentInputs, ExperimentProvenance, ExperimentViolation, FixtureRef,
    ImportedReceipt, InjectionMode, InjectionProof, LabVerdict, LessonAssignment, LessonCategory,
    LessonId, LogRef, OracleOrigin, OracleRef, Sensitivity, SuggestedAction, VerdictReason,
    VerifierRef, EXPERIMENT_EVIDENCE_VERSION, LAB_LOG_RETENTION_DAYS, LESSON_ASSIGNMENT_VERSION,
    MAX_OBSERVATION_CHARS,
};

const DAY: i64 = 86_400;

fn candidate() -> CandidateLesson {
    CandidateLesson::new(
        LessonId::new("lesson-1"),
        "Give every fixture store its own temporary root.",
        LessonCategory::TestingStrategy,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(EvidenceRef::identified(
        EvidenceKind::TestOutput,
        "two cases failed",
        "session:a",
        "repo@9f1c2d#run.log",
        "sha256:01",
    ))
}

fn assignment(candidate: &CandidateLesson) -> LessonAssignment {
    LessonAssignment {
        version: LESSON_ASSIGNMENT_VERSION,
        candidate_identity: candidate.content_identity(),
        task: "make the review-queue suite pass with parallel cases".to_string(),
        task_evidence: candidate.evidence().iter().map(|e| e.id.clone()).collect(),
        oracle: OracleRef {
            locator: "repo@9f1c2d#tests/review_queue.rs".to_string(),
            content_hash: "sha256:oracle".to_string(),
            origin: OracleOrigin::Preexisting,
        },
        fixture: FixtureRef {
            locator: "repo@9f1c2d".to_string(),
            content_hash: "sha256:fixture".to_string(),
        },
        initial_state: "clean checkout at 9f1c2d".to_string(),
        allowed_tools: vec!["read_file".to_string(), "write_file".to_string()],
        success_observations: vec!["all review_queue tests pass".to_string()],
        failure_observations: vec!["a test reads another test's rows".to_string()],
        verifier: VerifierRef {
            name: "cargo-test".to_string(),
            version: "1".to_string(),
        },
        cleanup: "remove the temporary worktree".to_string(),
        sensitivity: Sensitivity::Redacted,
        source: None,
        preconditions: Vec::new(),
        counterfactual: None,
    }
}

fn inputs(candidate: &CandidateLesson, assignment: &LessonAssignment) -> ExperimentInputs {
    ExperimentInputs {
        candidate_identity: candidate.content_identity(),
        assignment_identity: Some(assignment.identity()),
        source_revision: "9f1c2d".to_string(),
        model: None,
        runtime: None,
        settings_digest: None,
        seed: None,
        budgets_digest: Some("budget:v1".to_string()),
        tool_versions: [("write_file".to_string(), "2".to_string())].into(),
        validation_profile: Some("default".to_string()),
        verifier: Some(assignment.verifier.clone()),
    }
}

fn logic_valid(candidate: &CandidateLesson) -> ExperimentEvidence {
    let assignment = assignment(candidate);
    ExperimentEvidence {
        version: EXPERIMENT_EVIDENCE_VERSION,
        tier: EvidenceTier::Logic,
        inputs: inputs(candidate, &assignment),
        assignment: Some(assignment),
        verdict: LabVerdict::Valid,
        reasons: Vec::new(),
        injection: None,
        receipt: None,
        arms: vec![ArmRecord {
            arm: "logic".to_string(),
            attempts: 1,
            passed: 1,
            ..ArmRecord::default()
        }],
        provenance: ExperimentProvenance {
            producer: "localpilot-lab 1".to_string(),
            produced_at: 1_789_000_000,
        },
        limitations: vec!["replays a known trajectory; says nothing about the lesson".to_string()],
    }
}

fn uplift(candidate: &CandidateLesson, verdict: LabVerdict, passed: bool) -> ExperimentEvidence {
    ExperimentEvidence {
        tier: EvidenceTier::Uplift,
        verdict,
        injection: Some(InjectionProof {
            mode: InjectionMode::Forced,
            assertion_passed: passed,
        }),
        receipt: Some(ImportedReceipt::new(
            "localbench-uplift-v2",
            r#"{"task_set":"headroom-v1","uplift":"Uplift"}"#,
        )),
        ..logic_valid(candidate)
    }
}

fn violations(
    evidence: &ExperimentEvidence,
    candidate: &CandidateLesson,
) -> Vec<ExperimentViolation> {
    evidence.validate(candidate).err().unwrap_or_default()
}

#[test]
fn a_sound_logic_result_validates() {
    let candidate = candidate();
    assert_eq!(logic_valid(&candidate).validate(&candidate), Ok(()));
}

#[test]
fn each_tier_may_emit_only_its_own_verdicts() {
    use EvidenceTier::{Logic, Replay, Uplift};
    use LabVerdict::*;

    for (verdict, logic, replay, uplift) in [
        (Valid, true, true, false),
        (Invalid, true, true, false),
        (NotExecutable, true, true, false),
        (Supported, false, false, true),
        (Contradicted, false, false, true),
        (Inconclusive, false, false, true),
        (InvalidExperiment, true, true, true),
    ] {
        assert_eq!(verdict.permitted_for(Logic), logic, "{verdict:?} on Logic");
        assert_eq!(
            verdict.permitted_for(Replay),
            replay,
            "{verdict:?} on Replay"
        );
        assert_eq!(
            verdict.permitted_for(Uplift),
            uplift,
            "{verdict:?} on Uplift"
        );
    }
}

#[test]
fn replay_cannot_award_support_however_well_it_went() {
    let candidate = candidate();
    let replay = ExperimentEvidence {
        tier: EvidenceTier::Replay,
        verdict: LabVerdict::Supported,
        ..uplift(&candidate, LabVerdict::Supported, true)
    };

    assert!(violations(&replay, &candidate).contains(
        &ExperimentViolation::VerdictNotPermittedForTier {
            verdict: LabVerdict::Supported,
            tier: EvidenceTier::Replay,
        }
    ));
}

#[test]
fn a_supported_result_without_injection_proof_is_refused() {
    let candidate = candidate();

    let unproven = ExperimentEvidence {
        injection: None,
        ..uplift(&candidate, LabVerdict::Supported, true)
    };
    assert!(violations(&unproven, &candidate)
        .contains(&ExperimentViolation::EfficacyWithoutInjectionProof));

    let failed = uplift(&candidate, LabVerdict::Contradicted, false);
    assert!(violations(&failed, &candidate)
        .contains(&ExperimentViolation::EfficacyWithoutInjectionProof));

    assert_eq!(
        uplift(&candidate, LabVerdict::Supported, true).validate(&candidate),
        Ok(())
    );
}

#[test]
fn a_failed_injection_is_an_invalid_experiment_and_never_no_effect() {
    let candidate = candidate();

    let masquerading = uplift(&candidate, LabVerdict::Inconclusive, false);
    assert!(violations(&masquerading, &candidate).contains(
        &ExperimentViolation::FailedInjectionNotInvalid {
            verdict: LabVerdict::Inconclusive,
        }
    ));

    let honest = ExperimentEvidence {
        reasons: vec![VerdictReason::InjectionNotObserved],
        ..uplift(&candidate, LabVerdict::InvalidExperiment, false)
    };
    assert_eq!(honest.validate(&candidate), Ok(()));
}

#[test]
fn an_uplift_verdict_needs_its_receipt_and_the_receipt_must_be_intact() {
    let candidate = candidate();

    let missing = ExperimentEvidence {
        receipt: None,
        ..uplift(&candidate, LabVerdict::Inconclusive, true)
    };
    assert!(violations(&missing, &candidate)
        .contains(&ExperimentViolation::UpliftVerdictWithoutReceipt));

    let mut tampered = uplift(&candidate, LabVerdict::Supported, true);
    if let Some(receipt) = tampered.receipt.as_mut() {
        receipt.payload = receipt.payload.replace("Uplift", "Regression");
    }
    assert!(violations(&tampered, &candidate).contains(&ExperimentViolation::ReceiptDigestMismatch));
}

#[test]
fn a_changed_candidate_makes_the_result_stale() {
    let candidate = candidate();
    let evidence = logic_valid(&candidate);
    assert!(!evidence.is_stale_for(&candidate));

    // The lesson is revised after the run. The wording is untouched — the
    // evidence behind it changed — and the result no longer describes it.
    let revised = candidate.clone().with_evidence(EvidenceRef::identified(
        EvidenceKind::TestOutput,
        "a third run",
        "session:b",
        "repo@aaaaaa#run.log",
        "sha256:02",
    ));
    assert!(evidence.is_stale_for(&revised));
    assert!(violations(&evidence, &revised).contains(&ExperimentViolation::StaleCandidate));
}

#[test]
fn attaching_a_result_does_not_make_it_stale() {
    let candidate = candidate();
    let evidence = logic_valid(&candidate);
    let carrying = candidate.clone().with_experiment(evidence.clone());

    // If results were part of candidate identity, recording one would change
    // the identity it is bound to and every result would be stale on arrival.
    assert_eq!(candidate.content_identity(), carrying.content_identity());
    assert_eq!(evidence.validate(&carrying), Ok(()));
}

#[test]
fn identity_covers_inputs_and_ignores_what_varies_between_honest_reruns() {
    let candidate = candidate();
    let first = logic_valid(&candidate);

    let rerun = ExperimentEvidence {
        provenance: ExperimentProvenance {
            producer: "localpilot-lab 1".to_string(),
            produced_at: first.provenance.produced_at + DAY,
        },
        arms: vec![ArmRecord {
            arm: "logic".to_string(),
            attempts: 3,
            passed: 3,
            wall_ms: 91_000,
            ..ArmRecord::default()
        }],
        limitations: Vec::new(),
        ..first.clone()
    };
    assert_eq!(
        first.identity(),
        rerun.identity(),
        "timings are not identity"
    );

    for (field, mut changed) in [
        ("source revision", first.clone()),
        ("seed", first.clone()),
        ("tool version", first.clone()),
        ("verifier", first.clone()),
        ("budget", first.clone()),
    ] {
        match field {
            "source revision" => changed.inputs.source_revision = "000000".to_string(),
            "seed" => changed.inputs.seed = Some(7),
            "tool version" => {
                changed
                    .inputs
                    .tool_versions
                    .insert("write_file".to_string(), "3".to_string());
            }
            "verifier" => {
                changed.inputs.verifier = Some(VerifierRef {
                    name: "cargo-test".to_string(),
                    version: "2".to_string(),
                });
            }
            _ => changed.inputs.budgets_digest = Some("budget:v2".to_string()),
        }
        assert_ne!(first.identity(), changed.identity(), "{field} is identity");
    }
}

#[test]
fn a_fixture_or_oracle_changed_after_freezing_is_caught() {
    let candidate = candidate();

    // The assignment carried with the result no longer matches the identity
    // the result recorded — a swapped fixture or an edited oracle.
    let mut swapped = logic_valid(&candidate);
    if let Some(assignment) = swapped.assignment.as_mut() {
        assignment.fixture.content_hash = "sha256:another-fixture".to_string();
    }
    assert!(violations(&swapped, &candidate).contains(&ExperimentViolation::AssignmentMismatch));

    let mut edited = logic_valid(&candidate);
    if let Some(assignment) = edited.assignment.as_mut() {
        assignment.oracle.content_hash = "sha256:loosened".to_string();
    }
    assert!(violations(&edited, &candidate).contains(&ExperimentViolation::AssignmentMismatch));
}

#[test]
fn an_oracle_that_can_change_or_that_came_from_the_lesson_is_refused() {
    let candidate = candidate();

    let rebind = |mut evidence: ExperimentEvidence| {
        if let Some(assignment) = &evidence.assignment {
            evidence.inputs.assignment_identity = Some(assignment.identity());
        }
        evidence
    };

    let mut mutable = logic_valid(&candidate);
    if let Some(assignment) = mutable.assignment.as_mut() {
        assignment.oracle.content_hash = String::new();
    }
    let mutable = rebind(mutable);
    assert!(violations(&mutable, &candidate).contains(&ExperimentViolation::MutableOracle));

    let mut derived = logic_valid(&candidate);
    if let Some(assignment) = derived.assignment.as_mut() {
        assignment.oracle.origin = OracleOrigin::DerivedFromLesson;
    }
    let derived = rebind(derived);
    assert!(violations(&derived, &candidate).contains(&ExperimentViolation::OracleNotIndependent));

    // Reporting that finding is exactly what an Invalid verdict is for.
    let reported = ExperimentEvidence {
        verdict: LabVerdict::Invalid,
        reasons: vec![VerdictReason::OracleNotIndependent],
        ..derived
    };
    assert_eq!(reported.validate(&candidate), Ok(()));
}

#[test]
fn a_negative_result_must_say_why() {
    let candidate = candidate();
    let silent = ExperimentEvidence {
        verdict: LabVerdict::NotExecutable,
        assignment: None,
        ..logic_valid(&candidate)
    };

    assert!(
        violations(&silent, &candidate).contains(&ExperimentViolation::MissingReason {
            verdict: LabVerdict::NotExecutable,
        })
    );

    let explained = ExperimentEvidence {
        reasons: vec![VerdictReason::NotReplayable],
        inputs: ExperimentInputs {
            assignment_identity: None,
            ..silent.inputs.clone()
        },
        ..silent
    };
    assert_eq!(explained.validate(&candidate), Ok(()));
}

#[test]
fn a_verdict_about_an_assignment_must_carry_that_assignment() {
    let candidate = candidate();
    let orphan = ExperimentEvidence {
        assignment: None,
        ..logic_valid(&candidate)
    };

    assert!(
        violations(&orphan, &candidate).contains(&ExperimentViolation::MissingAssignment {
            verdict: LabVerdict::Valid,
        })
    );
}

#[test]
fn a_log_expires_after_thirty_days_and_a_future_capture_never_does() {
    let captured = 1_789_000_000;
    let log = LogRef {
        locator: ".localpilot/lab/run-1/check.log".to_string(),
        content_hash: "sha256:log".to_string(),
        bytes: 4_096,
        summary: "2 failed, 38 passed".to_string(),
        captured_at: captured,
    };
    let retention = i64::try_from(LAB_LOG_RETENTION_DAYS).unwrap() * DAY;

    assert!(!log.is_expired(captured));
    assert!(!log.is_expired(captured + retention));
    assert!(log.is_expired(captured + retention + 1));
    assert!(!log.is_expired(captured - DAY), "clock skew keeps a log");
}

#[test]
fn an_expired_log_does_not_invalidate_the_result_that_cites_it() {
    let candidate = candidate();
    let mut evidence = logic_valid(&candidate);
    evidence.arms[0].logs.push(LogRef {
        locator: ".localpilot/lab/run-1/check.log".to_string(),
        content_hash: "sha256:log".to_string(),
        bytes: 4_096,
        summary: "2 failed, 38 passed".to_string(),
        captured_at: 0,
    });

    assert!(evidence.arms[0].logs[0].is_expired(1_789_000_000));
    assert_eq!(evidence.validate(&candidate), Ok(()));
}

#[test]
fn bulky_output_is_refused_inline() {
    let candidate = candidate();
    let mut evidence = logic_valid(&candidate);
    evidence.arms[0].observations = vec!["x".repeat(localmind_core::MAX_OBSERVATION_CHARS + 1)];

    assert!(violations(&evidence, &candidate)
        .iter()
        .any(|violation| matches!(violation, ExperimentViolation::FieldTooLong { .. })));
}

#[test]
fn a_result_round_trips_and_a_candidate_without_results_gains_no_key() {
    let candidate = candidate();
    let evidence = uplift(&candidate, LabVerdict::Supported, true);
    let carrying = candidate.clone().with_experiment(evidence);

    let json = serde_json::to_string(&carrying).unwrap();
    let back: CandidateLesson = serde_json::from_str(&json).unwrap();
    assert_eq!(carrying, back);

    let bare = serde_json::to_string(&candidate).unwrap();
    assert!(!bare.contains("experiments"));
}

#[test]
fn a_corrupt_result_fails_to_load_instead_of_defaulting() {
    let candidate = candidate();
    let json = serde_json::to_string(&logic_valid(&candidate)).unwrap();

    for corruption in [
        json.replace("\"Valid\"", "\"Probably\""),
        json.replace("\"Logic\"", "\"Vibes\""),
        json.replace("\"version\":1", "\"version\":\"one\""),
    ] {
        assert!(
            serde_json::from_str::<ExperimentEvidence>(&corruption).is_err(),
            "a corrupt record must be an error, not a default"
        );
    }
}

#[test]
fn an_assignment_without_source_preconditions_or_counterfactual_keeps_its_identity() {
    let candidate = candidate();
    let plain = assignment(&candidate);
    let json = serde_json::to_value(&plain).unwrap();

    // Identity hashes the serialized form, so absent fields must stay absent:
    // an assignment frozen before they existed identifies exactly as it did.
    for key in ["source", "preconditions", "counterfactual"] {
        assert!(json.get(key).is_none(), "{key} is written only when set");
    }

    let mut sourced = plain.clone();
    sourced.source = Some(localmind_core::AssignmentSource::FailFixPair {
        base_revision: "9f1c2d".to_string(),
        fix_revision: "a1b2c3".to_string(),
    });
    let mut counterfactual = plain.clone();
    counterfactual.counterfactual =
        Some("had the migration been written first, the suite would have passed".to_string());
    let mut preconditions = plain.clone();
    preconditions.preconditions = vec!["the database is empty".to_string()];

    for changed in [sourced, counterfactual, preconditions] {
        assert_ne!(
            changed.identity(),
            plain.identity(),
            "what is tested is identity"
        );
    }
}

#[test]
fn an_assignment_is_checked_before_it_is_frozen() {
    let candidate = candidate();
    assert!(assignment(&candidate).validate().is_ok());

    let mut unsound = assignment(&candidate);
    unsound.oracle.content_hash = String::new();
    unsound.fixture.content_hash = " ".to_string();
    unsound.oracle.origin = OracleOrigin::DerivedFromLesson;
    unsound.counterfactual = Some("x".repeat(MAX_OBSERVATION_CHARS + 1));

    let violations = unsound.validate().unwrap_err();
    for expected in [
        ExperimentViolation::MutableOracle,
        ExperimentViolation::UnfrozenFixture,
        ExperimentViolation::OracleNotIndependent,
    ] {
        assert!(violations.contains(&expected), "{violations:?}");
    }
    assert!(violations.iter().any(|violation| matches!(
        violation,
        ExperimentViolation::FieldTooLong {
            field: "counterfactual",
            ..
        }
    )));
}

#[test]
fn reason_codes_round_trip_and_an_unknown_code_still_reads() {
    for reason in [
        VerdictReason::Preference,
        VerdictReason::HumanIntent,
        VerdictReason::UnverifiableStyle,
        VerdictReason::UnsafeAction,
        VerdictReason::NoTrustedSource,
        VerdictReason::OracleChangedByFix,
        VerdictReason::OracleNotIndependent,
        VerdictReason::Other("custom".to_string()),
    ] {
        let json = serde_json::to_string(&reason).unwrap();
        assert_eq!(
            serde_json::from_str::<VerdictReason>(&json).unwrap(),
            reason,
            "{json}"
        );
    }

    // A code written by a newer build opens in this one rather than failing.
    assert_eq!(
        serde_json::from_str::<VerdictReason>("\"SomeFutureCode\"").unwrap(),
        VerdictReason::Other("SomeFutureCode".to_string())
    );
    assert_eq!(
        serde_json::from_str::<VerdictReason>(r#"{"FutureTagged":"detail"}"#).unwrap(),
        VerdictReason::Other("FutureTagged: detail".to_string())
    );
}
