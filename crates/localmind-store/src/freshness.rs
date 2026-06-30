//! Deterministic, offline freshness/staleness flagging for accepted memory.
//!
//! The change-aware staleness flag only fires for memory anchored to a project's
//! code; the bulk of the machine-wide global store (language idioms, tooling
//! notes, anti-patterns) is *not* code-anchored, so nothing ever re-checks "is
//! this still true?". This module adds the missing proactive half: a pure,
//! offline pass that selects accepted memory for **review** by four independent,
//! conservative heuristics — age, never-retrieved-after-a-grace, a
//! version-sensitive-tooling keyword set, and low quality (the write-time
//! classifier applied retroactively). It only ever routes a memory to the
//! existing review gate (`flag_for_review`); it never deletes, never re-ranks,
//! and never acts (a human or the automatic-review mode decides). The selection
//! logic here is pure (no I/O), so the whole pass is unit-testable without a
//! model or network; the store drives it over each connection.

use time::OffsetDateTime;

/// Conservative, configurable thresholds for one freshness pass.
///
/// Defaults are deliberately generous so a pass over a warm store flags little —
/// the operator lowers them on purpose. Each heuristic is independent, and every
/// one only *flags for review*, never deletes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshnessThresholds {
    /// Flag a memory older than this many days (the weakest signal: a long-lived
    /// lesson deserves a re-look).
    pub max_age_days: i64,
    /// Flag a never-retrieved memory (`hit_count = 0`) once it is older than this
    /// many days. The grace is essential: every memory starts at zero usage, so
    /// without it a first pass would flag the entire store.
    pub unused_grace_days: i64,
    /// Flag a version-sensitive memory (its body names a tool/version/flag marker)
    /// once it is older than this many days, so a brand-new version note is not
    /// flagged the moment it is written.
    pub version_sensitive_min_age_days: i64,
    /// The most flags a single pass may emit — a flood guard so a pass can never
    /// swamp the review queue. The most-actionable reasons survive the cap.
    pub max_flags: usize,
}

impl Default for FreshnessThresholds {
    fn default() -> Self {
        Self {
            max_age_days: 365,
            unused_grace_days: 90,
            version_sensitive_min_age_days: 180,
            max_flags: 25,
        }
    }
}

/// Why a memory was flagged. When several heuristics match one memory the
/// most-actionable wins (the precedence here, lower rank = higher priority),
/// and the most-actionable reasons are the ones kept when the per-run cap bites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshnessReason {
    /// Its content reads as tooling-noise or over-fit (the write-time quality
    /// classifier) — a bad lesson that predates the write gate. The most
    /// actionable reason: it does not depend on age, so a retroactive pass catches
    /// it immediately.
    LowQuality,
    /// Its body names a version-sensitive tooling marker and it is past the
    /// version-sensitive age floor.
    VersionSensitive,
    /// Never retrieved and past the unused grace.
    Unused,
    /// Simply older than the max age.
    Age,
}

impl FreshnessReason {
    /// Cap-priority rank (lower survives the cap first).
    fn rank(self) -> u8 {
        match self {
            FreshnessReason::LowQuality => 0,
            FreshnessReason::VersionSensitive => 1,
            FreshnessReason::Unused => 2,
            FreshnessReason::Age => 3,
        }
    }

    /// A stable token for display/JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FreshnessReason::LowQuality => "low-quality",
            FreshnessReason::VersionSensitive => "version-sensitive",
            FreshnessReason::Unused => "never-retrieved",
            FreshnessReason::Age => "age",
        }
    }

    /// The reason string recorded on the review flag's audit row.
    #[must_use]
    pub fn audit_reason(self) -> &'static str {
        match self {
            FreshnessReason::LowQuality => {
                "freshness: low-quality lesson (tooling-noise or over-fit) — re-judge or retire"
            }
            FreshnessReason::VersionSensitive => {
                "freshness: version-sensitive lesson — re-check it still holds"
            }
            FreshnessReason::Unused => "freshness: never retrieved — possible dead weight",
            FreshnessReason::Age => "freshness: old lesson — re-check it still holds",
        }
    }
}

/// Which store(s) a freshness pass examines. The default `Both` honours the
/// project's existing scope model (D-LM-0017): a project that allows global sees
/// its project memory and the shared global lessons; `Project`/`Global` narrow it
/// for a focused groom (e.g. only the machine-wide global store).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FreshnessScope {
    #[default]
    Both,
    Project,
    Global,
}

impl FreshnessScope {
    /// Whether the project store is in scope.
    #[must_use]
    pub fn includes_project(self) -> bool {
        matches!(self, FreshnessScope::Both | FreshnessScope::Project)
    }

    /// Whether the global store is in scope.
    #[must_use]
    pub fn includes_global(self) -> bool {
        matches!(self, FreshnessScope::Both | FreshnessScope::Global)
    }

    /// Parse a CLI token (`project`/`global`/`both`), case-insensitively.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "both" => Some(FreshnessScope::Both),
            "project" => Some(FreshnessScope::Project),
            "global" => Some(FreshnessScope::Global),
            _ => None,
        }
    }
}

