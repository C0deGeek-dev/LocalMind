//! Evidence-linked hindsight: what was intended, what happened, and the
//! bounded, cited analysis in between.
//!
//! A [`HindsightDraft`] is intermediate evidence — not accepted memory, and not
//! a reasoning transcript. It holds concise claims, references to facts that
//! were *supplied* to it, calibrated confidence, and the decisions that follow.
//! Every field is bounded, which is how "store no unrestricted reasoning
//! transcript" is enforced rather than merely asked for.
//!
//! The draft carries no facts of its own. Facts are the [`EvidenceRef`] values
//! already on the candidate, and a hypothesis cites them by [`EvidenceId`]: a
//! model may select an id it was given and can neither mint nor edit one. That
//! also means the shape a model fills in and the shape that is stored are the
//! same shape, so there is one contract to validate rather than two.
//!
//! ## The outcome is not the model's to choose
//!
//! [`HindsightDraft::suggested_outcome`] records what the drafting pass
//! proposed and is **advisory**. Abstention is decided afterwards by a
//! deterministic check the drafting model has no part in. This is measured, not
//! precautionary: over six frozen cases, two models at different capability
//! levels each produced a correct, correctly cited, high-confidence causal
//! hypothesis and then manufactured a durable rule from a one-off, failing on
//! the same two cases in the same direction — with zero invented ids and zero
//! uncited claims in either. The stronger model did worse. So an evidence check
//! cannot see this failure and a better model does not fix it.

use crate::{Confidence, EvidenceId, EvidenceRef};
use serde::{Deserialize, Serialize};

/// Contract version of [`HindsightDraft`]. A record written at an unknown
/// version is rejected rather than partially understood.
pub const HINDSIGHT_DRAFT_VERSION: u32 = 1;

/// Most causal hypotheses one draft may carry. Alternatives are the point —
/// a single forced explanation is what this contract exists to avoid — but an
/// unbounded list is a reasoning transcript by another name.
pub const MAX_HYPOTHESES: usize = 5;

/// Most facts one hypothesis may cite.
pub const MAX_EVIDENCE_IDS_PER_HYPOTHESIS: usize = 8;

/// Most entries in a bounded list field (missed signals, preconditions).
pub const MAX_LIST_ITEMS: usize = 8;

/// Character ceiling on a narrative field.
pub const MAX_TEXT_CHARS: usize = 500;

/// Character ceiling on the proposed lesson. Tighter than the analysis around
/// it: the durable artefact is one reusable sentence, not a summary of the
/// investigation.
pub const MAX_LESSON_CHARS: usize = 280;

/// Highest confidence a hypothesis citing a single fact may claim.
///
/// A structural bound, not a semantic one: it says nothing about whether a
/// claim is true, only that one observation does not structurally support
/// near-certainty. Domain review assesses the rest.
pub const SINGLE_FACT_CONFIDENCE_CEILING: f32 = 0.8;

/// A bounded causal claim over facts the drafter was given.
///
/// Several may coexist in one draft. Underdetermined failures are the common
/// case, and forcing a single explanation is how a plausible-but-wrong cause
/// becomes durable guidance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CausalHypothesis {
    /// One sentence. What is claimed to have caused the observed outcome.
    pub claim: String,
    /// The facts this claim rests on, by id. Never empty: a claim that cites
    /// nothing is not a hypothesis, it is an assertion.
    pub evidence_ids: Vec<EvidenceId>,
    /// Calibrated confidence in `[0, 1]`.
    ///
    /// A drafting pass that thinks in `high`/`medium`/`low` maps onto this at
    /// the extraction edge rather than introducing a second confidence
    /// vocabulary here.
    pub confidence: Confidence,
}

/// How a hindsight draft resolves.
///
/// `Candidate` is one outcome among five, not the goal. `UnknownCause` and
/// `NoLesson` close cleanly into episodic evidence and are successes: many
/// failures are incidental or underdetermined, and teaching a confident fiction
/// is worse than learning nothing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum HindsightOutcome {
    /// The evidence supports a reusable lesson worth proposing for review.
    Candidate,
    /// The evidence does not establish why the outcome differed.
    UnknownCause,
    /// The cause is clear and nothing reusable follows from it.
    NoLesson,
    /// Structurally sound, but a human should look before it goes further.
    NeedsReview,
    /// The record violates the contract. Carries no reusable guidance.
    Malformed,
}

