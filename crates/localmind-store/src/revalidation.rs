//! Optional, opt-in source re-validation for accepted memory.
//!
//! The offline freshness pass (`freshness.rs`) flags version-sensitive lessons by
//! a keyword heuristic — it cannot tell whether a lesson is *actually* still true,
//! only that it *might* have gone stale. This module adds the deeper, opt-in
//! check: sample version-sensitive lessons and ask a verdict source whether each
//! still holds, routing a "no longer true" verdict to the existing review gate.
//!
//! It is **default-off and disclosed** (policy D007): the offline heuristic is the
//! default; this is the network-touching pass, only run on an explicit operator
//! action. The sample → check → flag logic is decoupled from any model by the
//! [`VerdictSource`] trait, so it is fully offline-testable with a fixture (the
//! acceptance bar, D008); the live model run is opportunistic. A verdict only ever
//! *flags for review* — it never deletes (D001).

use crate::freshness::is_version_sensitive;

/// A verdict on whether a lesson still holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevalidationVerdict {
    /// The lesson still holds — leave it alone.
    StillCurrent,
    /// The lesson no longer holds — route it to review.
    NoLongerTrue,
    /// The source could not decide (endpoint down, ambiguous answer). Never
    /// flags, so a flaky source cannot manufacture review noise.
    Unknown,
}

/// A source of verdicts on a lesson body. The live implementation asks a model;
/// a fixture implementation makes the pass offline-testable.
pub trait VerdictSource {
    /// Judge whether the lesson `body` still holds.
    fn judge(&self, body: &str) -> RevalidationVerdict;
}

/// Config for one re-validation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevalidationConfig {
    /// The most version-sensitive lessons to sample in one pass (a bound on both
    /// egress and review churn).
    pub sample_size: usize,
}

impl Default for RevalidationConfig {
    fn default() -> Self {
        Self { sample_size: 10 }
    }
}

/// The outcome of one re-validation pass (dry-run or applied).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RevalidationReport {
    /// Version-sensitive lessons that were judged (≤ `sample_size`).
    pub sampled: usize,
    pub still_current: usize,
    pub no_longer_true: usize,
    pub unknown: usize,
    /// The ids routed to review this run (the "no longer true" verdicts). On a
    /// dry run nothing is written; this is what *would* be flagged.
    pub flagged: Vec<String>,
    pub dry_run: bool,
}

/// Whether a lesson body is a re-validation candidate: it reads as
/// version-sensitive (reusing the freshness heuristic, so the two passes target
/// the same lessons).
#[must_use]
pub fn is_revalidation_candidate(body: &str) -> bool {
    is_version_sensitive(body)
}

/// Parse a model's free-text answer into a verdict. Conservative: only an
/// explicit "no longer true" reads as a flag; only an explicit "still current"
/// reads as a pass; anything else is `Unknown` (no flag), so an off-script answer
/// never manufactures a flag.
#[must_use]
pub fn parse_verdict(answer: &str) -> RevalidationVerdict {
    let lower = answer.to_ascii_lowercase();
    if lower.contains("no_longer_true") || lower.contains("no longer true") {
        RevalidationVerdict::NoLongerTrue
    } else if lower.contains("still_current") || lower.contains("still current") {
        RevalidationVerdict::StillCurrent
    } else {
        RevalidationVerdict::Unknown
    }
}

/// The instruction given to a model verdict source. Original prose; asks for one
/// of two exact tokens so the answer parses deterministically.
pub const VERDICT_PROMPT: &str =
    "You check whether a software-engineering lesson is still accurate today. \
The lesson may reference a tool, flag, version, or API that could have changed. \
Reply with exactly one token: STILL_CURRENT if it still holds, or NO_LONGER_TRUE \
if it is now wrong or deprecated. If you are not sure, reply STILL_CURRENT.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_is_conservative() {
        assert_eq!(
            parse_verdict("NO_LONGER_TRUE"),
            RevalidationVerdict::NoLongerTrue
        );
        assert_eq!(
            parse_verdict("I think this is no longer true"),
            RevalidationVerdict::NoLongerTrue
        );
        assert_eq!(
            parse_verdict("STILL_CURRENT"),
            RevalidationVerdict::StillCurrent
        );
        // An off-script answer never flags.
        assert_eq!(
            parse_verdict("maybe? it depends"),
            RevalidationVerdict::Unknown
        );
        assert_eq!(parse_verdict(""), RevalidationVerdict::Unknown);
    }

    #[test]
    fn candidate_matches_the_freshness_heuristic() {
        assert!(is_revalidation_candidate("the --foo flag was deprecated"));
        assert!(!is_revalidation_candidate("prefer guard clauses"));
    }
}
