//! Distil a finished piece of work into a [`HindsightDraft`] over facts
//! supplied to it — with one bounded repair, and a fallback that never invents
//! a cause.
//!
//! The distiller performs no I/O. It hands out [`DistillRequest`]s and takes
//! [`DistillReply`]s, so the same contract runs over LocalMind's own chat
//! endpoint or over whatever model a host already holds, synchronous or not:
//!
//! ```text
//! let mut step = distiller.start();
//! while let DistillStep::Ask(request) = step {
//!     step = distiller.reply(transport(request));
//! }
//! ```
//!
//! What it guarantees, on every path:
//!
//! - **Every reply is validated.** A constrained request is attempted when the
//!   caller says it may be, and that changes nothing about validation: a server
//!   can accept a schema, ignore it, and answer 200.
//! - **At most one repair pass**, shared across the whole distillation, and spent
//!   only on a reply that arrived and broke the contract. A transport that
//!   refused the constraint retried for free; the distiller records that, stops
//!   asking for a schema, and does not charge the repair budget for it.
//! - **The outcome is not the model's.** [`crate::decide_outcome`] settles it
//!   from the validated draft and the facts it cites.
//! - **Failure keeps the facts and invents nothing.** A model that cannot be
//!   reached, or still breaks the contract after the repair, yields a draft
//!   holding only the caller's intended and observed outcome — no hypothesis, no
//!   lesson — marked `NeedsReview` or `Malformed`.
//!
//! Capability adapts without naming models. A capable endpoint gets one compact
//! pass; a smaller one gets two single-purpose passes — first what happened,
//! then what follows from it — and the second is skipped when the first found
//! no cause. The choice is the caller's, through [`DistillPlan`], and model
//! names never branch it.

use localmind_core::{
    CausalHypothesis, EvidenceRef, HindsightDraft, HindsightOutcome, HindsightViolation,
    HINDSIGHT_DRAFT_SCHEMA, HINDSIGHT_DRAFT_VERSION, MAX_TEXT_CHARS,
};
use localmind_inference::{
    extract_json_payload, ChatMessage, ConstraintDisposition, JsonSchemaConstraint, RepairBudget,
};
use serde::Deserialize;

use crate::abstention::{decide, OutcomeReason};

/// Below this declared context, [`DistillPlan::adaptive`] chooses the staged
/// passes. A proxy, not a measurement: endpoints served with small contexts
/// tend to be the smaller models, and a smaller model does better with one
/// question at a time.
pub const STAGED_BELOW_CONTEXT_TOKENS: u32 = 16_384;

/// Fraction of the declared context the facts may take. The rest is the
/// instructions and the reply.
const FACT_SHARE_OF_CONTEXT: usize = 2;

/// Rough characters per token, for sizing the fact listing only.
const CHARS_PER_TOKEN: usize = 4;

/// How the analysis is split into requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Strategy {
    /// One request fills the whole contract.
    OnePass,
    /// One request names what happened; a second, only if a cause was found,
    /// proposes what follows.
    Staged,
}

/// How to run one distillation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DistillPlan {
    pub strategy: Strategy,
    /// Attempt a JSON-schema constraint on each request. Latency and quality
    /// only: the reply is validated either way.
    pub attempt_schema: bool,
    /// The endpoint's declared context, when known. Bounds the fact listing:
    /// facts keep their labels, and excerpts are dropped from the end once the
    /// listing would pass half the context.
    pub context_tokens: Option<u32>,
}

impl DistillPlan {
    /// Staged below [`STAGED_BELOW_CONTEXT_TOKENS`], one pass otherwise, and one
    /// pass when the context is unknown.
    #[must_use]
    pub fn adaptive(context_tokens: Option<u32>, attempt_schema: bool) -> Self {
        let strategy = match context_tokens {
            Some(tokens) if tokens < STAGED_BELOW_CONTEXT_TOKENS => Strategy::Staged,
            _ => Strategy::OnePass,
        };
        Self {
            strategy,
            attempt_schema,
            context_tokens,
        }
    }
}