impl HindsightOutcome {
    /// Whether this outcome may produce reusable guidance. `false` for every
    /// abstention, which is what keeps "no lesson" from quietly becoming one.
    #[must_use]
    pub fn proposes_a_lesson(self) -> bool {
        matches!(self, Self::Candidate)
    }
}

/// A versioned, bounded, evidence-linked hindsight record.
///
/// The concise `proposed_lesson` is deliberately separate from the analysis
/// that produced it: promotion writes a reusable sentence into memory, while
/// the hypotheses, missed signals and counterfactual stay behind as reviewable
/// evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HindsightDraft {
    /// Contract version. See [`HINDSIGHT_DRAFT_VERSION`].
    pub version: u32,
    /// What the session was trying to achieve.
    pub intended_outcome: String,
    /// What actually happened.
    pub observed_outcome: String,
    /// Candidate causes, each citing supplied facts. May be empty — that is
    /// what `UnknownCause` looks like.
    #[serde(default)]
    pub hypotheses: Vec<CausalHypothesis>,
    /// Evidence that was present and not acted on at the time.
    #[serde(default)]
    pub missed_signals: Vec<String>,
    /// The change that would have avoided the observed outcome.
    #[serde(default)]
    pub intervention: Option<String>,
    /// What the drafter predicts would have happened had the intervention been
    /// applied. Recorded so a later run can contradict it.
    #[serde(default)]
    pub counterfactual_prediction: Option<String>,
    /// Where the proposed lesson applies.
    #[serde(default)]
    pub applicability: Option<String>,
    /// What must hold for it to apply.
    #[serde(default)]
    pub preconditions: Vec<String>,
    /// What would make it stop being true.
    #[serde(default)]
    pub invalidation: Option<String>,
    /// The concise, reusable lesson, kept apart from the analysis above.
    #[serde(default)]
    pub proposed_lesson: Option<String>,
    /// What the drafting pass proposed. **Advisory only** — see the module
    /// documentation. Nothing downstream may be load-bearing on this field.
    #[serde(default)]
    pub suggested_outcome: Option<HindsightOutcome>,
}

impl HindsightDraft {
    /// A draft at the current contract version with the two required
    /// observations and nothing else.
    #[must_use]
    pub fn new(intended_outcome: impl Into<String>, observed_outcome: impl Into<String>) -> Self {
        Self {
            version: HINDSIGHT_DRAFT_VERSION,
            intended_outcome: intended_outcome.into(),
            observed_outcome: observed_outcome.into(),
            hypotheses: Vec::new(),
            missed_signals: Vec::new(),
            intervention: None,
            counterfactual_prediction: None,
            applicability: None,
            preconditions: Vec::new(),
            invalidation: None,
            proposed_lesson: None,
            suggested_outcome: None,
        }
    }

    #[must_use]
    pub fn with_hypothesis(mut self, hypothesis: CausalHypothesis) -> Self {
        self.hypotheses.push(hypothesis);
        self
    }

    #[must_use]
    pub fn with_proposed_lesson(mut self, lesson: impl Into<String>) -> Self {
        self.proposed_lesson = Some(lesson.into());
        self
    }

    #[must_use]
    pub fn with_intervention(mut self, intervention: impl Into<String>) -> Self {
        self.intervention = Some(intervention.into());
        self
    }

    /// The suggestion the drafting pass made, if any.
    ///
    /// Named to be awkward to mistake for a decision: the deterministic
    /// abstention gate owns the real outcome.
    #[must_use]
    pub fn suggested_outcome(&self) -> Option<HindsightOutcome> {
        self.suggested_outcome
    }

    /// Every distinct fact id this draft cites.
    #[must_use]
    pub fn cited_evidence_ids(&self) -> Vec<&EvidenceId> {
        let mut cited: Vec<&EvidenceId> = Vec::new();
        for hypothesis in &self.hypotheses {
            for id in &hypothesis.evidence_ids {
                if !cited.contains(&id) {
                    cited.push(id);
                }
            }
        }
        cited
    }

