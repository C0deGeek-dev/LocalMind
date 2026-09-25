//! Whether a hindsight draft earns a lesson, decided without the model that
//! wrote it.
//!
//! A drafting pass asked both "what happened?" and "is there a durable lesson
//! here?" answers the second optimistically whenever the first is answerable.
//! Measured on six frozen cases, two local models of different capability each
//! produced a correct, correctly cited cause and then a durable rule from a
//! one-off — with no invented evidence and no uncited claim, so no evidence
//! check could see it, and the stronger model did it more often. The model's
//! own `suggested_outcome` is therefore recorded and never consulted here.
//!
//! [`decide`] reads only the validated draft and the facts it cites, in the
//! marker style [`crate::is_version_sensitive`] uses: substring and structure
//! checks, no model, no network. It can only take a lesson away, never add one.

use localmind_core::{EvidenceKind, EvidenceRef, HindsightDraft, HindsightOutcome, Observation};

/// Why an outcome other than `Candidate` was reached, or what failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutcomeReason {
    /// The draft names no cause the facts support.
    NoHypothesis,
    /// A cause is named, and nothing reusable was proposed from it.
    NoLessonProposed,
    /// The proposed change amounts to "try again".
    RetryIntervention,
    /// The proposed change amounts to "check the environment first".
    EnvironmentCheckIntervention,
    /// Every failure the draft cites happened once, and the identical attempt
    /// later succeeded with nothing changed: a transient, not a lesson.
    OneOffResolvedByRetry,
    /// Every failure the draft cites happened once, and a correction the draft
    /// cites names the environment as the cause.
    OneOffEnvironmentalCause,
    /// No analysis ran: the model could not be reached.
    ModelUnavailable { detail: String },
    /// The reply still broke the contract after the one repair pass.
    MalformedAfterRepair { violations: Vec<String> },
    /// The analysis stopped after naming causes; the part that proposes a
    /// change never produced a usable reply.
    Incomplete,
    /// A lesson was proposed over a record with pieces missing where it looks.
    IncompleteRecord { gaps: Vec<String> },
}

impl OutcomeReason {
    /// One line a reviewer can read.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::NoHypothesis => "no cause the recorded facts support".into(),
            Self::NoLessonProposed => "a cause, and nothing reusable follows from it".into(),
            Self::RetryIntervention => "the proposed change amounts to trying again".into(),
            Self::EnvironmentCheckIntervention => {
                "the proposed change amounts to checking the environment first".into()
            }
            Self::OneOffResolvedByRetry => {
                "a one-off: the failure happened once and the identical retry succeeded".into()
            }
            Self::OneOffEnvironmentalCause => {
                "a one-off: the failure happened once and a correction blames the environment"
                    .into()
            }
            Self::ModelUnavailable { detail } => format!("no analysis ran: {detail}"),
            Self::MalformedAfterRepair { violations } => format!(
                "the analysis broke its contract after one repair: {}",
                violations.join("; ")
            ),
            Self::Incomplete => {
                "the analysis named causes but never produced a usable proposal".into()
            }
            Self::IncompleteRecord { gaps } => format!(
                "a lesson was proposed over an incomplete record: {}",
                gaps.join("; ")
            ),
        }
    }
}

/// Leading phrases meaning "do the same thing again". Matched at the start of
/// the proposal only: "Add retry logic with backoff" is a change, "Retry the
/// command" is not.
const RETRY_OPENINGS: [&str; 7] = [
    "retry",
    "try again",
    "re-run",
    "rerun",
    "run it again",
    "wait and",
    "increase the timeout",
];

/// Leading phrases meaning "look before you act".
const CHECK_OPENINGS: [&str; 6] = [
    "check that",
    "check whether",
    "verify that",
    "verify the",
    "make sure",
    "ensure that",
];

/// What the environment is made of, for the two checks above that need it.
const ENVIRONMENT_NOUNS: [&str; 14] = [
    "mounted",
    "network",
    "connection",
    "online",
    "offline",
    "vpn",
    "disk",
    "drive",
    "installed",
    "running",
    "available",
    "credential",
    "permission",
    "environment",
];

