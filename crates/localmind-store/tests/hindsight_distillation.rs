//! Hindsight distillation: every reply validated, one repair at most, a
//! fallback that invents nothing, and an outcome the model does not choose.
//!
//! These tests deliver scripted replies. They prove the contract and the
//! control flow — what is sent, what is accepted, how many calls are made — and
//! say nothing about how well any real model fills the contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{EvidenceKind, EvidenceRef, HindsightOutcome, Observation};
use localmind_inference::ConstraintDisposition;
use localmind_store::{
    decide_outcome, DistillInput, DistillPlan, DistillReply, DistillStep, Distillation, Distiller,
    Incompleteness, InputGap, OutcomeReason, RequestPurpose, Strategy,
};

fn fact(label: &str, locator: &str) -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::ToolEvent,
        label,
        "session:fixture",
        locator,
        format!("sha256:{locator}"),
    )
}

fn failure(label: &str, locator: &str, signature: &str) -> EvidenceRef {
    fact(label, locator)
        .with_observation(Observation::Failure)
        .with_signature(signature)
}

fn success(label: &str, locator: &str, signature: &str) -> EvidenceRef {
    fact(label, locator)
        .with_observation(Observation::Success)
        .with_signature(signature)
}

fn correction(label: &str, locator: &str, excerpt: &str) -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::UserCorrection,
        label,
        "session:fixture",
        locator,
        format!("sha256:{locator}"),
    )
    .with_observation(Observation::Correction)
    .with_excerpt(excerpt)
}

fn input(facts: Vec<EvidenceRef>) -> DistillInput {
    DistillInput {
        facts,
        gaps: vec![InputGap::note(
            "session s1: 1 of 2 call(s) carry no verifier verdict",
        )],
        intended: "Make the test suite pass".to_string(),
        observed: "1 of 1 plan step(s) complete".to_string(),
    }
}

fn one_pass() -> DistillPlan {
    DistillPlan {
        strategy: Strategy::OnePass,
        attempt_schema: false,
        context_tokens: None,
    }
}

fn staged() -> DistillPlan {
    DistillPlan {
        strategy: Strategy::Staged,
        ..one_pass()
    }
}

fn text(content: &str) -> DistillReply {
    DistillReply::Text {
        content: content.to_string(),
        disposition: ConstraintDisposition::NotRequested,
    }
}

/// Drive a distillation with scripted replies, returning the result and every
/// request that was made.
fn run(
    input: DistillInput,
    plan: DistillPlan,
    replies: Vec<DistillReply>,
) -> (Distillation, Vec<localmind_store::DistillRequest>) {
    let mut distiller = Distiller::new(input, plan);
    let mut requests = Vec::new();
    let mut replies = replies.into_iter();
    let mut step = distiller.start();
    loop {
        match step {
            DistillStep::Ask(request) => {
                requests.push(request);
                let reply = replies
                    .next()
                    .expect("the distiller asked more than was scripted");
                step = distiller.reply(reply);
            }
            DistillStep::Done(result) => {
                assert!(replies.next().is_none(), "the distiller stopped early");
                return (*result, requests);
            }
        }
    }
}

/// A fixture run: a test failed on a missing migration, the migration was added,
/// the suite passed.
fn clear_cause() -> Vec<EvidenceRef> {
    vec![
        failure(
            "`run_shell` call `c1` failed (it ran and reported failure)",
            "c1",
            "run_shell:tests",
        ),
        success(
            "`write_file` call `c2` succeeded",
            "c2",
            "write_file:migration",
        ),
        success("`run_shell` call `c3` succeeded", "c3", "run_shell:tests"),
    ]
}

fn draft_json(facts: &[EvidenceRef], lesson: Option<&str>, intervention: Option<&str>) -> String {
    serde_json::json!({
        "version": 1,
        "intended_outcome": "Make the test suite pass",
        "observed_outcome": "The suite failed once, then passed after a migration was added",
        "hypotheses": [{
            "claim": "The users table did not exist because its migration had never been written",
            "evidence_ids": [facts[0].id.as_str(), facts[1].id.as_str()],
            "confidence": 0.7
        }],
        "intervention": intervention,
        "proposed_lesson": lesson,
        "suggested_outcome": "Candidate"
    })
    .to_string()
}

