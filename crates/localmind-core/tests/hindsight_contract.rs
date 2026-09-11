//! The hindsight contract as a consumer sees it.
//!
//! Two things are being defended here. The first is that adding hindsight took
//! nothing away: a candidate written before the lab existed must still load,
//! review and promote exactly as it did. The second is that a draft cannot
//! smuggle in a fact — an id that was never supplied, an observation edited
//! after the fact, or an id whose construction cannot be verified is refused
//! with no model and no network.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use localmind_core::{
    CandidateLesson, CausalHypothesis, Confidence, EvidenceKind, EvidenceRef, HindsightDraft,
    HindsightOutcome, HindsightViolation, LessonCategory, LessonId, SuggestedAction,
};

/// A candidate serialized before hindsight existed. Byte-for-byte what the
/// review queue holds in `candidate_json` today, so this file failing to load
/// means the on-disk contract broke.
const CANDIDATE_BEFORE_HINDSIGHT: &str = r#"{
  "id": "lesson-1",
  "summary": "Prefer reviewed memory writes over automatic promotion.",
  "rationale": null,
  "category": "Process",
  "confidence": 0.8,
  "evidence": [
    {
      "id": "redacted transcript",
      "kind": "Transcript",
      "label": "redacted transcript",
      "uri": null,
      "redacted": true,
      "content_hash": null,
      "metadata": {}
    }
  ],
  "related_files": [],
  "related_entities": [],
  "suggested_destination": "ProjectMemory",
  "suggested_action": "PromoteToMemory",
  "validation_status": "Valid",
  "review_annotation": null,
  "tool_use": null,
  "requires_edit_before_promotion": false
}"#;

fn fact(locator: &str, content: &str) -> EvidenceRef {
    EvidenceRef::identified(
        EvidenceKind::TestOutput,
        "cargo nextest: 2 failed",
        "session:2026-09-11-a",
        locator,
        content,
    )
}

fn supplied() -> Vec<EvidenceRef> {
    vec![
        fact("repo@9f1c2d#target/nextest/run-1.log", "sha256:01"),
        fact("repo@9f1c2d#target/nextest/run-2.log", "sha256:02"),
    ]
}

fn candidate_with(draft: HindsightDraft, facts: Vec<EvidenceRef>) -> CandidateLesson {
    let mut candidate = CandidateLesson::new(
        LessonId::new("lesson-2"),
        "Give every fixture store its own temporary root.",
        LessonCategory::TestingStrategy,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    );
    for fact in facts {
        candidate = candidate.with_evidence(fact);
    }
    candidate.with_hindsight(draft)
}

fn sound_draft(facts: &[EvidenceRef]) -> HindsightDraft {
    HindsightDraft::new(
        "the review-queue suite passes",
        "two cases failed against a shared fixture store",
    )
    .with_hypothesis(CausalHypothesis {
        claim: "the two cases shared one fixture store and raced on it".to_string(),
        evidence_ids: facts.iter().map(|fact| fact.id.clone()).collect(),
        confidence: Confidence::new(0.7).unwrap(),
    })
    .with_intervention("give each case its own temporary store")
    .with_proposed_lesson("Give every fixture store its own temporary root.")
}

#[test]
fn a_candidate_written_before_hindsight_still_loads() {
    let candidate: CandidateLesson = serde_json::from_str(CANDIDATE_BEFORE_HINDSIGHT).unwrap();

    assert!(candidate.hindsight.is_none());
    assert_eq!(candidate.evidence().len(), 1);
    assert!(candidate.evidence()[0].redacted, "redaction survives");
    assert_eq!(
        candidate.summary(),
        "Prefer reviewed memory writes over automatic promotion."
    );
    // No draft is not a defect. Ordinary extraction and direct proposal never
    // produce one, and both remain first-class review paths.
    assert!(candidate.validate_hindsight().is_ok());
}

#[test]
fn a_candidate_without_hindsight_serializes_without_the_field() {
    let candidate: CandidateLesson = serde_json::from_str(CANDIDATE_BEFORE_HINDSIGHT).unwrap();
    let json = serde_json::to_string(&candidate).unwrap();

    assert!(
        !json.contains("hindsight"),
        "an absent draft must not add a key to every existing record"
    );

    let round_tripped: CandidateLesson = serde_json::from_str(&json).unwrap();
    assert_eq!(candidate, round_tripped);
}

#[test]
fn a_candidate_carrying_hindsight_round_trips() {
    let facts = supplied();
    let candidate = candidate_with(sound_draft(&facts), facts);

    let json = serde_json::to_string(&candidate).unwrap();
    let back: CandidateLesson = serde_json::from_str(&json).unwrap();

    assert_eq!(candidate, back);
    assert!(back.validate_hindsight().is_ok());
}

#[test]
fn an_unknown_field_is_ignored_and_the_version_is_what_catches_a_future_writer() {
    // `deny_unknown_fields` is not set, so this documents what actually
    // happens: an unfamiliar key is dropped. The contract version is what
    // catches a record from a future writer, not the field list.
    let json = r#"{
      "version": 1,
      "intended_outcome": "it works",
      "observed_outcome": "it did not",
      "hypotheses": [],
      "invented_field": "ignored"
    }"#;

    let draft: HindsightDraft = serde_json::from_str(json).unwrap();
    assert_eq!(draft.intended_outcome, "it works");

    let future = json.replace("\"version\": 1", "\"version\": 99");
    let draft: HindsightDraft = serde_json::from_str(&future).unwrap();
    assert!(matches!(
        draft.validate(&[]).unwrap_err().as_slice(),
        [HindsightViolation::UnsupportedVersion { version: 99 }]
    ));
}