/// What the analysis is over.
#[derive(Clone, Debug, PartialEq)]
pub struct DistillInput {
    /// The supplied facts. Every one must carry a canonical, intact id.
    pub facts: Vec<EvidenceRef>,
    /// What the facts cannot say, shown as such. Never citable.
    pub gaps: Vec<InputGap>,
    /// What the work set out to do, in the caller's words. Used verbatim by the
    /// fallback, which has nothing else it may say.
    pub intended: String,
    /// What the work ended with, in the caller's words.
    pub observed: String,
}

/// Something the facts cannot say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputGap {
    /// One line, worded as what is not known.
    pub description: String,
    /// Whether, and where, this gap means the record itself is incomplete —
    /// lines lost, a result never written, a step whose sessions are unknown.
    /// `None` for a gap that says nothing about completeness, such as a call
    /// no verifier looked at.
    pub incompleteness: Option<Incompleteness>,
}

impl InputGap {
    /// A gap that says nothing about whether the record is complete.
    #[must_use]
    pub fn note(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            incompleteness: None,
        }
    }
}

/// Where a record is incomplete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incompleteness {
    /// Everything the analysis rests on.
    Run,
    /// Facts from this producing source (an `EvidenceRef::source`).
    Source(String),
}

/// Which part of the analysis a request asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestPurpose {
    OnePass,
    WhatHappened,
    WhatFollows,
}

/// One request for the transport to send.
#[derive(Clone, Debug, PartialEq)]
pub struct DistillRequest {
    pub purpose: RequestPurpose,
    /// Whether this is the one repair pass.
    pub repair: bool,
    pub messages: Vec<ChatMessage>,
    /// The constraint to attempt, if any. A transport that cannot send it, or
    /// whose server refuses it, answers without it and says so.
    pub schema: Option<JsonSchemaConstraint>,
}

/// What came back.
#[derive(Clone, Debug, PartialEq)]
pub enum DistillReply {
    /// A reply, with an honest account of the constraint.
    Text {
        content: String,
        disposition: ConstraintDisposition,
    },
    /// The model could not be reached or failed before answering.
    Unavailable { detail: String },
}

/// The next thing to do.
#[derive(Clone, Debug, PartialEq)]
pub enum DistillStep {
    Ask(DistillRequest),
    Done(Box<Distillation>),
}

/// How a distillation went, for evidence and for the person reading it.
#[derive(Clone, Debug, PartialEq)]
pub struct DistillTrace {
    pub strategy: Strategy,
    /// Replies received, repairs included.
    pub model_calls: u32,
    pub repair_spent: bool,
    /// What became of the constraint on each reply, in order.
    pub dispositions: Vec<ConstraintDisposition>,
    /// The draft is the fallback, not the model's.
    pub fallback: bool,
    /// Facts listed without their excerpt to stay within the context.
    pub excerpts_dropped: usize,
}

/// A finished distillation.
#[derive(Clone, Debug, PartialEq)]
pub struct Distillation {
    /// Valid against the supplied facts on every path.
    pub draft: HindsightDraft,
    /// Decided here, never by the model.
    pub outcome: HindsightOutcome,
    pub reasons: Vec<OutcomeReason>,
    pub trace: DistillTrace,
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    /// Waiting on the reply to this request.
    Awaiting(DistillRequest),
    Finished,
}

/// One distillation in progress.
#[derive(Clone, Debug)]
pub struct Distiller {
    input: DistillInput,
    plan: DistillPlan,
    listing: String,
    budget: RepairBudget,
    trace: DistillTrace,
    /// The validated first half of a staged analysis.
    causes: Option<HindsightDraft>,
    state: State,
}

impl Distiller {
    #[must_use]
    pub fn new(input: DistillInput, plan: DistillPlan) -> Self {
        let (listing, excerpts_dropped) = list_facts(&input, plan.context_tokens);
        Self {
            trace: DistillTrace {
                strategy: plan.strategy,
                model_calls: 0,
                repair_spent: false,
                dispositions: Vec::new(),
                fallback: false,
                excerpts_dropped,
            },
            input,
            plan,
            listing,
            budget: RepairBudget::new(),
            causes: None,
            state: State::Finished,
        }
    }