const LESSON: &str = "Write the schema migration before the test that reads the table";

#[test]
fn a_valid_reply_wrapped_in_prose_and_fences_is_accepted_in_one_call() {
    let facts = clear_cause();
    let reply = format!(
        "<think>\nweighing it\n</think>\nHere is the analysis:\n```json\n{}\n```\nHope that helps.",
        draft_json(&facts, Some(LESSON), Some("Add the migration first"))
    );

    let (result, requests) = run(input(facts), one_pass(), vec![text(&reply)]);

    assert_eq!(
        result.outcome,
        HindsightOutcome::Candidate,
        "{:?}",
        result.reasons
    );
    assert_eq!(result.trace.model_calls, 1);
    assert!(!result.trace.repair_spent && !result.trace.fallback);
    assert_eq!(requests.len(), 1);
    assert_eq!(result.draft.proposed_lesson.as_deref(), Some(LESSON));
}

#[test]
fn an_omitted_version_is_ours_to_fill_but_an_omitted_outcome_costs_the_repair() {
    let facts = clear_cause();
    let mut without_version: serde_json::Value =
        serde_json::from_str(&draft_json(&facts, Some(LESSON), None)).unwrap();
    without_version.as_object_mut().unwrap().remove("version");
    let (result, _) = run(
        input(facts.clone()),
        one_pass(),
        vec![text(&without_version.to_string())],
    );
    assert_eq!(result.outcome, HindsightOutcome::Candidate);
    assert!(!result.trace.repair_spent);

    let mut missing = without_version.clone();
    missing.as_object_mut().unwrap().remove("observed_outcome");
    let (result, requests) = run(
        input(facts.clone()),
        one_pass(),
        vec![
            text(&missing.to_string()),
            text(&draft_json(&facts, Some(LESSON), None)),
        ],
    );
    assert_eq!(result.outcome, HindsightOutcome::Candidate);
    assert!(result.trace.repair_spent);
    assert_eq!(result.trace.model_calls, 2);
    assert!(requests[1].repair);
    assert!(requests[1]
        .messages
        .last()
        .unwrap()
        .content
        .contains("observed_outcome"));
}

#[test]
fn truncated_json_twice_ends_in_a_fallback_that_invents_nothing() {
    let facts = clear_cause();
    let truncated =
        r#"{"intended_outcome": "Make the test suite pass", "observed_outcome": "The su"#;

    let (result, requests) = run(
        input(facts),
        one_pass(),
        vec![text(truncated), text(truncated)],
    );

    assert_eq!(result.outcome, HindsightOutcome::Malformed);
    assert_eq!(
        requests.len(),
        2,
        "one repair and then stop — never a third request"
    );
    assert!(result.trace.fallback && result.trace.repair_spent);
    assert!(result.draft.hypotheses.is_empty());
    assert_eq!(result.draft.proposed_lesson, None);
    assert_eq!(result.draft.intended_outcome, "Make the test suite pass");
    assert_eq!(
        result.draft.observed_outcome,
        "1 of 1 plan step(s) complete"
    );
    assert!(matches!(
        result.reasons.as_slice(),
        [OutcomeReason::MalformedAfterRepair { .. }]
    ));
}

#[test]
fn an_invented_id_is_refused_and_named_in_the_repair() {
    let facts = clear_cause();
    let invented = draft_json(&facts, Some(LESSON), None).replace(facts[1].id.as_str(), "ev-0000");

    let (result, requests) = run(
        input(facts),
        one_pass(),
        vec![text(&invented), text(&invented)],
    );

    assert_eq!(result.outcome, HindsightOutcome::Malformed);
    assert!(requests[1]
        .messages
        .last()
        .unwrap()
        .content
        .contains("ev-0000"));
    let OutcomeReason::MalformedAfterRepair { violations } = &result.reasons[0] else {
        panic!("{:?}", result.reasons);
    };
    assert!(violations.iter().any(|v| v.contains("never supplied")));
}

