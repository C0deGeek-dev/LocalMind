//! Deterministic review-queue deduplication primitives.
//!
//! Two rungs, both pure and offline so the contract is testable without a model:
//! a normalized-canonical hash that collapses trivial variants (case, spacing,
//! trailing punctuation), and a lexical near-duplicate test (token-set overlap)
//! that catches rewordings. An optional semantic rung layers on top in the
//! caller; when it is absent these rungs are the whole story.

use std::collections::BTreeSet;

/// Token-set overlap at or above this is treated as a near-duplicate at enqueue.
/// Higher than the review-time annotation threshold (0.6): enqueue-time merging
/// is silent, so it stays conservative and only folds genuine restatements while
/// still keeping lessons that merely share a topic.
pub const NEAR_DUP_THRESHOLD: f32 = 0.7;

/// Very common words carry no topic signal; dropping them keeps similarity keyed
/// on substantive terms.
const STOP_WORDS: [&str; 24] = [
    "the", "a", "an", "and", "or", "but", "to", "of", "in", "on", "for", "with", "is", "are", "be",
    "this", "that", "it", "as", "at", "by", "from", "use", "using",
];

/// The canonical form of a candidate summary: lowercased, internal whitespace
/// collapsed to single spaces, and surrounding/trailing punctuation trimmed.
/// Trivial variants of the same statement share a canonical form.
#[must_use]
pub fn canonical(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_ascii_lowercase()
}

/// A stable hex hash of the canonical form, used as the exact-duplicate key.
#[must_use]
pub fn canonical_hash(text: &str) -> String {
    let canonical = canonical(text);
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The substantive, lowercased word set of a summary (alphanumeric tokens,
/// stop-words and very short tokens removed).
#[must_use]
pub fn token_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|w| w.len() > 2 && !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// Overlap coefficient `|A∩B| / min(|A|,|B|)` — robust to length differences, so
/// a short lesson contained in a longer one still scores high.
#[must_use]
pub fn similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let smaller = a.len().min(b.len());
    intersection as f32 / smaller as f32
}

/// Whether two summaries are lexical near-duplicates at the enqueue threshold.
#[must_use]
pub fn is_near_duplicate(a: &str, b: &str) -> bool {
    similarity(&token_set(a), &token_set(b)) >= NEAR_DUP_THRESHOLD
}

/// The candidate-level half of the two-level dedup decision. Replaces the
/// prior single undifferentiated "similar, review it" outcome: a *confident*
/// duplicate is redundant enough to skip outright, a clean candidate creates
/// as normal, and only a genuinely ambiguous (borderline) match defers to the
/// second level ([`ExistingItemDecision`]) instead of being guessed at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateDedupDecision {
    /// A confident duplicate of already-accepted memory — not worth creating
    /// as a new memory. Routed to `ReviewAction::IgnoreSimilar`, never
    /// silently dropped (it still closes as a reviewable, audited decision).
    Skip,
    /// No duplicate found (or found but not confident/borderline) — proceeds
    /// through the existing accept/create path unaffected.
    Create,
    /// A borderline match: neither confidently redundant nor clearly novel.
    /// Defers to a human, annotated with an [`ExistingItemDecision`]
    /// suggestion for the *existing* memory it resembles.
    None,
}

/// Pure classifier for the candidate-level decision, from the two signals
/// `review_modes.rs`'s accepted-memory match already computes: whether a
/// duplicate was found at all, and — when one was — whether it was a
/// *confident* match (lexical, or vector cosine ≥ the confident bar) versus
/// a *borderline* one (vector cosine in the route-to-review band). Neither
/// existing threshold changes (D-LM-0020/D-LM-0023 stay exactly as they
/// are) — this only adds a name and a routed outcome to what those
/// thresholds already distinguish.
#[must_use]
pub fn classify_candidate_decision(
    duplicate_found: bool,
    borderline: bool,
) -> CandidateDedupDecision {
    match (duplicate_found, borderline) {
        (false, _) => CandidateDedupDecision::Create,
        (true, false) => CandidateDedupDecision::Skip,
        (true, true) => CandidateDedupDecision::None,
    }
}

/// The existing-item half of the two-level decision: what a human reviewer
/// should consider doing about the *existing* accepted memory a
/// [`CandidateDedupDecision::None`] candidate resembles. A suggestion only —
/// per D-LM-0016, neither outcome ever auto-applies; a reviewer always
/// confirms it explicitly (`review merge` / `review delete-existing`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExistingItemDecision {
    /// The candidate reads as a restatement/consolidation of the existing
    /// memory's wording, not a contradiction of it — consider merging the
    /// two (`review merge`, which promotes the candidate as the existing
    /// memory's replacement, same mechanics as a supersede).
    Merge,
    /// The candidate reads as a correction of the existing memory (it
    /// contradicts it), and the existing memory is not being kept in any
    /// form — consider deleting it (`review delete-existing`), which
    /// rejects the candidate too rather than promoting it as a replacement.
    Delete,
}

/// Pure suggester for the existing-item decision, from the same
/// contradiction signal `review_modes.rs` already computes
/// (`is_contradiction`) for the auto-supersede path — reused, not
/// reimplemented, so the two never diverge.
#[must_use]
pub fn suggest_existing_item_decision(contradicts_existing: bool) -> ExistingItemDecision {
    if contradicts_existing {
        ExistingItemDecision::Delete
    } else {
        ExistingItemDecision::Merge
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_collapses_case_whitespace_and_trailing_punctuation() {
        assert_eq!(
            canonical("  Prefer Guard   clauses!! "),
            "prefer guard clauses"
        );
        // Trivial variants share a canonical hash.
        assert_eq!(
            canonical_hash("Use ripgrep over grep."),
            canonical_hash("use ripgrep over grep")
        );
        assert_eq!(
            canonical_hash("use  ripgrep  over  grep"),
            canonical_hash("Use ripgrep over grep!!!")
        );
        // A genuinely different statement does not collide.
        assert_ne!(
            canonical_hash("use ripgrep over grep"),
            canonical_hash("use fd over find")
        );
    }

    #[test]
    fn near_duplicate_catches_restatements_but_not_distinct_lessons() {
        // A reordering/rewording of the same lesson is a near-duplicate.
        assert!(is_near_duplicate(
            "run the integration suite after every exporter change",
            "after an exporter change, run the integration suite",
        ));
        // A genuinely different lesson is not.
        assert!(!is_near_duplicate(
            "run the integration suite after every exporter change",
            "prefer ripgrep over grep when searching the codebase",
        ));
        // Sharing only a topic word is not enough to merge.
        assert!(!is_near_duplicate(
            "use guard clauses in the parser",
            "use guard clauses in the request handler",
        ));
    }

    #[test]
    fn candidate_decision_maps_confidence_to_the_right_outcome() {
        assert_eq!(
            classify_candidate_decision(false, false),
            CandidateDedupDecision::Create
        );
        assert_eq!(
            classify_candidate_decision(true, false),
            CandidateDedupDecision::Skip
        );
        assert_eq!(
            classify_candidate_decision(true, true),
            CandidateDedupDecision::None
        );
    }

    #[test]
    fn existing_item_decision_follows_the_contradiction_signal() {
        assert_eq!(
            suggest_existing_item_decision(true),
            ExistingItemDecision::Delete
        );
        assert_eq!(
            suggest_existing_item_decision(false),
            ExistingItemDecision::Merge
        );
    }
}
