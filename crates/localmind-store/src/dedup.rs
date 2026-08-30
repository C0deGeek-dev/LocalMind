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
}