#[test]
fn unsupported_certainty_is_a_violation_not_a_stronger_claim() {
    let facts = clear_cause();
    let certain = serde_json::json!({
        "intended_outcome": "Make the test suite pass",
        "observed_outcome": "It passed",
        "hypotheses": [{ "claim": "The migration was missing", "evidence_ids": [facts[0].id.as_str()], "confidence": 0.97 }]
    })
    .to_string();
    let calibrated = certain.replace("0.97", "0.6");

    let (result, requests) = run(
        input(facts),
        one_pass(),
        vec![text(&certain), text(&calibrated)],
    );

    assert!(requests[1]
        .messages
        .last()
        .unwrap()
        .content
        .contains("confidence 0.97"));
    assert_eq!(result.draft.hypotheses[0].confidence.value(), 0.6);
    assert_eq!(
        result.outcome,
        HindsightOutcome::NoLesson,
        "a cause and no lesson"
    );
}

#[test]
fn several_causes_may_stand_together() {
    let facts = clear_cause();
    let reply = serde_json::json!({
        "intended_outcome": "Make the test suite pass",
        "observed_outcome": "It passed after a migration was added",
        "hypotheses": [
            { "claim": "The migration was never written", "evidence_ids": [facts[0].id.as_str(), facts[1].id.as_str()], "confidence": 0.6 },
            { "claim": "The test ran against a stale schema cache", "evidence_ids": [facts[0].id.as_str()], "confidence": 0.3 }
        ],
        "proposed_lesson": LESSON
    })
    .to_string();

    let (result, _) = run(input(facts), one_pass(), vec![text(&reply)]);

    assert_eq!(result.draft.hypotheses.len(), 2);
    assert_eq!(result.outcome, HindsightOutcome::Candidate);
}

#[test]
fn the_models_suggested_outcome_is_recorded_and_never_decides() {
    let facts = clear_cause();
    // The model calls it a candidate and proposes nothing: no lesson.
    let reply = draft_json(&facts, None, None);

    let (result, _) = run(input(facts), one_pass(), vec![text(&reply)]);

    assert_eq!(
        result.draft.suggested_outcome,
        Some(HindsightOutcome::Candidate)
    );
    assert_eq!(result.outcome, HindsightOutcome::NoLesson);
    assert_eq!(result.reasons, vec![OutcomeReason::NoLessonProposed]);
}

#[test]
fn an_unreachable_model_or_no_model_keeps_the_facts_and_proposes_nothing() {
    let facts = clear_cause();

    let (result, requests) = run(
        input(facts),
        one_pass(),
        vec![DistillReply::Unavailable {
            detail: "no model is configured".to_string(),
        }],
    );

    assert_eq!(requests.len(), 1);
    assert_eq!(result.outcome, HindsightOutcome::NeedsReview);
    assert_eq!(result.trace.model_calls, 0);
    assert!(result.trace.fallback);
    assert!(result.draft.hypotheses.is_empty() && result.draft.proposed_lesson.is_none());
    assert!(
        result.draft.validate(&clear_cause()).is_ok(),
        "the fallback is itself valid"
    );
}

#[test]
fn a_refused_constraint_is_free_and_is_not_asked_for_again() {
    let facts = clear_cause();
    let plan = DistillPlan {
        attempt_schema: true,
        ..staged()
    };
    let causes = serde_json::json!({
        "intended_outcome": "Make the test suite pass",
        "observed_outcome": "It passed after a migration was added",
        "hypotheses": [{ "claim": "The migration was never written", "evidence_ids": [facts[0].id.as_str(), facts[1].id.as_str()], "confidence": 0.6 }]
    })
    .to_string();
    let proposal = serde_json::json!({ "proposed_lesson": LESSON }).to_string();

    let (result, requests) = run(
        input(facts),
        plan,
        vec![
            DistillReply::Text {
                content: causes,
                disposition: ConstraintDisposition::RefusedByTransport,
            },
            text(&proposal),
        ],
    );

    assert!(requests[0].schema.is_some(), "the constraint was attempted");
    assert!(
        requests[1].schema.is_none(),
        "and not asked for again once refused"
    );
    assert!(
        !result.trace.repair_spent,
        "a refusal never spends the repair pass"
    );
    assert_eq!(
        result.trace.dispositions,
        vec![
            ConstraintDisposition::RefusedByTransport,
            ConstraintDisposition::NotRequested
        ]
    );
    assert_eq!(result.outcome, HindsightOutcome::Candidate);
}

