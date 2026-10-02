//! What a reviewer is shown about a candidate: its hindsight and its lab
//! results, as two linked cards.
//!
//! Built once here so every surface — a command line, a terminal review, a web
//! page — shows the same facts in the same words. [`review_cards`] produces the
//! data; [`render_text`] is the plain-text form, which is complete on its own:
//! nothing is carried only by colour, a badge, or a layout.
//!
//! Three rules shape the wording:
//!
//! - **A verdict is named and then explained.** The stored name is shown so it
//!   matches logs and listings, followed by one plain sentence.
//! - **A result is narrow.** `Supported` is about these tasks, this model and
//!   this revision. Logic and Replay check reasoning and reproducibility; they
//!   are never worded as a measure of whether a lesson helps.
//! - **Absence is not a defect.** An untested lesson, an unknown cause and "no
//!   lesson" are ordinary, safe outcomes and are worded as such.
//!
//! The cards describe; they never decide. The closing line lists what a
//! reviewer can do and preselects nothing.

use localmind_core::{
    AssignmentSource, CandidateLesson, EvidenceRef, EvidenceTier, ExperimentEvidence,
    HindsightOutcome, HindsightProvenance, InjectionMode, LabVerdict, OracleOrigin, ReviewItemId,
    ReviewState, VerdictReason,
};
use serde::Serialize;

/// What the cards need to know about the review item besides its candidate.
#[derive(Clone, Copy, Debug)]
pub struct CardContext<'a> {
    pub candidate: &'a CandidateLesson,
    pub state: &'a ReviewState,
    /// The recorded action that closed the item, when it is closed.
    pub closed_as: Option<&'a str>,
    /// The items a rewrite or split replaced this one with.
    pub descendants: &'a [ReviewItemId],
}

/// Whether a retained detail (a log, a receipt) can still be opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailState {
    Available,
    /// Removed by retention or never kept. The result itself still stands.
    NoLongerRetained,
    /// This surface cannot check.
    NotChecked,
}

/// Everything a reviewer is shown about one candidate.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewCards {
    /// Where this item sits among rewrites and splits. Empty for an original
    /// that was never rewritten.
    pub lineage: Vec<String>,
    /// Why review automation is holding this item for a person, when it is.
    pub hold: Option<String>,
    pub hindsight: Option<HindsightCard>,
    /// Stated when there is no hindsight, so its absence is not read as a gap.
    pub no_hindsight: Option<String>,
    pub experiments: Vec<ExperimentCard>,
    /// Stated when nothing was tested, so its absence is not read as a defect.
    pub untested: Option<String>,
    /// What the reviewer can do. Never a preselected action.
    pub next: String,
}

/// One fact the hindsight was drafted over.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FactLine {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub redacted: bool,
    pub excerpt: Option<String>,
}

/// One causal claim and what it rests on.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CauseLine {
    pub claim: String,
    pub confidence: f32,
    pub cites: Vec<String>,
}

/// The hindsight card.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HindsightCard {
    pub intended: String,
    pub observed: String,
    /// The outcome the deterministic check decided, named and explained.
    pub outcome: Option<String>,
    /// Whether that outcome is one where nothing is proposed for memory.
    pub safe_abstention: bool,
    /// How the analysis was produced: a model, or the no-model fallback.
    pub analysis: Option<String>,
    /// The facts are fixed: they were recorded by the run, not written by the
    /// analysis, and a rewrite cannot change them.
    pub facts: Vec<FactLine>,
    pub cause: Option<CauseLine>,
    pub alternatives: Vec<CauseLine>,
    pub missed_signals: Vec<String>,
    pub intervention: Option<String>,
    pub counterfactual: Option<String>,
    pub applicability: Option<String>,
    pub preconditions: Vec<String>,
    pub invalidation: Option<String>,
}