/// The outcome a valid draft earns over `facts`, and why.
///
/// Call only with a draft that passed [`HindsightDraft::validate`] against the
/// same facts. The order is fixed: no cause, then no proposal, then the
/// proposal's own wording, then the one-off checks over the cited facts.
#[must_use]
pub fn decide(
    draft: &HindsightDraft,
    facts: &[EvidenceRef],
) -> (HindsightOutcome, Vec<OutcomeReason>) {
    if draft.hypotheses.is_empty() {
        return (
            HindsightOutcome::UnknownCause,
            vec![OutcomeReason::NoHypothesis],
        );
    }
    let Some(lesson) = draft.proposed_lesson.as_deref() else {
        return (
            HindsightOutcome::NoLesson,
            vec![OutcomeReason::NoLessonProposed],
        );
    };

    let mut reasons = Vec::new();
    for proposal in [Some(lesson), draft.intervention.as_deref()]
        .into_iter()
        .flatten()
    {
        let proposal = normalise(proposal);
        if opens_with(&proposal, &RETRY_OPENINGS)
            && !reasons.contains(&OutcomeReason::RetryIntervention)
        {
            reasons.push(OutcomeReason::RetryIntervention);
        }
        if opens_with(&proposal, &CHECK_OPENINGS)
            && mentions_environment(&proposal)
            && !reasons.contains(&OutcomeReason::EnvironmentCheckIntervention)
        {
            reasons.push(OutcomeReason::EnvironmentCheckIntervention);
        }
    }

    let cited: Vec<&EvidenceRef> = draft
        .cited_evidence_ids()
        .into_iter()
        .filter_map(|id| facts.iter().find(|fact| &fact.id == id))
        .collect();
    let cited_failures: Vec<&EvidenceRef> = cited
        .iter()
        .copied()
        .filter(|fact| fact.observation() == Some(Observation::Failure))
        .collect();
    let one_off = !cited_failures.is_empty()
        && cited_failures
            .iter()
            .all(|failure| happened_once(failure, facts));
    if one_off {
        if cited_failures
            .iter()
            .all(|failure| identical_retry_succeeded(failure, facts))
        {
            reasons.push(OutcomeReason::OneOffResolvedByRetry);
        }
        if cited.iter().any(|fact| blames_environment(fact)) {
            reasons.push(OutcomeReason::OneOffEnvironmentalCause);
        }
    }

    if reasons.is_empty() {
        (HindsightOutcome::Candidate, reasons)
    } else {
        (HindsightOutcome::NoLesson, reasons)
    }
}

fn normalise(text: &str) -> String {
    text.trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn opens_with(text: &str, openings: &[&str]) -> bool {
    openings.iter().any(|opening| text.starts_with(opening))
}

fn mentions_environment(text: &str) -> bool {
    ENVIRONMENT_NOUNS.iter().any(|noun| text.contains(noun))
}

/// No other failure in the run carries this failure's signature. A failure
/// with no signature cannot be shown to be a one-off, so it is not treated as
/// one: the check only ever removes a lesson on positive evidence.
fn happened_once(failure: &EvidenceRef, facts: &[EvidenceRef]) -> bool {
    let Some(signature) = failure.signature() else {
        return false;
    };
    facts
        .iter()
        .filter(|fact| {
            fact.observation() == Some(Observation::Failure) && fact.signature() == Some(signature)
        })
        .count()
        == 1
}

/// The next thing that succeeded after the failure, in capture order, was the
/// identical attempt. Anything else succeeding first — an edit, a new file, a
/// different command — may be the fix, so it counts as a change and the
/// failure is not presumed to have gone away on its own.
fn identical_retry_succeeded(failure: &EvidenceRef, facts: &[EvidenceRef]) -> bool {
    let Some(signature) = failure.signature() else {
        return false;
    };
    let Some(position) = facts.iter().position(|fact| fact.id == failure.id) else {
        return false;
    };
    facts[position + 1..]
        .iter()
        .find(|fact| fact.observation() == Some(Observation::Success))
        .is_some_and(|next| next.signature() == Some(signature))
}

fn blames_environment(fact: &EvidenceRef) -> bool {
    let is_correction = fact.kind == EvidenceKind::UserCorrection
        || fact.observation() == Some(Observation::Correction);
    if !is_correction {
        return false;
    }
    let text = format!(
        "{} {}",
        fact.label.to_lowercase(),
        fact.excerpt.as_deref().unwrap_or("").to_lowercase()
    );
    mentions_environment(&text)
}