    /// Check this draft against the fact set it was drafted over.
    ///
    /// Returns **every** violation rather than the first, so a rejection can
    /// name what is wrong instead of sending the drafter back one problem at a
    /// time. An empty result means the record is structurally sound — never
    /// that its causal claims are true. Only domain review decides that.
    ///
    /// # Errors
    /// [`HindsightViolation`] values describing each contract breach.
    pub fn validate(&self, facts: &[EvidenceRef]) -> Result<(), Vec<HindsightViolation>> {
        let mut violations = Vec::new();

        if self.version != HINDSIGHT_DRAFT_VERSION {
            violations.push(HindsightViolation::UnsupportedVersion {
                version: self.version,
            });
        }

        check_required(&self.intended_outcome, "intended_outcome", &mut violations);
        check_required(&self.observed_outcome, "observed_outcome", &mut violations);
        check_optional(
            self.intervention.as_deref(),
            "intervention",
            &mut violations,
        );
        check_optional(
            self.counterfactual_prediction.as_deref(),
            "counterfactual_prediction",
            &mut violations,
        );
        check_optional(
            self.applicability.as_deref(),
            "applicability",
            &mut violations,
        );
        check_optional(
            self.invalidation.as_deref(),
            "invalidation",
            &mut violations,
        );

        if let Some(lesson) = self.proposed_lesson.as_deref() {
            if lesson.trim().is_empty() {
                violations.push(HindsightViolation::EmptyField {
                    field: "proposed_lesson",
                });
            }
            check_length(lesson, "proposed_lesson", MAX_LESSON_CHARS, &mut violations);
        }

        check_list(&self.missed_signals, "missed_signals", &mut violations);
        check_list(&self.preconditions, "preconditions", &mut violations);

        self.check_facts(facts, &mut violations);
        self.check_hypotheses(facts, &mut violations);
        self.check_internal_consistency(&mut violations);

        if violations.is_empty() {
            Ok(())
        } else {
            Err(violations)
        }
    }

    /// The supplied facts must each be verifiable. A fact set mixing canonical
    /// and label-derived ids would satisfy "every cited id was supplied" while
    /// remaining collidable, which is the whole guarantee.
    fn check_facts(&self, facts: &[EvidenceRef], violations: &mut Vec<HindsightViolation>) {
        for fact in facts {
            if !fact.has_canonical_id() {
                violations.push(HindsightViolation::NonCanonicalEvidenceId {
                    id: fact.id.clone(),
                });
            } else if !fact.identity_is_intact() {
                violations.push(HindsightViolation::MutatedFact {
                    id: fact.id.clone(),
                });
            }
        }
    }

    fn check_hypotheses(&self, facts: &[EvidenceRef], violations: &mut Vec<HindsightViolation>) {
        if self.hypotheses.len() > MAX_HYPOTHESES {
            violations.push(HindsightViolation::TooManyItems {
                field: "hypotheses",
                count: self.hypotheses.len(),
                limit: MAX_HYPOTHESES,
            });
        }

        for (index, hypothesis) in self.hypotheses.iter().enumerate() {
            if hypothesis.claim.trim().is_empty() {
                violations.push(HindsightViolation::EmptyClaim { index });
            }
            check_length(&hypothesis.claim, "claim", MAX_TEXT_CHARS, violations);

            if hypothesis.evidence_ids.is_empty() {
                violations.push(HindsightViolation::UncitedHypothesis { index });
            }
            if hypothesis.evidence_ids.len() > MAX_EVIDENCE_IDS_PER_HYPOTHESIS {
                violations.push(HindsightViolation::TooManyItems {
                    field: "evidence_ids",
                    count: hypothesis.evidence_ids.len(),
                    limit: MAX_EVIDENCE_IDS_PER_HYPOTHESIS,
                });
            }

            for id in &hypothesis.evidence_ids {
                if !facts.iter().any(|fact| &fact.id == id) {
                    violations.push(HindsightViolation::UnknownEvidenceId { id: id.clone() });
                }
            }

            let distinct = distinct_count(&hypothesis.evidence_ids);
            if distinct <= 1 && hypothesis.confidence.value() > SINGLE_FACT_CONFIDENCE_CEILING {
                violations.push(HindsightViolation::UnsupportedCertainty {
                    index,
                    confidence: hypothesis.confidence.value(),
                    cited: distinct,
                });
            }
        }
    }