/// One arm of a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ArmLine {
    pub arm: String,
    pub passed: u32,
    pub attempts: u32,
    pub wall_ms: u64,
    pub cancelled: bool,
    pub truncated: bool,
    pub observations: Vec<String>,
}

/// A retained detail a reviewer can open locally.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DetailLine {
    pub locator: String,
    pub summary: String,
    pub state: DetailState,
}

/// The experiment card: one lab result.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ExperimentCard {
    pub tier: String,
    /// What this tier can and cannot show.
    pub tier_meaning: String,
    pub verdict: String,
    /// The verdict in one plain sentence.
    pub meaning: String,
    pub reasons: Vec<String>,
    /// The result describes an earlier version of the lesson.
    pub stale: bool,
    pub harmful: bool,
    /// What was run.
    pub task: Option<String>,
    /// Where the assignment came from.
    pub assignment_source: Option<String>,
    /// What decided pass or fail, and who wrote it.
    pub oracle: Option<String>,
    pub arms: Vec<ArmLine>,
    pub injection: Option<String>,
    pub model: Option<String>,
    pub source_revision: String,
    /// Total attempts across arms.
    pub repetitions: u32,
    /// Wall time across arms, in milliseconds.
    pub wall_ms: u64,
    pub limitations: Vec<String>,
    pub details: Vec<DetailLine>,
    pub produced_by: String,
}

fn tier_name(tier: EvidenceTier) -> &'static str {
    match tier {
        EvidenceTier::Logic => "Logic",
        EvidenceTier::Replay => "Replay",
        EvidenceTier::Uplift => "Uplift",
    }
}

/// What a tier can and cannot show. Logic and Replay are never worded as
/// evidence that a lesson helps.
#[must_use]
pub fn tier_meaning(tier: EvidenceTier) -> &'static str {
    match tier {
        EvidenceTier::Logic => {
            "checks the lesson's reasoning against what the run recorded; it does not measure \
             whether the lesson helps"
        }
        EvidenceTier::Replay => {
            "re-runs the project's own check on the commits the lesson came from; it does not \
             measure whether the lesson helps"
        }
        EvidenceTier::Uplift => "compares a model on the same tasks with and without the lesson",
    }
}

fn verdict_name(verdict: LabVerdict) -> &'static str {
    match verdict {
        LabVerdict::Valid => "Valid",
        LabVerdict::Invalid => "Invalid",
        LabVerdict::NotExecutable => "NotExecutable",
        LabVerdict::Supported => "Supported",
        LabVerdict::Contradicted => "Contradicted",
        LabVerdict::Inconclusive => "Inconclusive",
        LabVerdict::InvalidExperiment => "InvalidExperiment",
    }
}

/// A verdict in one plain sentence.
#[must_use]
pub fn verdict_meaning(verdict: LabVerdict) -> &'static str {
    match verdict {
        LabVerdict::Valid => {
            "the check came out the way the lesson's reasoning says it should. That is \
             consistency, not proof the lesson helps."
        }
        LabVerdict::Invalid => {
            "the check did not come out the way the lesson's reasoning says it should. The \
             diagnosis may be wrong even if the advice is sound."
        }
        LabVerdict::NotExecutable => {
            "this lesson cannot be tested mechanically. That describes the lesson, and is not a \
             mark against it."
        }
        LabVerdict::Supported => {
            "the model did better with the lesson on these tasks. Narrow evidence: these tasks, \
             this model, this revision — not general truth."
        }
        LabVerdict::Contradicted => {
            "the model did worse with the lesson on these tasks. The lesson is held for a person \
             in every review mode."
        }
        LabVerdict::Inconclusive => {
            "no clear difference with and without the lesson. This is not evidence for it or \
             against it."
        }
        LabVerdict::InvalidExperiment => {
            "the run did not produce a usable comparison. It says nothing about the lesson."
        }
    }
}

