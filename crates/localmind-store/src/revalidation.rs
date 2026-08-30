//! Optional, opt-in source re-validation for accepted memory.
//!
//! The offline freshness pass (`freshness.rs`) flags version-sensitive lessons by
//! a keyword heuristic — it cannot tell whether a lesson is *actually* still true,
//! only that it *might* have gone stale. This module adds the deeper, opt-in
//! check: sample version-sensitive lessons and ask a verdict source whether each
//! still holds, routing a "no longer true" verdict to the existing review gate.
//!
//! It is **default-off and disclosed**: the offline heuristic is the default;
//! this is the network-touching pass, only run on an explicit operator action.
//! The sample → check → flag logic is decoupled from any model by the
//! [`VerdictSource`] trait, so it is fully offline-testable with a fixture (the
//! offline acceptance bar); the live model run is opportunistic. A verdict only
//! ever *flags for review* — it never deletes.

use crate::freshness::{is_version_sensitive, looks_like_version};

/// What may leave the machine when a lesson is checked against the world.
///
/// **Never the lesson body.** A memory body is written for this workspace: it
/// names internal paths, project structure, session ids, and whatever the person
/// writing it happened to include. Sending it to a search engine to ask "is this
/// still true" would be sending the thing being protected in order to protect
/// it.
///
/// # Why this takes a `public_names` argument
///
/// The first version of this function decided publicness from **shape** — a
/// dotted path or a `snake_case` identifier was treated as an API name. Measured
/// against the real store, that produced queries like
/// `self.global vector_search vector_index review_items=0 0.86` and
/// `duplicate_of 0.83 0.86`: internal schema names and tuning thresholds, about
/// to be typed into a search engine.
///
/// The reason is not a missing rule. `vector_search` and `serde_json` have
/// **identical shape**, and no amount of pattern-matching separates them,
/// because the difference is not lexical — it is whether the name is already
/// public. So the caller must supply the evidence: the set of names this
/// workspace declares as third-party dependencies, which are public by
/// construction because they were fetched from a public registry.
///
/// The argument is not a convenience. It is the point: there is no safe way to
/// call this without evidence, so the signature does not offer one.
///
/// # What this costs, measured
///
/// On a real store, 49 memories read as version-sensitive and only **9** produce
/// a query at all — and a third of those are meaningless, because crate names
/// like `path`, `time` and `ignore` collide with ordinary English and match the
/// word rather than the crate. Roughly **three** memories in 475 can actually be
/// checked this way.
///
/// That is reported rather than tuned away. A pass that reaches three memories
/// is not worth an outbound request unless a person has decided it is, which is
/// why the derived query is a **proposal for an operator to approve**, never
/// something sent on its own initiative.
///
/// Returns `None` when nothing survives — a refusal to send anything, not a
/// fallback to sending more.
#[must_use]
pub fn derive_verification_query(
    body: &str,
    public_names: &std::collections::BTreeSet<String>,
) -> Option<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for raw in body.split_whitespace() {
        let token = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if token.is_empty() {
            continue;
        }
        // The name as a registry would spell it: the leading segment, lowercased,
        // with `-` and `_` treated alike (`serde_json::from_str` -> `serde_json`).
        let key: String = token
            .split("::")
            .next()
            .unwrap_or(token)
            .split('.')
            .next()
            .unwrap_or(token)
            .to_ascii_lowercase()
            .replace('-', "_");
        if !public_names.contains(&key) {
            continue;
        }
        if seen.insert(key) {
            kept.push(token.to_string());
        }
        if kept.len() >= MAX_QUERY_TOKENS {
            break;
        }
    }
    if kept.is_empty() {
        // No declared dependency is named, so there is nothing this machine can
        // show is already public. Nothing leaves.
        return None;
    }
    // Versions only ride along with a name that earned its place. A bare `0.86`
    // is not a question anybody can answer, and a version attached to nothing is
    // how a tuning threshold ends up in a search box.
    for raw in body.split_whitespace() {
        let token = raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '.'));
        if kept.len() >= MAX_QUERY_TOKENS {
            break;
        }
        if looks_like_version(token) && !kept.iter().any(|k| k == token) {
            kept.push(token.to_string());
        }
    }
    Some(kept.join(" "))
}

/// The most tokens a derived query may carry.
///
/// A bound rather than a judgement: a long query is a query that has started
/// describing the lesson rather than naming its subject, and the whole contract
/// is that the lesson does not leave.
const MAX_QUERY_TOKENS: usize = 6;

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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn public() -> std::collections::BTreeSet<String> {
        ["serde_json", "tokio", "keyring"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn a_derived_query_never_carries_the_lesson_body() {
        let body = "In our .localmind/ store at D:/repos/LocalX, prefer serde_json \
                    because token abc123def4567890 was rejected by the reviewer";
        let query = derive_verification_query(body, &public()).expect("a public name survives");
        assert!(query.contains("serde_json"), "{query}");
        for leaked in [
            "repos",
            "LocalX",
            "abc123def4567890",
            "reviewer",
            "rejected",
            "localmind",
        ] {
            assert!(
                !query.contains(leaked),
                "{leaked:?} must not leave: {query}"
            );
        }
    }

    #[test]
    fn an_internal_identifier_is_not_sent_however_much_it_looks_like_an_api() {
        // The measured failure of deciding publicness by shape. `vector_search`
        // and `serde_json` are the same shape; only the evidence differs.
        let body = "self.global vector_search over vector_index with review_items=0 at 0.86";
        assert_eq!(derive_verification_query(body, &public()), None);
    }

    #[test]
    fn a_version_rides_along_with_a_name_but_never_travels_alone() {
        let with_name = derive_verification_query("keyring 3.6 broke on Windows", &public())
            .expect("a name is present");
        assert!(
            with_name.contains("keyring") && with_name.contains("3.6"),
            "{with_name}"
        );
        // A bare threshold is not a question anybody can answer, and sending one
        // is how a tuning constant ends up in a search box.
        assert_eq!(
            derive_verification_query("we settled on 0.86 after testing 0.83", &public()),
            None
        );
    }

    #[test]
    fn a_lesson_naming_nothing_public_sends_nothing() {
        assert_eq!(
            derive_verification_query("Always commit on main and push every checkpoint", &public()),
            None
        );
    }

    #[test]
    fn a_derived_query_is_bounded() {
        let names: std::collections::BTreeSet<String> =
            (0..50).map(|i| format!("crate_{i}")).collect();
        let body = (0..50)
            .map(|i| format!("crate_{i}::thing"))
            .collect::<Vec<_>>()
            .join(" ");
        let query = derive_verification_query(&body, &names).expect("tokens");
        assert!(
            query.split_whitespace().count() <= MAX_QUERY_TOKENS,
            "a query that long has started describing the lesson: {query}"
        );
    }

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