#[test]
fn the_staged_passes_skip_the_second_when_the_first_finds_no_cause() {
    let facts = clear_cause();
    let no_cause = r#"{"intended_outcome": "Make the test suite pass", "observed_outcome": "It passed", "hypotheses": []}"#;

    let (result, requests) = run(input(facts), staged(), vec![text(no_cause)]);

    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].purpose, RequestPurpose::WhatHappened);
    assert_eq!(result.outcome, HindsightOutcome::UnknownCause);
}

#[test]
fn the_second_staged_pass_builds_on_the_causes_the_first_found() {
    let facts = clear_cause();
    let causes = serde_json::json!({
        "intended_outcome": "Make the test suite pass",
        "observed_outcome": "It passed after a migration was added",
        "hypotheses": [{ "claim": "The migration was never written", "evidence_ids": [facts[0].id.as_str(), facts[1].id.as_str()], "confidence": 0.6 }]
    })
    .to_string();

    let (result, requests) = run(
        input(facts.clone()),
        staged(),
        vec![
            text(&causes),
            DistillReply::Unavailable {
                detail: "connection reset".to_string(),
            },
        ],
    );

    assert_eq!(requests[1].purpose, RequestPurpose::WhatFollows);
    assert!(requests[1].messages[1]
        .content
        .contains("The migration was never written"));
    assert_eq!(result.outcome, HindsightOutcome::NeedsReview);
    assert_eq!(result.reasons[0], OutcomeReason::Incomplete);
    assert_eq!(
        result.draft.hypotheses.len(),
        1,
        "the validated causes are kept"
    );
    assert!(result.draft.proposed_lesson.is_none());
    assert!(!result.trace.fallback);
}

#[test]
fn adaptive_plans_follow_the_declared_context_not_the_model_name() {
    assert_eq!(
        DistillPlan::adaptive(Some(8_192), false).strategy,
        Strategy::Staged
    );
    assert_eq!(
        DistillPlan::adaptive(Some(131_072), false).strategy,
        Strategy::OnePass
    );
    assert_eq!(
        DistillPlan::adaptive(None, false).strategy,
        Strategy::OnePass
    );
}

#[test]
fn a_small_context_keeps_every_label_and_drops_excerpts_from_the_end() {
    let facts: Vec<EvidenceRef> = (0..12)
        .map(|index| {
            fact(
                &format!("`run_shell` call `c{index}` failed"),
                &format!("c{index}"),
            )
            .with_excerpt("x".repeat(400))
        })
        .collect();
    let plan = DistillPlan {
        context_tokens: Some(2_000),
        ..one_pass()
    };

    let mut distiller = Distiller::new(input(facts.clone()), plan);
    let DistillStep::Ask(request) = distiller.start() else {
        panic!("expected a request");
    };
    let prompt = &request.messages[1].content;

    for fact in &facts {
        assert!(
            prompt.contains(fact.id.as_str()),
            "every fact stays citable"
        );
    }
    assert!(prompt.contains("Not recorded (these are gaps, not facts"));
    let DistillStep::Done(result) = distiller.reply(DistillReply::Unavailable {
        detail: "stop".to_string(),
    }) else {
        panic!("expected a result");
    };
    assert!(result.trace.excerpts_dropped > 0);
}

// --- the abstention check, on the six frozen cases -------------------------
//
// Each case below is an original fixture written from the recorded descriptions
// of the six cases two local models were measured on. The drafts are the kind
// both models produced: correctly cited, and for three of the six, a durable
// rule manufactured from a one-off.

fn draft(
    facts: &[EvidenceRef],
    cited: &[usize],
    lesson: Option<&str>,
) -> localmind_core::HindsightDraft {
    let mut draft = localmind_core::HindsightDraft::new("intended", "observed");
    if !cited.is_empty() {
        draft = draft.with_hypothesis(localmind_core::CausalHypothesis {
            claim: "cause".to_string(),
            evidence_ids: cited.iter().map(|index| facts[*index].id.clone()).collect(),
            confidence: localmind_core::Confidence::new(0.7).unwrap(),
        });
    }
    if let Some(lesson) = lesson {
        draft = draft.with_proposed_lesson(lesson);
    }
    assert!(draft.validate(facts).is_ok());
    draft
}