/// A reason code in plain words.
#[must_use]
pub fn reason_meaning(reason: &VerdictReason) -> String {
    let text = match reason {
        VerdictReason::FixtureUnavailable => "the fixture the test needs is not available",
        VerdictReason::OracleMutable => "what decides pass or fail could have changed",
        VerdictReason::OracleNotIndependent => {
            "what decides pass or fail was derived from the lesson itself"
        }
        VerdictReason::NoDiscriminatingVerifier => {
            "no check tells a right outcome from a wrong one"
        }
        VerdictReason::NotReplayable => "the situation cannot be reproduced",
        VerdictReason::InjectionNotObserved => "the lesson was not seen to reach the model",
        VerdictReason::ArmContaminated => "an arm saw memory it should not have",
        VerdictReason::PartialPair => "only one of the two arms finished",
        VerdictReason::BudgetExceeded => "a declared ceiling was passed and the run was stopped",
        VerdictReason::Cancelled => "the run was cancelled",
        VerdictReason::VerifierFailed => "the check itself failed to run",
        VerdictReason::Preference => "the lesson states a preference, not a testable claim",
        VerdictReason::HumanIntent => "the lesson depends on what a person meant",
        VerdictReason::UnverifiableStyle => "the lesson is about style no check can judge",
        VerdictReason::UnsafeAction => "testing it would need an unsafe action",
        VerdictReason::NoTrustedSource => "no trusted source exists to test it against",
        VerdictReason::OracleChangedByFix => "the fix also changed what decides pass or fail",
        VerdictReason::Other(code) => return format!("{code} (recorded by the runner)"),
    };
    format!("{reason:?} — {text}")
}

/// A hindsight outcome, named and explained.
#[must_use]
pub fn outcome_meaning(outcome: HindsightOutcome) -> &'static str {
    match outcome {
        HindsightOutcome::Candidate => {
            "Candidate — the evidence supports a lesson worth reviewing."
        }
        HindsightOutcome::UnknownCause => {
            "UnknownCause — the evidence does not show why the outcome differed. A safe outcome: \
             nothing is proposed for memory."
        }
        HindsightOutcome::NoLesson => {
            "NoLesson — the cause is clear and nothing reusable follows from it. A safe outcome: \
             nothing is proposed for memory."
        }
        HindsightOutcome::NeedsReview => {
            "NeedsReview — the record is sound, and a person should read it before it goes further."
        }
        HindsightOutcome::Malformed => {
            "Malformed — the analysis broke its contract and carries no guidance."
        }
    }
}

fn analysis_line(provenance: &HindsightProvenance) -> String {
    let mut text = if provenance.fallback {
        "no model analysis was used: the record holds only what the run intended and how it \
         ended. This is the designed fallback, not a judgement of any model"
            .to_string()
    } else {
        format!("drafted by a model in {} call(s)", provenance.model_calls)
    };
    if provenance.repaired {
        text.push_str(", after one repair request");
    }
    if provenance.excerpts_dropped > 0 {
        text.push_str(&format!(
            "; {} fact excerpt(s) were left out to fit the context",
            provenance.excerpts_dropped
        ));
    }
    text
}

fn short(id: &str) -> String {
    id.chars().take(11).collect()
}

fn fact_line(fact: &EvidenceRef) -> FactLine {
    FactLine {
        id: short(fact.id.as_str()),
        kind: format!("{:?}", fact.kind),
        label: fact.label.clone(),
        redacted: fact.redacted,
        excerpt: fact.excerpt.clone(),
    }
}