/// One memory the pass selected for review, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshnessFlag {
    pub memory_id: String,
    pub reason: FreshnessReason,
}

/// The outcome of one pass (dry-run or applied).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FreshnessReport {
    /// Active memories examined across the project and global stores.
    pub scanned: usize,
    /// Candidates that matched a heuristic, *before* the per-run cap.
    pub low_quality: usize,
    pub version_sensitive: usize,
    pub unused: usize,
    pub age: usize,
    /// The flags this run emits — capped to `max_flags`, most-actionable first.
    /// On a dry run nothing is written; this is what *would* be flagged.
    pub flagged: Vec<FreshnessFlag>,
    /// True when more candidates matched than the cap allows (some were withheld
    /// this run); rerun (or raise `max_flags`) to reach the rest.
    pub capped: bool,
    /// Whether this was a dry run (no writes).
    pub dry_run: bool,
}

impl FreshnessReport {
    /// Total candidates that matched a heuristic before the cap.
    #[must_use]
    pub fn total_candidates(&self) -> usize {
        self.low_quality + self.version_sensitive + self.unused + self.age
    }
}

/// Distinctive substrings that signal a lesson is pinned to a specific tool,
/// version, edition, or flag — and so could quietly go stale. Kept focused on
/// purpose: deprecation and version-pinned markers, not broad words like
/// "version"/"cli"/"api" that ride along in evergreen lessons and would flood
/// the queue. Matched case-insensitively as substrings; the semver scan below
/// covers concrete pins like `1.82` or `v0.2.0`.
const VERSION_MARKERS: [&str; 8] = [
    "deprecat",  // deprecated / deprecation
    "msrv",      // minimum supported Rust version
    "nightly",   // nightly toolchain pin
    "cuda",      // GPU/CUDA version coupling
    "no longer", // "no longer works / supported"
    "edition",   // Rust edition pin
    "end of life",
    "eol",
];

/// Whether `body` reads as version-sensitive tooling guidance: it names a
/// version marker or contains a concrete `[v]MAJOR.MINOR` version token.
#[must_use]
pub fn is_version_sensitive(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if VERSION_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return true;
    }
    body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .any(looks_like_version)
}

/// A token like `1.82`, `0.3.37`, or `v0.2.0` — a digit run, a dot, a digit run
/// (an optional leading `v`/`V`). Distinguishes a pinned version from a bare
/// integer or a sentence's end-of-line period.
fn looks_like_version(token: &str) -> bool {
    let token = token.strip_prefix(['v', 'V']).unwrap_or(token);
    let mut groups = token.split('.');
    let (Some(first), Some(second)) = (groups.next(), groups.next()) else {
        return false;
    };
    !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_digit())
        && !second.is_empty()
        && second.bytes().all(|b| b.is_ascii_digit())
}

/// Decide whether one memory flags and why, given its age, usage, and body.
/// `now` is injected so the pass is deterministic in tests. Returns `None` when
/// the memory is fresh, or when its `created_at` cannot be read as a date (it is
/// then treated conservatively as fresh — the pass never over-flags on a parse
/// failure).
#[must_use]
pub(crate) fn classify(
    category: &localmind_core::LessonCategory,
    created_at: &str,
    hit_count: i64,
    body: &str,
    now: OffsetDateTime,
    thresholds: &FreshnessThresholds,
) -> Option<FreshnessReason> {
    // Low quality is independent of age: a tooling-noise or over-fit lesson that
    // predates the write gate is flagged on the first retroactive pass, even if it
    // is brand new or its date cannot be parsed. It routes to review like every
    // other reason — never deleted. Reuses the one shared `classify_quality` fn
    // (the write gate's classifier), so the two callers can never diverge.
    if !crate::quality::classify_quality(category, "", body).is_general() {
        return Some(FreshnessReason::LowQuality);
    }
    let age_days = age_in_days(created_at, now)?;
    if age_days >= thresholds.version_sensitive_min_age_days && is_version_sensitive(body) {
        return Some(FreshnessReason::VersionSensitive);
    }
    if hit_count == 0 && age_days >= thresholds.unused_grace_days {
        return Some(FreshnessReason::Unused);
    }
    if age_days >= thresholds.max_age_days {
        return Some(FreshnessReason::Age);
    }
    None
}

/// Whole days between a stored `created_at` and `now`, or `None` when the date
/// cannot be parsed. Reads only the leading `YYYY-MM-DD`, which both the database
/// (`OffsetDateTime` Display) and the Markdown (RFC 3339) forms share, so it does
/// not depend on the exact timestamp serialization.
fn age_in_days(created_at: &str, now: OffsetDateTime) -> Option<i64> {
    let date = parse_ymd(created_at.get(0..10)?)?;
    Some((now.date() - date).whole_days())
}

/// Parse a `YYYY-MM-DD` prefix into a date without needing the `macros` feature.
fn parse_ymd(ymd: &str) -> Option<time::Date> {
    let mut parts = ymd.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    let month = time::Month::try_from(month).ok()?;
    time::Date::from_calendar_date(year, month, day).ok()
}