#[test]
fn case_clear_cause_keeps_its_lesson() {
    let facts = clear_cause();
    let (outcome, reasons) = decide_outcome(&draft(&facts, &[0, 1], Some(LESSON)), &facts);
    assert_eq!(outcome, HindsightOutcome::Candidate, "{reasons:?}");
}

#[test]
fn case_unknown_cause_with_no_cause_named_is_unknown() {
    let facts = clear_cause();
    let (outcome, _) = decide_outcome(&draft(&facts, &[], None), &facts);
    assert_eq!(outcome, HindsightOutcome::UnknownCause);
}

#[test]
fn case_unknown_cause_with_a_rule_from_one_timeout_is_no_lesson() {
    // One timeout, then the identical run passed. The measured draft proposed
    // "retry logic or a longer timeout" — plausible, and founded on nothing.
    let facts = vec![
        failure("`run_shell` call `c1` failed", "c1", "run_shell:suite"),
        success("`run_shell` call `c2` succeeded", "c2", "run_shell:suite"),
    ];
    let lesson = "Implement retry logic or increase the test timeout threshold";

    let (outcome, reasons) = decide_outcome(&draft(&facts, &[0, 1], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::NoLesson);
    assert!(
        reasons.contains(&OutcomeReason::OneOffResolvedByRetry),
        "{reasons:?}"
    );
}

#[test]
fn case_no_lesson_an_unmounted_drive_is_the_environment() {
    let facts = vec![
        failure("`write_file` call `c1` failed", "c1", "write_file:backup"),
        correction(
            "driver `host` intervened: steer",
            "d1",
            "the external drive was unmounted; it is plugged back in now",
        ),
        success(
            "`write_file` call `c2` succeeded",
            "c2",
            "write_file:backup",
        ),
    ];
    let lesson = "Verify that external storage devices are mounted before attempting file writes.";

    let (outcome, reasons) = decide_outcome(&draft(&facts, &[0, 1], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::NoLesson);
    assert!(reasons.contains(&OutcomeReason::EnvironmentCheckIntervention));
    assert!(reasons.contains(&OutcomeReason::OneOffEnvironmentalCause));
}

#[test]
fn case_confabulation_bait_try_again_is_not_a_lesson() {
    let facts = vec![
        failure("`fetch` call `c1` failed", "c1", "fetch:registry"),
        success("`fetch` call `c2` succeeded", "c2", "fetch:registry"),
    ];
    let lesson = "Retry the command if a transient network error occurs.";

    let (outcome, reasons) = decide_outcome(&draft(&facts, &[0], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::NoLesson);
    assert!(reasons.contains(&OutcomeReason::RetryIntervention));
    assert!(reasons.contains(&OutcomeReason::OneOffResolvedByRetry));
}

#[test]
fn case_user_correction_a_convention_is_a_lesson_not_the_environment() {
    let facts = vec![
        success("`edit_file` call `c1` succeeded", "c1", "edit_file:parser"),
        correction(
            "driver `host` intervened: steer",
            "d1",
            "this repository indents with tabs, not spaces",
        ),
        success(
            "`edit_file` call `c2` succeeded",
            "c2",
            "edit_file:parser-tabs",
        ),
    ];
    let lesson = "Indent with tabs in this repository; the maintainer asked for it";

    let (outcome, reasons) = decide_outcome(&draft(&facts, &[1], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::Candidate, "{reasons:?}");
}

#[test]
fn case_noisy_truncated_malformed_input_never_becomes_a_lesson() {
    let facts = clear_cause();
    let noise = "{\"intended_outcome\": \"???\", \"hypotheses\": [{\"claim\": \"it br";

    let (result, _) = run(
        input(facts),
        one_pass(),
        vec![text(noise), text("sorry, I lost the thread")],
    );

    assert_eq!(result.outcome, HindsightOutcome::Malformed);
    assert!(result.draft.proposed_lesson.is_none());
}

#[test]
fn a_repeated_failure_is_not_a_one_off() {
    let facts = vec![
        failure("`run_shell` call `c1` failed", "c1", "run_shell:suite"),
        failure("`run_shell` call `c2` failed", "c2", "run_shell:suite"),
        success("`run_shell` call `c3` succeeded", "c3", "run_shell:suite"),
    ];
    let lesson = "Pin the fixture clock so the suite does not depend on wall time";

    let (outcome, reasons) = decide_outcome(&draft(&facts, &[0, 1], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::Candidate, "{reasons:?}");
}

#[test]
fn a_failure_with_no_signature_is_never_presumed_a_one_off() {
    let facts = vec![
        fact("`run_shell` call `c1` failed", "c1").with_observation(Observation::Failure),
        fact("`run_shell` call `c2` succeeded", "c2").with_observation(Observation::Success),
    ];
    let lesson = "Build the generated bindings before the tests that import them";

    let (outcome, _) = decide_outcome(&draft(&facts, &[0], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::Candidate);
}

#[test]
fn adding_retry_logic_is_a_change_not_trying_again() {
    let facts = clear_cause();
    let lesson = "Add retry logic with backoff to the registry client";

    let (outcome, reasons) = decide_outcome(&draft(&facts, &[0, 1], Some(lesson)), &facts);

    assert_eq!(outcome, HindsightOutcome::Candidate, "{reasons:?}");
}

#[test]
fn recording_abstentions_is_off_unless_the_project_asks() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".localmind.toml");
    std::fs::write(&config, "[learning]\nenabled = true\n").unwrap();
    let default = localmind_store::ProjectConfig::discover(dir.path()).unwrap();
    assert!(!default.config.review.record_abstentions);

    std::fs::write(
        &config,
        "[learning]\nenabled = true\n\n[review]\nrecord_abstentions = true\n",
    )
    .unwrap();
    let opted_in = localmind_store::ProjectConfig::discover(dir.path()).unwrap();
    assert!(opted_in.config.review.record_abstentions);
}

#[test]
fn a_lesson_over_an_incomplete_record_is_kept_for_review_not_queued_as_a_lesson() {
    let facts = clear_cause();
    let reply = draft_json(&facts, Some(LESSON), None);
    let damaged = |incompleteness| InputGap {
        description: "session fixture: 2 damaged log line(s) skipped".to_string(),
        incompleteness: Some(incompleteness),
    };

    for (gap, expect) in [
        // Lines lost in the session the cited facts came from.
        (
            damaged(Incompleteness::Source("session:fixture".to_string())),
            HindsightOutcome::NeedsReview,
        ),
        // A run-wide gap, such as a step whose sessions are unknown.
        (damaged(Incompleteness::Run), HindsightOutcome::NeedsReview),
        // Lines lost somewhere the draft does not look.
        (
            damaged(Incompleteness::Source("session:elsewhere".to_string())),
            HindsightOutcome::Candidate,
        ),
        // A gap that says nothing about completeness.
        (
            InputGap::note("3 of 3 call(s) carry no verifier verdict"),
            HindsightOutcome::Candidate,
        ),
    ] {
        let mut input = input(facts.clone());
        input.gaps = vec![gap];

        let (result, _) = run(input, one_pass(), vec![text(&reply)]);

        assert_eq!(result.outcome, expect, "{:?}", result.reasons);
        if expect == HindsightOutcome::NeedsReview {
            assert!(matches!(
                result.reasons.last(),
                Some(OutcomeReason::IncompleteRecord { .. })
            ));
            assert_eq!(
                result.draft.proposed_lesson.as_deref(),
                Some(LESSON),
                "the lesson is kept for a person to judge"
            );
        }
    }
}

#[test]
fn the_prompt_no_longer_talks_the_model_out_of_a_lesson() {
    let mut distiller = Distiller::new(input(clear_cause()), one_pass());
    let DistillStep::Ask(request) = distiller.start() else {
        panic!("expected a request");
    };
    let prompt = &request.messages[1].content;
    // One-offs are the check's to catch, not the drafter's to fear.
    assert!(!prompt.contains("is not a lesson"));
    assert!(prompt.contains("names no fact ids"));
}