fn hindsight_card(candidate: &CandidateLesson) -> Option<HindsightCard> {
    let draft = candidate.hindsight.as_ref()?;
    let provenance = candidate.hindsight_provenance.as_ref();
    let cited = draft.cited_evidence_ids();
    let facts: Vec<FactLine> = candidate
        .evidence()
        .iter()
        .filter(|fact| cited.is_empty() || cited.contains(&&fact.id))
        .map(fact_line)
        .collect();
    let mut causes = draft.hypotheses.iter().map(|hypothesis| CauseLine {
        claim: hypothesis.claim.clone(),
        confidence: hypothesis.confidence.value(),
        cites: hypothesis
            .evidence_ids
            .iter()
            .map(|id| short(id.as_str()))
            .collect(),
    });
    let cause = causes.next();
    Some(HindsightCard {
        intended: draft.intended_outcome.clone(),
        observed: draft.observed_outcome.clone(),
        outcome: provenance.map(|p| {
            let mut text = outcome_meaning(p.outcome).to_string();
            if !p.reasons.is_empty() {
                text.push_str(&format!(" Why: {}.", p.reasons.join("; ")));
            }
            text
        }),
        safe_abstention: provenance.is_some_and(|p| {
            matches!(
                p.outcome,
                HindsightOutcome::UnknownCause | HindsightOutcome::NoLesson
            )
        }),
        analysis: provenance.map(analysis_line),
        facts,
        cause,
        alternatives: causes.collect(),
        missed_signals: draft.missed_signals.clone(),
        intervention: draft.intervention.clone(),
        counterfactual: draft.counterfactual_prediction.clone(),
        applicability: draft.applicability.clone(),
        preconditions: draft.preconditions.clone(),
        invalidation: draft.invalidation.clone(),
    })
}

fn source_line(source: &AssignmentSource) -> String {
    match source {
        AssignmentSource::RecordedTrajectory { session } => {
            format!("the recorded run (session {})", short(session))
        }
        AssignmentSource::FailFixPair { .. } => {
            "a failing commit and the commit that fixed it".to_string()
        }
        AssignmentSource::RatifiedCheck { name } => {
            format!("the project's ratified check `{name}`")
        }
        AssignmentSource::ControlledMutation { .. } => {
            "a controlled change that reintroduces the failure".to_string()
        }
        AssignmentSource::ApprovedTaskSet {
            approved_by,
            drafted_by,
        } => match drafted_by {
            Some(model) => format!("a task set drafted by {model} and approved by {approved_by}"),
            None => format!("a task set written and approved by {approved_by}"),
        },
    }
}

fn oracle_line(origin: OracleOrigin, locator: &str) -> String {
    let who = match origin {
        OracleOrigin::Preexisting => "existed before the lesson",
        OracleOrigin::Human => "approved by a person",
        OracleOrigin::DerivedFromLesson => "derived from the lesson itself (not independent)",
    };
    format!("{locator} — {who}")
}

fn experiment_card(
    result: &ExperimentEvidence,
    candidate: &CandidateLesson,
    detail: &dyn Fn(&str) -> DetailState,
) -> ExperimentCard {
    let stale = result.is_stale_for(candidate);
    let arms: Vec<ArmLine> = result
        .arms
        .iter()
        .map(|arm| ArmLine {
            arm: arm.arm.clone(),
            passed: arm.passed,
            attempts: arm.attempts,
            wall_ms: arm.wall_ms,
            cancelled: arm.cancelled,
            truncated: arm.truncated,
            observations: arm.observations.clone(),
        })
        .collect();
    // Arms of one run usually cite the same receipt: list each file once.
    let mut details: Vec<DetailLine> = Vec::new();
    for log in result.arms.iter().flat_map(|arm| arm.logs.iter()) {
        if details.iter().all(|seen| seen.locator != log.locator) {
            details.push(DetailLine {
                locator: log.locator.clone(),
                summary: log.summary.clone(),
                state: detail(&log.locator),
            });
        }
    }
    let injection = result.injection.as_ref().map(|proof| {
        let how = match proof.mode {
            InjectionMode::Forced => "the lesson was placed in front of the model directly",
            InjectionMode::Retrieved => {
                "the lesson reached the model through retrieval, so a null result may reflect \
                 retrieval rather than the lesson"
            }
        };
        if proof.assertion_passed {
            format!("{how}; it was seen in the lesson arm and absent from the baseline")
        } else {
            format!("{how}; the check that it reached the model failed")
        }
    });
    ExperimentCard {
        tier: tier_name(result.tier).to_string(),
        tier_meaning: tier_meaning(result.tier).to_string(),
        verdict: verdict_name(result.verdict).to_string(),
        meaning: verdict_meaning(result.verdict).to_string(),
        reasons: result.reasons.iter().map(reason_meaning).collect(),
        stale,
        harmful: result.verdict == LabVerdict::Contradicted && !stale,
        task: result.assignment.as_ref().map(|a| a.task.clone()),
        assignment_source: result
            .assignment
            .as_ref()
            .and_then(|a| a.source.as_ref())
            .map(source_line),
        oracle: result
            .assignment
            .as_ref()
            .map(|a| oracle_line(a.oracle.origin, &a.oracle.locator)),
        repetitions: arms.iter().map(|arm| arm.attempts).sum(),
        wall_ms: arms.iter().map(|arm| arm.wall_ms).sum(),
        arms,
        injection,
        model: result.inputs.model.clone(),
        source_revision: result.inputs.source_revision.clone(),
        limitations: result.limitations.clone(),
        details,
        produced_by: result.provenance.producer.clone(),
    }
}