    /// The first request.
    #[must_use]
    pub fn start(&mut self) -> DistillStep {
        let purpose = match self.plan.strategy {
            Strategy::OnePass => RequestPurpose::OnePass,
            Strategy::Staged => RequestPurpose::WhatHappened,
        };
        self.ask(self.request(purpose))
    }

    /// Hand in the reply to the last request.
    #[must_use]
    pub fn reply(&mut self, reply: DistillReply) -> DistillStep {
        let State::Awaiting(request) = std::mem::replace(&mut self.state, State::Finished) else {
            // Nothing was asked. Answering again cannot change a finished result,
            // so finish as unavailable rather than guess.
            return self.fail(OutcomeReason::ModelUnavailable {
                detail: "a reply arrived with no request outstanding".to_string(),
            });
        };
        let content = match reply {
            DistillReply::Unavailable { detail } => {
                return self.fail(OutcomeReason::ModelUnavailable { detail });
            }
            DistillReply::Text {
                content,
                disposition,
            } => {
                self.trace.model_calls += 1;
                self.trace.dispositions.push(disposition);
                if disposition == ConstraintDisposition::RefusedByTransport {
                    // Asking again would only be refused again. The refusal was
                    // free; it says nothing about the content that came back.
                    self.plan.attempt_schema = false;
                }
                content
            }
        };

        match self.interpret(request.purpose, &content) {
            Ok(Interpreted::Finished(draft)) => self.finish(draft),
            Ok(Interpreted::NeedsProposal(causes)) => {
                self.causes = Some(causes);
                self.ask(self.request(RequestPurpose::WhatFollows))
            }
            Err(problems) => {
                if self.budget.spend() {
                    self.trace.repair_spent = true;
                    self.ask(self.repair(&request, &content, &problems))
                } else {
                    self.fail(OutcomeReason::MalformedAfterRepair {
                        violations: problems,
                    })
                }
            }
        }
    }

    fn ask(&mut self, request: DistillRequest) -> DistillStep {
        self.state = State::Awaiting(request.clone());
        DistillStep::Ask(request)
    }

    fn interpret(
        &self,
        purpose: RequestPurpose,
        content: &str,
    ) -> Result<Interpreted, Vec<String>> {
        let json = extract_json_payload(content)
            .ok_or_else(|| vec!["the reply contains no JSON object".to_string()])?;
        match purpose {
            RequestPurpose::OnePass => {
                let draft = parse_draft(json)?;
                self.validated(draft).map(Interpreted::Finished)
            }
            RequestPurpose::WhatHappened => {
                let causes: Causes = parse(json)?;
                let mut draft =
                    HindsightDraft::new(causes.intended_outcome, causes.observed_outcome);
                draft.hypotheses = causes.hypotheses;
                draft.missed_signals = causes.missed_signals;
                let draft = self.validated(draft)?;
                if draft.hypotheses.is_empty() {
                    // No cause, so there is nothing for a second pass to follow
                    // from. Save the call.
                    Ok(Interpreted::Finished(draft))
                } else {
                    Ok(Interpreted::NeedsProposal(draft))
                }
            }
            RequestPurpose::WhatFollows => {
                let proposal: Proposal = parse(json)?;
                let mut draft = self.causes.clone().ok_or_else(|| {
                    vec!["a proposal arrived before any cause was named".to_string()]
                })?;
                draft.intervention = proposal.intervention;
                draft.counterfactual_prediction = proposal.counterfactual_prediction;
                draft.applicability = proposal.applicability;
                draft.preconditions = proposal.preconditions;
                draft.invalidation = proposal.invalidation;
                draft.proposed_lesson = proposal.proposed_lesson;
                draft.suggested_outcome = proposal.suggested_outcome;
                self.validated(draft).map(Interpreted::Finished)
            }
        }
    }

    fn validated(&self, draft: HindsightDraft) -> Result<HindsightDraft, Vec<String>> {
        draft
            .validate(&self.input.facts)
            .map(|()| draft)
            .map_err(|violations| {
                violations
                    .iter()
                    .map(HindsightViolation::to_string)
                    .collect()
            })
    }