    /// Contradictions inside the record itself. Structure, not judgement: the
    /// advisory outcome may be wrong, but it may not disagree with the record
    /// it sits in.
    fn check_internal_consistency(&self, violations: &mut Vec<HindsightViolation>) {
        if self.proposed_lesson.is_some() && self.hypotheses.is_empty() {
            violations.push(HindsightViolation::LessonWithoutHypothesis);
        }

        match self.suggested_outcome {
            Some(HindsightOutcome::UnknownCause) if !self.hypotheses.is_empty() => {
                violations.push(HindsightViolation::ContradictoryOutcome {
                    outcome: HindsightOutcome::UnknownCause,
                    reason: "the draft carries causal hypotheses",
                });
            }
            Some(HindsightOutcome::NoLesson) if self.proposed_lesson.is_some() => {
                violations.push(HindsightViolation::ContradictoryOutcome {
                    outcome: HindsightOutcome::NoLesson,
                    reason: "the draft carries a proposed lesson",
                });
            }
            _ => {}
        }
    }
}

fn distinct_count(ids: &[EvidenceId]) -> usize {
    let mut seen: Vec<&EvidenceId> = Vec::new();
    for id in ids {
        if !seen.contains(&id) {
            seen.push(id);
        }
    }
    seen.len()
}

fn check_required(value: &str, field: &'static str, violations: &mut Vec<HindsightViolation>) {
    if value.trim().is_empty() {
        violations.push(HindsightViolation::EmptyField { field });
    }
    check_length(value, field, MAX_TEXT_CHARS, violations);
}

fn check_optional(
    value: Option<&str>,
    field: &'static str,
    violations: &mut Vec<HindsightViolation>,
) {
    if let Some(value) = value {
        if value.trim().is_empty() {
            violations.push(HindsightViolation::EmptyField { field });
        }
        check_length(value, field, MAX_TEXT_CHARS, violations);
    }
}

fn check_length(
    value: &str,
    field: &'static str,
    limit: usize,
    violations: &mut Vec<HindsightViolation>,
) {
    let chars = value.chars().count();
    if chars > limit {
        violations.push(HindsightViolation::FieldTooLong {
            field,
            chars,
            limit,
        });
    }
}

fn check_list(items: &[String], field: &'static str, violations: &mut Vec<HindsightViolation>) {
    if items.len() > MAX_LIST_ITEMS {
        violations.push(HindsightViolation::TooManyItems {
            field,
            count: items.len(),
            limit: MAX_LIST_ITEMS,
        });
    }
    for item in items {
        check_length(item, field, MAX_TEXT_CHARS, violations);
    }
}

/// One way a hindsight record breaks its contract.
///
/// Every variant is decidable without a model and without judging whether a
/// claim is true. Citation existence is checked here; causal truth is not, and
/// this type never claims otherwise.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum HindsightViolation {
    #[error("hindsight contract version {version} is not supported")]
    UnsupportedVersion { version: u32 },
    #[error("{field} is required and empty")]
    EmptyField { field: &'static str },
    #[error("{field} is {chars} characters, over the {limit} limit")]
    FieldTooLong {
        field: &'static str,
        chars: usize,
        limit: usize,
    },
    #[error("{field} has {count} entries, over the {limit} limit")]
    TooManyItems {
        field: &'static str,
        count: usize,
        limit: usize,
    },
    #[error("hypothesis {index} has an empty claim")]
    EmptyClaim { index: usize },
    #[error("hypothesis {index} cites no evidence")]
    UncitedHypothesis { index: usize },
    #[error("evidence id {id} was cited but never supplied")]
    UnknownEvidenceId { id: EvidenceId },
    #[error("evidence id {id} was not canonically constructed")]
    NonCanonicalEvidenceId { id: EvidenceId },
    #[error("evidence {id} no longer matches the identity it was recorded under")]
    MutatedFact { id: EvidenceId },
    #[error("hypothesis {index} claims confidence {confidence} citing {cited} fact(s)")]
    UnsupportedCertainty {
        index: usize,
        confidence: f32,
        cited: usize,
    },
    #[error("a lesson is proposed with no causal hypothesis behind it")]
    LessonWithoutHypothesis,
    #[error("suggested outcome {outcome:?} contradicts the record: {reason}")]
    ContradictoryOutcome {
        outcome: HindsightOutcome,
        reason: &'static str,
    },
}