fn lineage(context: &CardContext<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(prior) = &context.candidate.revises {
        lines.push(format!(
            "This lesson replaces an earlier version ({}). Results about that version are not \
             carried over.",
            short(prior)
        ));
    }
    let names = context
        .descendants
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match context.closed_as {
        Some("revised_into") => lines.push(format!(
            "History: a reviewer rewrote this lesson as {names}. This item is kept unchanged, \
             with its results."
        )),
        Some("split_into") => lines.push(format!(
            "History: a reviewer split this lesson into {names}. This item is kept unchanged, \
             with its results."
        )),
        _ => {}
    }
    lines
}

fn is_history(state: &ReviewState) -> bool {
    matches!(state, ReviewState::Rejected | ReviewState::Merged)
}

/// What the reviewer can do. Lists options; preselects none.
fn next_line(context: &CardContext<'_>, experiments: &[ExperimentCard]) -> String {
    if is_history(context.state) {
        return "Nothing to decide: this item is history.".to_string();
    }
    if !matches!(context.state, ReviewState::Pending | ReviewState::Deferred) {
        return "Already decided. You can still rewrite it; a rewrite becomes a new, untested \
                lesson."
            .to_string();
    }
    if context.candidate.requires_edit_before_promotion {
        return "This is a record or a source excerpt, not a lesson: it cannot become memory as it                 stands. You can reject it, defer it, or rewrite it into a lesson if you see one."
            .to_string();
    }
    let current: Vec<&ExperimentCard> = experiments.iter().filter(|card| !card.stale).collect();
    let has = |verdict: &str| current.iter().any(|card| card.verdict == verdict);
    let all = "accept, rewrite, split, reject or defer";
    if has("Contradicted") {
        "You can reject, rewrite, split, or ask for a rerun. Accepting is still your decision, \
         against this result."
            .to_string()
    } else if has("Invalid") {
        format!(
            "You can {all}. The check disagreed with the lesson's reasoning: consider correcting \
             the cause in a rewrite."
        )
    } else if has("InvalidExperiment") {
        format!(
            "You can {all}, or ask for a rerun once the cause is fixed. No result is not a \
             negative result."
        )
    } else if has("Inconclusive") {
        format!("You can {all}, or ask for a rerun. The result neither supports nor opposes it.")
    } else if has("Supported") {
        format!("You can {all}. The result is narrow; accepting remains your decision.")
    } else if has("Valid") {
        format!("You can {all}. The check agrees with the reasoning; it does not show the lesson helps.")
    } else if experiments.iter().any(|card| card.stale) {
        format!(
            "You can {all}, or ask for a rerun. The results shown describe an earlier version of \
             this lesson."
        )
    } else {
        format!("You can {all}.")
    }
}