    fn finish(&mut self, draft: HindsightDraft) -> DistillStep {
        let (mut outcome, mut reasons) = decide(&draft, &self.input.facts);
        if outcome == HindsightOutcome::Candidate {
            // A lesson may rest only on a record that is whole where it looks.
            // Where lines were lost or a result was never written, what the
            // draft calls a cause may be the part that is missing. The lesson is
            // kept for a person to judge, and cannot be queued as promotable.
            let incomplete = self.gaps_under(&draft);
            if !incomplete.is_empty() {
                outcome = HindsightOutcome::NeedsReview;
                reasons.push(OutcomeReason::IncompleteRecord { gaps: incomplete });
            }
        }
        DistillStep::Done(Box::new(Distillation {
            draft,
            outcome,
            reasons,
            trace: self.trace.clone(),
        }))
    }

    /// The gaps that make the record incomplete under this draft: any
    /// run-wide one, and any in a source a cited fact came from.
    fn gaps_under(&self, draft: &HindsightDraft) -> Vec<String> {
        let cited_sources: Vec<&str> = draft
            .cited_evidence_ids()
            .into_iter()
            .filter_map(|id| self.input.facts.iter().find(|fact| &fact.id == id))
            .filter_map(EvidenceRef::source)
            .collect();
        self.input
            .gaps
            .iter()
            .filter(|gap| match &gap.incompleteness {
                Some(Incompleteness::Run) => true,
                Some(Incompleteness::Source(source)) => {
                    cited_sources.iter().any(|cited| cited == source)
                }
                None => false,
            })
            .map(|gap| gap.description.clone())
            .collect()
    }

    /// Stop without a usable reply. A staged analysis that already named its
    /// causes keeps them — they were validated — and is marked incomplete; any
    /// other failure keeps only what the caller supplied.
    fn fail(&mut self, reason: OutcomeReason) -> DistillStep {
        self.state = State::Finished;
        if let Some(causes) = self.causes.take() {
            return DistillStep::Done(Box::new(Distillation {
                draft: causes,
                outcome: HindsightOutcome::NeedsReview,
                reasons: vec![OutcomeReason::Incomplete, reason],
                trace: self.trace.clone(),
            }));
        }
        let outcome = match reason {
            OutcomeReason::MalformedAfterRepair { .. } => HindsightOutcome::Malformed,
            _ => HindsightOutcome::NeedsReview,
        };
        self.trace.fallback = true;
        DistillStep::Done(Box::new(Distillation {
            draft: fallback_draft(&self.input),
            outcome,
            reasons: vec![reason],
            trace: self.trace.clone(),
        }))
    }