#[test]
fn an_invented_evidence_id_is_rejected() {
    let facts = supplied();
    let mut draft = sound_draft(&facts);
    draft.hypotheses[0]
        .evidence_ids
        .push(localmind_core::stable_evidence_id(
            &EvidenceKind::Commit,
            "session:elsewhere",
            "repo@000000",
            "sha256:ff",
        ));

    let candidate = candidate_with(draft, facts);
    let violations = candidate.validate_hindsight().unwrap_err();

    assert!(violations
        .iter()
        .any(|violation| matches!(violation, HindsightViolation::UnknownEvidenceId { .. })));
}

#[test]
fn a_modified_observation_is_rejected() {
    let facts = supplied();
    let draft = sound_draft(&facts);
    let mut candidate = candidate_with(draft, facts);

    // Rewrite the fact after the draft cited it. The id still resolves and the
    // citation check passes; the identity no longer matches the content.
    let mut tampered = candidate.evidence()[0].clone();
    tampered.content_hash = Some("sha256:rewritten".to_string());
    let rest = candidate.evidence()[1].clone();
    candidate = CandidateLesson::new(
        LessonId::new("lesson-2"),
        "Give every fixture store its own temporary root.",
        LessonCategory::TestingStrategy,
        Confidence::new(0.7).unwrap(),
        SuggestedAction::PromoteToMemory,
    )
    .with_evidence(tampered)
    .with_evidence(rest)
    .with_hindsight(candidate.hindsight.unwrap());

    let violations = candidate.validate_hindsight().unwrap_err();
    assert!(violations
        .iter()
        .any(|violation| matches!(violation, HindsightViolation::MutatedFact { .. })));
}

#[test]
fn a_non_canonical_fact_set_is_rejected_even_though_every_citation_resolves() {
    let legacy = EvidenceRef::new(EvidenceKind::Transcript, "a transcript");
    let draft = HindsightDraft::new("it works", "it did not").with_hypothesis(CausalHypothesis {
        claim: "the transcript shows the tool was never called".to_string(),
        evidence_ids: vec![legacy.id.clone()],
        confidence: Confidence::new(0.6).unwrap(),
    });

    let candidate = candidate_with(draft, vec![legacy]);
    let violations = candidate.validate_hindsight().unwrap_err();

    assert!(
        !violations
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::UnknownEvidenceId { .. })),
        "the citation does resolve — that is exactly why the id check is needed"
    );
    assert!(violations
        .iter()
        .any(|violation| matches!(violation, HindsightViolation::NonCanonicalEvidenceId { .. })));
}

#[test]
fn an_unknown_cause_record_is_a_valid_outcome() {
    let facts = supplied();
    let draft = HindsightDraft {
        suggested_outcome: Some(HindsightOutcome::UnknownCause),
        ..HindsightDraft::new(
            "the suite passes",
            "one case timed out, then passed unchanged",
        )
    };
    let candidate = candidate_with(draft, facts);

    assert!(candidate.validate_hindsight().is_ok());

    let draft = candidate.hindsight.as_ref().unwrap();
    assert!(draft.hypotheses.is_empty());
    assert!(draft.proposed_lesson.is_none());
    assert_eq!(
        draft.suggested_outcome(),
        Some(HindsightOutcome::UnknownCause)
    );
    assert!(!HindsightOutcome::UnknownCause.proposes_a_lesson());
}

#[test]
fn evidence_identity_is_stable_across_a_serialization_round_trip() {
    let facts = supplied();
    let before: Vec<String> = facts.iter().map(|fact| fact.id.to_string()).collect();
    let candidate = candidate_with(sound_draft(&facts), facts);

    let json = serde_json::to_string(&candidate).unwrap();
    let back: CandidateLesson = serde_json::from_str(&json).unwrap();

    let after: Vec<String> = back
        .evidence()
        .iter()
        .map(|fact| fact.id.to_string())
        .collect();
    assert_eq!(before, after);
    for fact in back.evidence() {
        assert!(
            fact.identity_is_intact(),
            "an id must still verify after a round trip"
        );
    }
}

#[test]
fn a_redacted_fact_stays_redacted_through_the_draft() {
    let facts = vec![EvidenceRef::identified(
        EvidenceKind::Transcript,
        "redacted transcript",
        "session:2026-09-11-a",
        "repo@9f1c2d#transcript",
        "sha256:aa",
    )
    .redacted()];
    let draft = HindsightDraft::new("it works", "it did not").with_hypothesis(CausalHypothesis {
        claim: "the transcript records the failing call".to_string(),
        evidence_ids: vec![facts[0].id.clone()],
        confidence: Confidence::new(0.6).unwrap(),
    });

    let candidate = candidate_with(draft, facts);
    assert!(candidate.validate_hindsight().is_ok());

    let json = serde_json::to_string(&candidate).unwrap();
    let back: CandidateLesson = serde_json::from_str(&json).unwrap();
    assert!(back.evidence()[0].redacted);
}

#[test]
fn the_draft_stores_claims_and_citations_rather_than_a_reasoning_transcript() {
    let facts = supplied();
    let candidate = candidate_with(sound_draft(&facts), facts);
    let json = serde_json::to_string(&candidate).unwrap();

    // Bounds are the mechanism: every free-text field is capped, so there is
    // nowhere for an unrestricted transcript to be stored even if one is
    // offered.
    let draft = candidate.hindsight.as_ref().unwrap();
    for text in [&draft.intended_outcome, &draft.observed_outcome] {
        assert!(text.chars().count() <= localmind_core::MAX_TEXT_CHARS);
    }
    assert!(
        draft
            .proposed_lesson
            .as_ref()
            .map(|lesson| lesson.chars().count())
            .unwrap_or_default()
            <= localmind_core::MAX_LESSON_CHARS
    );
    assert!(!json.contains("<think>"));
}