/// Build the cards for one review item.
///
/// `detail` reports whether a retained log or receipt can still be opened;
/// pass a closure returning [`DetailState::NotChecked`] when the surface cannot
/// tell.
#[must_use]
pub fn review_cards(
    context: &CardContext<'_>,
    detail: &dyn Fn(&str) -> DetailState,
) -> ReviewCards {
    let candidate = context.candidate;
    let experiments: Vec<ExperimentCard> = candidate
        .experiments
        .iter()
        .map(|result| experiment_card(result, candidate, detail))
        .collect();
    let hindsight = hindsight_card(candidate);
    ReviewCards {
        lineage: lineage(context),
        hold: (candidate.has_harmful_result() && !is_history(context.state)).then(|| {
            "Held for a person: a lab run found this lesson made results worse. No review mode \
             will accept it automatically."
                .to_string()
        }),
        no_hindsight: hindsight.is_none().then(|| {
            "No hindsight was drafted for this lesson. Most lessons have none; it is not required \
             to review one."
                .to_string()
        }),
        hindsight,
        untested: experiments.is_empty().then(|| {
            "Not tested. Most lessons are not, and that says nothing about this one's quality."
                .to_string()
        }),
        next: next_line(context, &experiments),
        experiments,
    }
}

/// A duration a person can read: milliseconds under a second, so a check that
/// took 12 ms is not shown as `0.0 s`. `None` for zero, which is what a record
/// that never measured its time carries — said by saying nothing.
fn duration(wall_ms: u64) -> Option<String> {
    match wall_ms {
        0 => None,
        1..=999 => Some(format!("{wall_ms} ms")),
        _ => Some(format!("{}.{} s", wall_ms / 1000, (wall_ms % 1000) / 100)),
    }
}

/// Append `text` at `indent`, one output line per input line. Control
/// characters are replaced, so text that came from a model, a log or a tool
/// cannot move the cursor, recolour the screen or forge a line of this card.
fn push(out: &mut String, indent: usize, text: &str) {
    for line in text.lines() {
        out.push_str(&" ".repeat(indent));
        out.extend(line.chars().map(|c| {
            if c.is_control() && c != '\t' {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            }
        }));
        out.push('\n');
    }
}

fn cause_text(label: &str, cause: &CauseLine) -> String {
    let cites = if cause.cites.is_empty() {
        "cites no fact".to_string()
    } else {
        format!("cites {}", cause.cites.join(", "))
    };
    format!(
        "{label}: {} (confidence {:.2}; {cites})",
        cause.claim, cause.confidence
    )
}