    fn request(&self, purpose: RequestPurpose) -> DistillRequest {
        let (task, shape, schema) = match purpose {
            RequestPurpose::OnePass => (ONE_PASS_TASK, ONE_PASS_SHAPE, HINDSIGHT_DRAFT_SCHEMA),
            RequestPurpose::WhatHappened => (CAUSES_TASK, CAUSES_SHAPE, CAUSES_SCHEMA),
            RequestPurpose::WhatFollows => (PROPOSAL_TASK, PROPOSAL_SHAPE, PROPOSAL_SCHEMA),
        };
        let mut user = format!(
            "{task}\n\nIntended: {}\nObserved: {}\n\n{}\n",
            self.input.intended, self.input.observed, self.listing
        );
        if purpose == RequestPurpose::WhatFollows {
            if let Some(causes) = &self.causes {
                user.push_str("\nCauses already found:\n");
                for hypothesis in &causes.hypotheses {
                    user.push_str(&format!(
                        "- {} (cites {})\n",
                        hypothesis.claim,
                        hypothesis
                            .evidence_ids
                            .iter()
                            .map(|id| id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
        user.push_str(&format!(
            "\nReply with this JSON shape and nothing else:\n{shape}\n\n{RULES}"
        ));
        DistillRequest {
            purpose,
            repair: false,
            messages: vec![ChatMessage::system(SYSTEM), ChatMessage::user(user)],
            schema: self.schema(purpose, schema),
        }
    }

    fn repair(
        &self,
        failed: &DistillRequest,
        content: &str,
        problems: &[String],
    ) -> DistillRequest {
        let mut messages = failed.messages.clone();
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: content.to_string(),
        });
        messages.push(ChatMessage::user(format!(
            "That reply could not be used:\n- {}\n\nSend the same analysis again as one JSON object in \
the required shape, citing only the listed ids, and nothing else.",
            problems.join("\n- ")
        )));
        DistillRequest {
            purpose: failed.purpose,
            repair: true,
            messages,
            schema: self.schema(
                failed.purpose,
                match failed.purpose {
                    RequestPurpose::OnePass => HINDSIGHT_DRAFT_SCHEMA,
                    RequestPurpose::WhatHappened => CAUSES_SCHEMA,
                    RequestPurpose::WhatFollows => PROPOSAL_SCHEMA,
                },
            ),
        }
    }

    fn schema(&self, purpose: RequestPurpose, schema: &str) -> Option<JsonSchemaConstraint> {
        if !self.plan.attempt_schema {
            return None;
        }
        let name = match purpose {
            RequestPurpose::OnePass => "hindsight",
            RequestPurpose::WhatHappened => "hindsight_causes",
            RequestPurpose::WhatFollows => "hindsight_proposal",
        };
        JsonSchemaConstraint::new(name, schema).ok()
    }
}

enum Interpreted {
    Finished(HindsightDraft),
    NeedsProposal(HindsightDraft),
}

/// The first half of a staged analysis.
#[derive(Deserialize)]
struct Causes {
    intended_outcome: String,
    observed_outcome: String,
    #[serde(default)]
    hypotheses: Vec<CausalHypothesis>,
    #[serde(default)]
    missed_signals: Vec<String>,
}

/// The second half of a staged analysis.
#[derive(Deserialize)]
struct Proposal {
    #[serde(default)]
    intervention: Option<String>,
    #[serde(default)]
    counterfactual_prediction: Option<String>,
    #[serde(default)]
    applicability: Option<String>,
    #[serde(default)]
    preconditions: Vec<String>,
    #[serde(default)]
    invalidation: Option<String>,
    #[serde(default)]
    proposed_lesson: Option<String>,
    #[serde(default)]
    suggested_outcome: Option<HindsightOutcome>,
}

fn parse<T: for<'de> Deserialize<'de>>(json: &str) -> Result<T, Vec<String>> {
    serde_json::from_str(json)
        .map_err(|error| vec![format!("the JSON does not fit the shape: {error}")])
}

/// Parse a whole draft. The contract version is ours to state, not the model's
/// to remember, so an omitted `version` is filled in; a wrong one stays wrong
/// and validation rejects it.
fn parse_draft(json: &str) -> Result<HindsightDraft, Vec<String>> {
    let mut value: serde_json::Value = parse(json)?;
    if let Some(object) = value.as_object_mut() {
        object
            .entry("version")
            .or_insert_with(|| serde_json::Value::from(HINDSIGHT_DRAFT_VERSION));
    }
    serde_json::from_value(value)
        .map_err(|error| vec![format!("the JSON does not fit the shape: {error}")])
}

/// Everything the fallback may say: the caller's own intended and observed
/// outcome, bounded, and nothing else.
fn fallback_draft(input: &DistillInput) -> HindsightDraft {
    HindsightDraft::new(bounded(&input.intended), bounded(&input.observed))
}

fn bounded(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "Not recorded".to_string();
    }
    text.chars().take(MAX_TEXT_CHARS).collect()
}

/// The fact listing, and how many excerpts were left out to fit.
fn list_facts(input: &DistillInput, context_tokens: Option<u32>) -> (String, usize) {
    let budget = context_tokens
        .map(|tokens| (tokens as usize).saturating_mul(CHARS_PER_TOKEN) / FACT_SHARE_OF_CONTEXT);
    let mut out = String::from("Facts (cite these ids and no others):\n");
    for fact in &input.facts {
        out.push_str(&format!(
            "- {} [{}] {}\n",
            fact.id,
            fact.kind.as_str(),
            fact.label
        ));
    }
    let mut dropped = 0;
    let mut excerpts = String::new();
    for fact in &input.facts {
        let Some(excerpt) = fact.excerpt.as_deref() else {
            continue;
        };
        let entry = format!("{}:\n{}\n", fact.id, indent(excerpt));
        if budget.is_some_and(|budget| out.len() + excerpts.len() + entry.len() > budget) {
            dropped += 1;
            continue;
        }
        excerpts.push_str(&entry);
    }
    if !excerpts.is_empty() {
        out.push_str("\nWhat some facts observed:\n");
        out.push_str(&excerpts);
    }
    if !input.gaps.is_empty() {
        out.push_str(
            "\nNot recorded (these are gaps, not facts; do not cite them or treat them as failures):\n",
        );
        for gap in &input.gaps {
            out.push_str(&format!("- {}\n", gap.description));
        }
    }
    (out, dropped)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

const SYSTEM: &str = "You review finished software work after the fact. You are given facts, each \
with an id. You may use only those facts, and you cite them by id. You never invent an id, and you \
never state a guess as certain. Reply with one JSON object and nothing else.";

const ONE_PASS_TASK: &str = "Say what the work intended, what happened, why, and whether anything \
reusable follows from it.";

const CAUSES_TASK: &str =
    "Say what the work intended, what happened, and which of the facts explain \
the difference. Do not propose anything yet.";

const PROPOSAL_TASK: &str = "The causes below were found in the facts. Say whether a change would \
have avoided the outcome, and whether that change is worth keeping as a lesson for next time.";

const RULES: &str = "Rules:
- Cite only ids from the facts list. Every hypothesis cites at least one id.
- A hypothesis citing a single fact has confidence 0.8 or lower.
- Several causes may be listed when the facts allow more than one.
- If the facts do not show why, leave hypotheses empty.
- Propose a lesson when a change would have avoided the outcome and would apply to similar work \
next time. Otherwise leave proposed_lesson null.
- The lesson is one plain sentence a person can act on. It names no fact ids.
- Keep every text under 500 characters and the lesson under 280.";

const ONE_PASS_SHAPE: &str = r#"{"intended_outcome": "...", "observed_outcome": "...",
 "hypotheses": [{"claim": "...", "evidence_ids": ["ev-..."], "confidence": 0.6}],
 "missed_signals": [], "intervention": null, "counterfactual_prediction": null,
 "applicability": null, "preconditions": [], "invalidation": null, "proposed_lesson": null,
 "suggested_outcome": "Candidate | UnknownCause | NoLesson | NeedsReview"}"#;

const CAUSES_SHAPE: &str = r#"{"intended_outcome": "...", "observed_outcome": "...",
 "hypotheses": [{"claim": "...", "evidence_ids": ["ev-..."], "confidence": 0.6}],
 "missed_signals": []}"#;

const PROPOSAL_SHAPE: &str = r#"{"intervention": null, "counterfactual_prediction": null,
 "applicability": null, "preconditions": [], "invalidation": null, "proposed_lesson": null,
 "suggested_outcome": "Candidate | NoLesson | NeedsReview"}"#;

/// The first staged pass. Flat, like the full schema.
const CAUSES_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["intended_outcome", "observed_outcome", "hypotheses"],
  "properties": {
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
          "evidence_ids": { "type": "array", "maxItems": 8, "items": { "type": "string" } },
          "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
        }
      }
    },
    "missed_signals": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 500 } }
  }
}"#;

/// The second staged pass.
const PROPOSAL_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "intervention": { "type": ["string", "null"], "maxLength": 500 },
    "counterfactual_prediction": { "type": ["string", "null"], "maxLength": 500 },
    "applicability": { "type": ["string", "null"], "maxLength": 500 },
    "preconditions": { "type": "array", "maxItems": 8, "items": { "type": "string", "maxLength": 500 } },
    "invalidation": { "type": ["string", "null"], "maxLength": 500 },
    "proposed_lesson": { "type": ["string", "null"], "maxLength": 280 },
    "suggested_outcome": {
      "type": ["string", "null"],
      "enum": ["Candidate", "UnknownCause", "NoLesson", "NeedsReview", "Malformed", null]
    }
  }
}"#;