/// Order candidates so the most-actionable reasons survive the per-run cap, ties
/// broken by id for determinism.
pub(crate) fn cap_order(a: &FreshnessFlag, b: &FreshnessFlag) -> std::cmp::Ordering {
    a.reason
        .rank()
        .cmp(&b.reason.rank())
        .then_with(|| a.memory_id.cmp(&b.memory_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed "now" built without the macros feature.
    fn now_at(year: i32, month: u8, day: u8) -> OffsetDateTime {
        let date = time::Date::from_calendar_date(year, time::Month::try_from(month).unwrap(), day)
            .unwrap();
        date.midnight().assume_utc()
    }

    fn thresholds() -> FreshnessThresholds {
        FreshnessThresholds::default()
    }

    #[test]
    fn version_marker_and_semver_are_version_sensitive() {
        assert!(is_version_sensitive(
            "the --foo flag was deprecated in the CLI"
        ));
        assert!(is_version_sensitive("requires MSRV 1.82 or newer"));
        assert!(is_version_sensitive("pin turboquant to v0.2.0"));
        assert!(is_version_sensitive("works only on the nightly toolchain"));
        // Evergreen lessons are not version-sensitive.
        assert!(!is_version_sensitive(
            "always redact secrets before persisting a transcript"
        ));
        assert!(!is_version_sensitive(
            "prefer guard clauses over deep nesting"
        ));
        // A bare integer or a sentence period is not a version token.
        assert!(!is_version_sensitive("there are 3 cases to handle."));
    }

    // A category that admits every reason without itself reading as low quality.
    const GENERAL_CATEGORY: localmind_core::LessonCategory =
        localmind_core::LessonCategory::ProjectConvention;

    #[test]
    fn classify_picks_the_most_actionable_reason() {
        let t = thresholds();
        let now = now_at(2026, 6, 28);
        // ~400 days old, version-sensitive, never used -> version-sensitive wins.
        let created = "2025-05-20 10:00:00.0 +00:00:00";
        assert_eq!(
            classify(&GENERAL_CATEGORY, created, 0, "deprecated flag", now, &t),
            Some(FreshnessReason::VersionSensitive)
        );
        // ~400 days old, plain, never used -> unused wins over age.
        assert_eq!(
            classify(
                &GENERAL_CATEGORY,
                created,
                0,
                "an evergreen lesson",
                now,
                &t
            ),
            Some(FreshnessReason::Unused)
        );
        // ~400 days old, plain, but used -> falls through to age.
        assert_eq!(
            classify(
                &GENERAL_CATEGORY,
                created,
                5,
                "an evergreen lesson",
                now,
                &t
            ),
            Some(FreshnessReason::Age)
        );
    }

    #[test]
    fn a_low_quality_lesson_is_flagged_regardless_of_age() {
        let t = thresholds();
        let now = now_at(2026, 6, 28);
        // Brand new (today) and used, but tooling-noise -> flagged immediately.
        let created = "2026-06-28 10:00:00.0 +00:00:00";
        assert_eq!(
            classify(
                &localmind_core::LessonCategory::Process,
                created,
                5,
                "Use ./gradlew instead of gradlew on Windows.",
                now,
                &t
            ),
            Some(FreshnessReason::LowQuality)
        );
        // Even an unparseable date still flags a bad lesson.
        assert_eq!(
            classify(
                &localmind_core::LessonCategory::Process,
                "not-a-date",
                0,
                "Initial shell commands failed due to incorrect working directory assumptions.",
                now,
                &t
            ),
            Some(FreshnessReason::LowQuality)
        );
    }

    #[test]
    fn a_fresh_memory_is_not_flagged() {
        let t = thresholds();
        let now = now_at(2026, 6, 28);
        // Two days old, never used, even version-sensitive: under every floor.
        let created = "2026-06-26 10:00:00.0 +00:00:00";
        assert_eq!(
            classify(
                &GENERAL_CATEGORY,
                created,
                0,
                "deprecated flag in v1.2",
                now,
                &t
            ),
            None
        );
    }

    #[test]
    fn an_unparseable_date_is_treated_as_fresh() {
        let t = thresholds();
        let now = now_at(2026, 6, 28);
        // A general lesson with an unparseable date is fresh (low-quality is the
        // only age-independent reason, and "deprecated" is not low quality).
        assert_eq!(
            classify(&GENERAL_CATEGORY, "not-a-date", 0, "deprecated", now, &t),
            None
        );
    }

    #[test]
    fn the_unused_grace_protects_a_recent_but_unused_memory() {
        let t = thresholds();
        let now = now_at(2026, 6, 28);
        // 30 days old, never used: under the 90-day grace -> not flagged.
        let created = "2026-05-29 10:00:00.0 +00:00:00";
        assert_eq!(
            classify(
                &GENERAL_CATEGORY,
                created,
                0,
                "an evergreen lesson",
                now,
                &t
            ),
            None
        );
    }
}