/// The JSON Schema a drafting pass is asked to fill in.
///
/// Deliberately **flat**: no `$ref`, no `$defs`. Not a style preference —
/// llama.cpp silently falls back to unconstrained JSON when a schema uses them,
/// so the request succeeds, the reply is unconstrained, and the caller believes
/// it received constrained output. A schema that cannot be enforced is worse
/// than no schema, because it invites skipping validation.
///
/// This constant describes what a model may *emit*: facts are supplied to it
/// and cited by id, never authored, so they do not appear here.
pub const HINDSIGHT_DRAFT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["version", "intended_outcome", "observed_outcome", "hypotheses"],
  "properties": {
    "version": { "type": "integer" },
    "intended_outcome": { "type": "string", "maxLength": 500 },
    "observed_outcome": { "type": "string", "maxLength": 500 },
    "hypotheses": {
      "type": "array",
      "maxItems": 5,
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["claim", "evidence_ids", "confidence"],
        "properties": {
          "claim": { "type": "string", "maxLength": 500 },
          "evidence_ids": {
            "type": "array",
            "maxItems": 8,
            "items": { "type": "string" }
          },
          "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
        }
      }
    },
    "missed_signals": {
      "type": "array",
      "maxItems": 8,
      "items": { "type": "string", "maxLength": 500 }
    },
    "intervention": { "type": ["string", "null"], "maxLength": 500 },
    "counterfactual_prediction": { "type": ["string", "null"], "maxLength": 500 },
    "applicability": { "type": ["string", "null"], "maxLength": 500 },
    "preconditions": {
      "type": "array",
      "maxItems": 8,
      "items": { "type": "string", "maxLength": 500 }
    },
    "invalidation": { "type": ["string", "null"], "maxLength": 500 },
    "proposed_lesson": { "type": ["string", "null"], "maxLength": 280 },
    "suggested_outcome": {
      "type": ["string", "null"],
      "enum": ["Candidate", "UnknownCause", "NoLesson", "NeedsReview", "Malformed", null]
    }
  }
}"#;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        CausalHypothesis, HindsightDraft, HindsightOutcome, HindsightViolation,
        HINDSIGHT_DRAFT_SCHEMA, HINDSIGHT_DRAFT_VERSION, MAX_HYPOTHESES, MAX_LESSON_CHARS,
        MAX_TEXT_CHARS, SINGLE_FACT_CONFIDENCE_CEILING,
    };
    use crate::{Confidence, EvidenceId, EvidenceKind, EvidenceRef};

    fn fact(locator: &str, content: &str) -> EvidenceRef {
        EvidenceRef::identified(
            EvidenceKind::TestOutput,
            "an observation",
            "session:abc",
            locator,
            content,
        )
    }

    fn facts() -> Vec<EvidenceRef> {
        vec![
            fact("repo@abc#run/1", "sha256:01"),
            fact("repo@abc#run/2", "sha256:02"),
        ]
    }

    fn hypothesis(facts: &[EvidenceRef], confidence: f32) -> CausalHypothesis {
        CausalHypothesis {
            claim: "the run failed because the fixture store was not isolated".to_string(),
            evidence_ids: facts.iter().map(|fact| fact.id.clone()).collect(),
            confidence: Confidence::new(confidence).unwrap(),
        }
    }

    fn sound() -> (HindsightDraft, Vec<EvidenceRef>) {
        let facts = facts();
        let draft = HindsightDraft::new("the suite passes", "two cases failed")
            .with_hypothesis(hypothesis(&facts, 0.7))
            .with_intervention("give each case its own temporary store")
            .with_proposed_lesson("Give every fixture store its own temporary root.");
        (draft, facts)
    }

    #[test]
    fn a_sound_draft_validates() {
        let (draft, facts) = sound();
        assert!(draft.validate(&facts).is_ok());
    }

    #[test]
    fn a_draft_round_trips_through_json() {
        let (draft, _) = sound();
        let json = serde_json::to_string(&draft).unwrap();
        let back: HindsightDraft = serde_json::from_str(&json).unwrap();
        assert_eq!(draft, back);
    }

    #[test]
    fn an_invented_evidence_id_is_rejected() {
        let (mut draft, facts) = sound();
        draft.hypotheses[0]
            .evidence_ids
            .push(EvidenceId::new("ev-00000000000000000000000000000000"));

        let violations = draft.validate(&facts).unwrap_err();
        assert!(violations
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::UnknownEvidenceId { .. })));
    }

    #[test]
    fn a_modified_observation_is_rejected() {
        let (draft, mut facts) = sound();
        facts[0].content_hash = Some("sha256:rewritten".to_string());

        let violations = draft.validate(&facts).unwrap_err();
        assert!(violations
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::MutatedFact { .. })));
    }

    #[test]
    fn a_label_derived_fact_set_is_rejected() {
        let (mut draft, _) = sound();
        let legacy = EvidenceRef::new(EvidenceKind::Transcript, "a transcript");
        draft.hypotheses[0].evidence_ids = vec![legacy.id.clone()];

        // The id is cited and *is* supplied, so the "only supplied facts" rule
        // passes. The fact set is still collidable, which is what this catches.
        let violations = draft.validate(&[legacy]).unwrap_err();
        assert!(violations.iter().any(|violation| matches!(
            violation,
            HindsightViolation::NonCanonicalEvidenceId { .. }
        )));
    }

    #[test]
    fn an_uncited_claim_is_rejected() {
        let (mut draft, facts) = sound();
        draft.hypotheses[0].evidence_ids.clear();

        let violations = draft.validate(&facts).unwrap_err();
        assert!(violations
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::UncitedHypothesis { .. })));
    }

    #[test]
    fn near_certainty_from_a_single_fact_is_structurally_unsupported() {
        let facts = facts();
        let mut single = hypothesis(&facts, 0.95);
        single.evidence_ids.truncate(1);
        let draft = HindsightDraft::new("it works", "it did not").with_hypothesis(single);

        let violations = draft.validate(&facts).unwrap_err();
        assert!(violations
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::UnsupportedCertainty { .. })));

        // The same claim at a confidence one observation supports is fine.
        let mut modest = hypothesis(&facts, SINGLE_FACT_CONFIDENCE_CEILING);
        modest.evidence_ids.truncate(1);
        let draft = HindsightDraft::new("it works", "it did not").with_hypothesis(modest);
        assert!(draft.validate(&facts).is_ok());
    }

    #[test]
    fn repeating_one_citation_does_not_manufacture_support() {
        let facts = facts();
        let repeated = CausalHypothesis {
            claim: "the same fact, cited twice".to_string(),
            evidence_ids: vec![facts[0].id.clone(), facts[0].id.clone()],
            confidence: Confidence::new(0.95).unwrap(),
        };
        let draft = HindsightDraft::new("it works", "it did not").with_hypothesis(repeated);

        let violations = draft.validate(&facts).unwrap_err();
        assert!(violations
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::UnsupportedCertainty { .. })));
    }

    #[test]
    fn unknown_cause_and_no_lesson_are_valid_records() {
        let facts = facts();

        let unknown = HindsightDraft {
            suggested_outcome: Some(HindsightOutcome::UnknownCause),
            ..HindsightDraft::new(
                "the suite passes",
                "one case timed out, then passed unchanged",
            )
        };
        assert!(unknown.validate(&facts).is_ok());
        assert!(unknown.hypotheses.is_empty());

        let no_lesson = HindsightDraft {
            suggested_outcome: Some(HindsightOutcome::NoLesson),
            ..HindsightDraft::new("the write succeeds", "the write failed")
        }
        .with_hypothesis(hypothesis(&facts, 0.7));
        assert!(no_lesson.validate(&facts).is_ok());
        assert!(no_lesson.proposed_lesson.is_none());
    }

    #[test]
    fn multiple_hypotheses_coexist() {
        let facts = facts();
        let draft = HindsightDraft::new("the suite passes", "two cases failed")
            .with_hypothesis(hypothesis(&facts, 0.6))
            .with_hypothesis(hypothesis(&facts, 0.3));

        assert!(draft.validate(&facts).is_ok());
        assert_eq!(draft.hypotheses.len(), 2);
        assert_eq!(draft.cited_evidence_ids().len(), 2);
    }

    #[test]
    fn an_abstaining_outcome_that_contradicts_its_record_is_rejected() {
        let facts = facts();

        let claims_nothing_yet_explains = HindsightDraft {
            suggested_outcome: Some(HindsightOutcome::UnknownCause),
            ..HindsightDraft::new("it works", "it did not")
        }
        .with_hypothesis(hypothesis(&facts, 0.6));
        assert!(claims_nothing_yet_explains
            .validate(&facts)
            .unwrap_err()
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::ContradictoryOutcome { .. })));

        let teaches_while_abstaining = HindsightDraft {
            suggested_outcome: Some(HindsightOutcome::NoLesson),
            ..HindsightDraft::new("it works", "it did not")
        }
        .with_hypothesis(hypothesis(&facts, 0.6))
        .with_proposed_lesson("Always retry.");
        assert!(teaches_while_abstaining
            .validate(&facts)
            .unwrap_err()
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::ContradictoryOutcome { .. })));
    }

    #[test]
    fn a_lesson_with_no_hypothesis_behind_it_is_rejected() {
        let facts = facts();
        let draft = HindsightDraft::new("it works", "it did not")
            .with_proposed_lesson("Do the thing that works.");

        assert!(draft
            .validate(&facts)
            .unwrap_err()
            .contains(&HindsightViolation::LessonWithoutHypothesis));
    }

    #[test]
    fn bounds_hold_on_every_bounded_field() {
        let facts = facts();

        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        let draft = HindsightDraft::new(long, "it did not");
        assert!(draft
            .validate(&facts)
            .unwrap_err()
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::FieldTooLong { .. })));

        let mut many = HindsightDraft::new("it works", "it did not");
        for _ in 0..=MAX_HYPOTHESES {
            many = many.with_hypothesis(hypothesis(&facts, 0.5));
        }
        assert!(many
            .validate(&facts)
            .unwrap_err()
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::TooManyItems { .. })));

        let verbose = HindsightDraft::new("it works", "it did not")
            .with_hypothesis(hypothesis(&facts, 0.5))
            .with_proposed_lesson("y".repeat(MAX_LESSON_CHARS + 1));
        assert!(verbose
            .validate(&facts)
            .unwrap_err()
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::FieldTooLong { .. })));
    }

    #[test]
    fn an_unknown_contract_version_is_rejected_rather_than_guessed() {
        let (mut draft, facts) = sound();
        draft.version = HINDSIGHT_DRAFT_VERSION + 1;

        assert!(draft
            .validate(&facts)
            .unwrap_err()
            .iter()
            .any(|violation| matches!(violation, HindsightViolation::UnsupportedVersion { .. })));
    }

    #[test]
    fn validation_reports_every_violation_not_just_the_first() {
        let facts = facts();
        let draft = HindsightDraft {
            version: 99,
            ..HindsightDraft::new("", "")
        };

        let violations = draft.validate(&facts).unwrap_err();
        assert!(
            violations.len() >= 3,
            "expected version + both empty fields, got {violations:?}"
        );
    }

    #[test]
    fn the_schema_is_flat_so_a_server_cannot_silently_stop_enforcing_it() {
        assert!(!HINDSIGHT_DRAFT_SCHEMA.contains("$ref"));
        assert!(!HINDSIGHT_DRAFT_SCHEMA.contains("$defs"));
        assert!(!HINDSIGHT_DRAFT_SCHEMA.contains("definitions"));
        serde_json::from_str::<serde_json::Value>(HINDSIGHT_DRAFT_SCHEMA)
            .expect("the schema must be valid JSON");
    }

    #[test]
    fn the_schema_describes_exactly_the_fields_the_draft_serializes() {
        let (draft, _) = sound();
        let serialized = serde_json::to_value(&draft).unwrap();
        let schema: serde_json::Value = serde_json::from_str(HINDSIGHT_DRAFT_SCHEMA).unwrap();

        let properties = schema["properties"].as_object().unwrap();
        let fields = serialized.as_object().unwrap();

        for name in fields.keys() {
            assert!(
                properties.contains_key(name),
                "{name} is serialized but missing from the schema"
            );
        }
        for name in properties.keys() {
            assert!(
                fields.contains_key(name),
                "{name} is in the schema but never serialized"
            );
        }
    }

    #[test]
    fn only_a_candidate_outcome_proposes_a_lesson() {
        assert!(HindsightOutcome::Candidate.proposes_a_lesson());
        for abstaining in [
            HindsightOutcome::UnknownCause,
            HindsightOutcome::NoLesson,
            HindsightOutcome::NeedsReview,
            HindsightOutcome::Malformed,
        ] {
            assert!(!abstaining.proposes_a_lesson());
        }
    }
}