/// The cards as plain text. Complete on its own: every signal is a word.
#[must_use]
pub fn render_text(cards: &ReviewCards) -> String {
    let mut out = String::new();
    if !cards.lineage.is_empty() {
        out.push_str("Lineage\n");
        for line in &cards.lineage {
            push(&mut out, 2, line);
        }
    }
    if let Some(hold) = &cards.hold {
        push(&mut out, 0, hold);
    }

    out.push_str("Hindsight\n");
    match &cards.hindsight {
        None => push(
            &mut out,
            2,
            cards.no_hindsight.as_deref().unwrap_or_default(),
        ),
        Some(card) => {
            push(&mut out, 2, &format!("intended: {}", card.intended));
            push(&mut out, 2, &format!("observed: {}", card.observed));
            if let Some(outcome) = &card.outcome {
                push(&mut out, 2, &format!("outcome: {outcome}"));
            }
            if let Some(analysis) = &card.analysis {
                push(&mut out, 2, &format!("analysis: {analysis}"));
            }
            push(
                &mut out,
                2,
                &format!(
                    "facts ({}, recorded by the run and not editable):",
                    card.facts.len()
                ),
            );
            for fact in &card.facts {
                let redacted = if fact.redacted { " (redacted)" } else { "" };
                push(
                    &mut out,
                    4,
                    &format!("[{}] {}: {}{redacted}", fact.id, fact.kind, fact.label),
                );
                if let Some(excerpt) = &fact.excerpt {
                    for line in excerpt.lines() {
                        push(&mut out, 8, &format!("> {line}"));
                    }
                }
            }
            match &card.cause {
                Some(cause) => push(&mut out, 2, &cause_text("cause", cause)),
                None => push(&mut out, 2, "cause: none established"),
            }
            for alternative in &card.alternatives {
                push(&mut out, 2, &cause_text("alternative", alternative));
            }
            for signal in &card.missed_signals {
                push(&mut out, 2, &format!("missed signal: {signal}"));
            }
            for (label, value) in [
                ("intervention", &card.intervention),
                ("had it been applied", &card.counterfactual),
                ("applies", &card.applicability),
                ("stops being true when", &card.invalidation),
            ] {
                if let Some(value) = value {
                    push(&mut out, 2, &format!("{label}: {value}"));
                }
            }
            for precondition in &card.preconditions {
                push(&mut out, 2, &format!("needs: {precondition}"));
            }
        }
    }

    out.push_str("Experiments\n");
    if let Some(untested) = &cards.untested {
        push(&mut out, 2, untested);
    }
    for (index, card) in cards.experiments.iter().enumerate() {
        let stale = if card.stale {
            " — STALE: about an earlier version of this lesson; it does not count until rerun"
        } else {
            ""
        };
        push(
            &mut out,
            2,
            &format!("{}. {} {}{stale}", index + 1, card.tier, card.verdict),
        );
        push(&mut out, 5, &format!("{} — {}", card.verdict, card.meaning));
        push(
            &mut out,
            5,
            &format!("{} {}.", card.tier, card.tier_meaning),
        );
        for reason in &card.reasons {
            push(&mut out, 5, &format!("reason: {reason}"));
        }
        if let Some(task) = &card.task {
            push(&mut out, 5, &format!("tested: {task}"));
        }
        if let Some(source) = &card.assignment_source {
            push(&mut out, 5, &format!("from: {source}"));
        }
        if let Some(oracle) = &card.oracle {
            push(&mut out, 5, &format!("decided by: {oracle}"));
        }
        for arm in &card.arms {
            let mut line = format!("arm {}: {} of {} passed", arm.arm, arm.passed, arm.attempts);
            if let Some(took) = duration(arm.wall_ms) {
                line.push_str(&format!(", {took}"));
            }
            if arm.cancelled {
                line.push_str(", cancelled");
            }
            if arm.truncated {
                line.push_str(", output truncated");
            }
            push(&mut out, 5, &line);
            for observation in &arm.observations {
                push(&mut out, 8, observation);
            }
        }
        if let Some(injection) = &card.injection {
            push(&mut out, 5, &format!("injection: {injection}"));
        }
        let mut run = format!("run: revision {}", short(&card.source_revision));
        if let Some(model) = &card.model {
            run.push_str(&format!(", model {model}"));
        }
        if card.repetitions > 0 {
            run.push_str(&format!(", {} attempt(s)", card.repetitions));
            if let Some(took) = duration(card.wall_ms) {
                run.push_str(&format!(", {took} in total"));
            }
        }
        push(&mut out, 5, &run);
        for limitation in &card.limitations {
            push(&mut out, 5, &format!("limit: {limitation}"));
        }
        for detail in &card.details {
            let state = match detail.state {
                DetailState::Available => "available",
                DetailState::NoLongerRetained => {
                    "no longer retained; the result itself still stands"
                }
                DetailState::NotChecked => "not checked from here",
            };
            push(
                &mut out,
                5,
                &format!("details: {} ({}) — {state}", detail.locator, detail.summary),
            );
        }
    }

    out.push_str("Next\n");
    push(&mut out, 2, &cards.next);
    out
}
