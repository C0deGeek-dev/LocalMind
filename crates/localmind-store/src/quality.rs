//! Deterministic, offline lesson-quality classification.
//!
//! A model-pinned benchmark showed accepted learning is net-positive but noisy:
//! tooling/process artifacts (a working-directory or build-wrapper note) and
//! over-fit, exercise-specific claims (a single concrete method call coupled to
//! one exercise's identifiers) had auto-accepted and then mis-injected into
//! unrelated tasks. This module adds the missing quality dimension alongside the
//! existing dedup + confidence + conflict gates: one pure, offline classifier
//! that labels a candidate (or an already-stored memory's body) `General`,
//! `OverFit`, or `ToolingNoise`.
//!
//! The classifier never decides storage — it only labels. The write path
//! (`review_modes::apply_project`) withholds auto-accept for a non-`General`
//! candidate (routing it to manual review, never discarding it), and the
//! retroactive freshness pass routes an already-stored bad lesson to review.
//! Nothing here deletes (the standing never-auto-delete invariant). The logic is
//! pure and offline, so the contract is unit-testable without a model or network,
//! and conservative by construction (route, don't drop), with the cost of a wrong
//! label bounded to "a human re-judges it".
//!
//! Markers are matched on whole words/phrases, never bare substrings, so a marker
//! cannot fire inside an unrelated word (the `UNC`-in-`function` class of bug).

use localmind_core::LessonCategory;

/// The quality verdict for one lesson. `General` is the only label that may
/// auto-accept; `OverFit` and `ToolingNoise` route to review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// A transferable principle — the current accept path.
    General,
    /// A claim coupled to one exercise's identifiers/literals/structure, with no
    /// generalizable principle. Routed to review, never auto-accepted.
    OverFit,
    /// A build-tool / shell / working-directory / OS-env mechanic, not a
    /// code/algorithm lesson. Routed to review, never auto-accepted.
    ToolingNoise,
}

impl Quality {
    /// Whether this lesson may take the existing auto-accept path.
    #[must_use]
    pub fn is_general(self) -> bool {
        matches!(self, Quality::General)
    }

    /// A stable token for display/JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::General => "general",
            Quality::OverFit => "over-fit",
            Quality::ToolingNoise => "tooling-noise",
        }
    }

    /// The reviewer-facing note for a routed candidate, or `None` for `General`.
    #[must_use]
    pub fn review_note(self) -> Option<&'static str> {
        match self {
            Quality::General => None,
            Quality::OverFit => Some(
                "low quality (over-fit): exercise-specific, no transferable principle — routed to review, not auto-accepted",
            ),
            Quality::ToolingNoise => Some(
                "low quality (tooling-noise): a build/shell/working-directory mechanic, not a code lesson — routed to review, not auto-accepted",
            ),
        }
    }
}

/// Classify a lesson's quality from its category and text. `summary` is a
/// candidate's one-line lesson; `body` is an accepted memory's stored body — a
/// caller passes whichever it has (the other empty), and both are scanned. Pure
/// and deterministic.
#[must_use]
pub fn classify_quality(category: &LessonCategory, summary: &str, body: &str) -> Quality {
    let text = format!("{summary}\n{body}");
    let lower = text.to_ascii_lowercase();
    let words = word_set(&lower);

    // (1) Keep-list. An error-code / diagnostic recipe is specific *but*
    // generalizable (the taxonomy's keep rule), so it is never demoted, even
    // though it carries concrete tokens an over-fit check might otherwise catch.
    if has_error_code_recipe(&lower, &words) {
        return Quality::General;
    }

    // (2) Tooling-noise. A build/shell/cwd/OS-env mechanic — but only when the
    // category is not one where such a phrase is plausibly the substance of the
    // lesson (a security path-traversal warning, an architecture or code-pattern
    // note). This is the "× category" gate.
    if category_admits_tooling(category) && has_tooling_marker(&lower, &words) {
        return Quality::ToolingNoise;
    }

    // (3) Over-fit. A claim welded to one exercise: a concrete call with
    // arguments inside a code span (an exercise snippet, not a general keyword),
    // a single-fix narration, an unverified hedge about specific code, or text so
    // dominated by code identifiers that no transferable principle remains.
    if has_call_with_args(&text)
        || has_one_off_narration(&lower)
        || (has_hedge(&words) && has_backtick_code(&text))
        || over_fit_density(&text)
    {
        return Quality::OverFit;
    }

    Quality::General
}

/// The lowercased whole-word set of `text` (alphanumeric runs). Used for
/// word-boundary marker matching so a marker never fires inside another word.
fn word_set(lower: &str) -> std::collections::BTreeSet<String> {
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Categories where a build/shell/path phrase is plausibly the lesson's
/// substance (a path-traversal security warning, an architecture or code-pattern
/// note) — there the tooling markers do **not** fire. Every other category
/// admits the tooling-noise label.
fn category_admits_tooling(category: &LessonCategory) -> bool {
    !matches!(
        category,
        LessonCategory::CodePattern
            | LessonCategory::ArchitectureRule
            | LessonCategory::SecurityWarning
    )
}

/// Multi-word tooling phrases (distinctive enough to match as phrases).
const TOOLING_PHRASES: [&str; 11] = [
    "working directory",
    "current directory",
    "path formatting",
    "path handling",
    "npm install",
    "windows shell",
    "windows environment",
    "shell command",
    "gradle build",
    "build tool",
    "package install",
];

/// Single-token tooling markers (matched whole-word).
const TOOLING_WORDS: [&str; 6] = ["gradlew", "powershell", "cwd", "chdir", "enoent", "gradle"];

/// Whether the text reads as a build-tool / shell / OS-env mechanic.
fn has_tooling_marker(lower: &str, words: &std::collections::BTreeSet<String>) -> bool {
    TOOLING_PHRASES.iter().any(|phrase| lower.contains(phrase))
        || TOOLING_WORDS.iter().any(|word| words.contains(*word))
}

/// Hedge words that mark an unverified guess.
const HEDGE_WORDS: [&str; 6] = ["might", "maybe", "perhaps", "possibly", "seems", "appears"];

fn has_hedge(words: &std::collections::BTreeSet<String>) -> bool {
    HEDGE_WORDS.iter().any(|word| words.contains(*word))
}

/// Phrases that narrate one exercise's single fix rather than state a principle.
const NARRATION_PHRASES: [&str; 6] = [
    "initial ", // "Initial <X> implementation …" (paired with the next)
    "resolved test failure",
    "was thrown as",
    "added an extra",
    "test expected",
    "fixed the test where",
];

fn has_one_off_narration(lower: &str) -> bool {
    // "initial " is only narration when it precedes an implementation note, so it
    // does not catch a general "initial state" principle.
    (lower.contains("initial ") && lower.contains("implementation"))
        || NARRATION_PHRASES
            .iter()
            .skip(1)
            .any(|phrase| lower.contains(phrase))
}

/// Whether `text` contains a backtick-delimited code span.
fn has_backtick_code(text: &str) -> bool {
    text.split('`').count() >= 3
}

/// Whether `text` contains a backtick code span holding a call with arguments —
/// `name(arg, …)` or `.method(…)`. A call with a non-empty argument list welded
/// to a code span is an exercise-local snippet (`zip(words, letters)`,
/// `row.trim().split("…")`), unlike a bare keyword/type span (`enum class`,
/// `===`, `await`) which carries no call. Restricting to code spans keeps prose
/// like "(passes sometimes)" from reading as a call.
fn has_call_with_args(text: &str) -> bool {
    // Odd indices of a backtick split are the spans between backticks.
    text.split('`')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .any(span_has_call_with_args)
}

fn span_has_call_with_args((_, span): (usize, &str)) -> bool {
    let bytes = span.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'(' {
            continue;
        }
        // The char before `(` must be an identifier char (a call, not grouping).
        let preceded_by_ident = i
            .checked_sub(1)
            .and_then(|j| bytes.get(j))
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        if !preceded_by_ident {
            continue;
        }
        // A matching `)` with non-empty content after the `(`.
        if let Some(close_rel) = span[i + 1..].find(')') {
            if close_rel > 0 {
                return true;
            }
        }
    }
    false
}

/// A last-resort over-fit signal: text dominated by code identifiers/literals
/// with little prose, so no transferable principle remains. Conservative — it
/// requires a code span *and* a majority of substantive words that look like code
/// — so a general recommendation that merely names a keyword (`enum class`) is
/// untouched.
fn over_fit_density(text: &str) -> bool {
    if !has_backtick_code(text) {
        return false;
    }
    let words: Vec<&str> = text
        .split(|c: char| c.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .collect();
    let substantive: Vec<&str> = words
        .iter()
        .copied()
        .filter(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric()).len() > 2)
        .collect();
    if substantive.len() > 14 || substantive.is_empty() {
        return false;
    }
    let code_ish = substantive.iter().filter(|w| looks_like_code(w)).count();
    code_ish * 2 > substantive.len()
}

/// Whether a raw word looks like a code identifier/literal: a backtick span, a
/// `.`/`::`/`(` call shape, an embedded digit, an underscore-joined identifier,
/// or interior capitalization (camelCase).
fn looks_like_code(word: &str) -> bool {
    if word.contains('`') || word.contains("::") || word.contains('(') || word.contains(')') {
        return true;
    }
    let core = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    core.contains('_')
        || core.bytes().any(|b| b.is_ascii_digit())
        || core
            .char_indices()
            .any(|(i, c)| i > 0 && c.is_ascii_uppercase())
}

/// Whether the text reads as an error-code / diagnostic recipe — specific but
/// generalizable, so kept. Signals: a Rust-style `error[E…]`, a standalone
/// diagnostic code token (`e0107`), or an explicit exit-code/errno mention.
fn has_error_code_recipe(lower: &str, words: &std::collections::BTreeSet<String>) -> bool {
    lower.contains("error[")
        || lower.contains("warning[")
        || lower.contains("exit code")
        || lower.contains("exit status")
        || words.contains("errno")
        || words.iter().any(|w| is_diagnostic_code(w))
}

/// A standalone diagnostic code like `e0107` (a letter then 3–5 digits).
fn is_diagnostic_code(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    let digits = chars.as_str();
    (3..=5).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The real tooling-noise lessons from the v1.1.0 sweep (taxonomy doc). All
    // must classify ToolingNoise under a globalizing category.
    const TOOLING: [&str; 5] = [
        "Initial shell commands failed due to incorrect working directory assumptions in the Windows environment.",
        "Gradle build issues occurred due to path formatting in Windows shell commands.",
        "Use ./gradlew instead of gradlew on Windows if the wrapper is in the current directory.",
        "Ensure npm install is run before executing tests in fresh environments.",
        "Initial shell command execution failed due to Windows path handling; switching to PowerShell resolved it.",
    ];

    // The real over-fit lessons (taxonomy doc) — exercise-specific, no principle.
    const OVERFIT: [&str; 4] = [
        "The parsing logic `row.trim().split(\"\\s+\")` might be stripping necessary context or the grid coordinates.",
        "Avoid `zip(words, letters)` when you need to emit an initial state before any letter arrives.",
        "Initial `verses` implementation added an extra newline at the end.",
        "Resolved test failure where 'O started' error was thrown as 'X went twice'.",
    ];

    // The real general lessons (taxonomy doc) — transferable principles, kept.
    const GENERAL: [&str; 7] = [
        "Analyzing the test file first gives a clear contract for signatures and edge cases.",
        "Ensure function signatures match test expectations before implementing.",
        "Use `enum class` for type safety on classification return values.",
        "Use set comparison for ID validation when input order is unknown.",
        "Debugging by verifying greedy output against brute-force optimal.",
        "error[E0107]: a type alias takes 0 generic arguments — drop the parameters.",
        "Always acquire locks in a consistent global order to avoid deadlocks.",
    ];

    #[test]
    fn the_real_tooling_lessons_classify_tooling_noise() {
        for lesson in TOOLING {
            assert_eq!(
                classify_quality(&LessonCategory::Process, lesson, ""),
                Quality::ToolingNoise,
                "tooling-noise misclassified: {lesson}"
            );
        }
    }

    #[test]
    fn the_real_over_fit_lessons_classify_over_fit() {
        for lesson in OVERFIT {
            assert_eq!(
                classify_quality(&LessonCategory::CodePattern, lesson, ""),
                Quality::OverFit,
                "over-fit misclassified: {lesson}"
            );
        }
    }

    #[test]
    fn the_real_general_lessons_classify_general() {
        for lesson in GENERAL {
            assert_eq!(
                classify_quality(&LessonCategory::CodePattern, lesson, ""),
                Quality::General,
                "general lesson wrongly demoted: {lesson}"
            );
        }
    }

    #[test]
    fn the_named_contractual_examples_are_pinned() {
        // The two examples named in the plan's box 01.4 / 05.1.
        assert_eq!(
            classify_quality(
                &LessonCategory::Process,
                "Initial shell commands failed due to incorrect working directory assumptions.",
                "",
            ),
            Quality::ToolingNoise,
        );
        assert_eq!(
            classify_quality(
                &LessonCategory::CodePattern,
                "The parsing logic `row.trim().split(\"\\s+\")` might be stripping the grid coordinates.",
                "",
            ),
            Quality::OverFit,
        );
    }

    #[test]
    fn the_category_gate_protects_a_substantive_security_or_code_lesson() {
        // A path/shell phrase inside a security or code-pattern lesson is the
        // substance of the lesson, not tooling noise.
        assert_eq!(
            classify_quality(
                &LessonCategory::SecurityWarning,
                "Validate the working directory against a path-traversal allowlist before opening files.",
                "",
            ),
            Quality::General,
        );
        assert_eq!(
            classify_quality(
                &LessonCategory::ArchitectureRule,
                "Resolve the current directory once at startup and pass it explicitly, never read cwd ad hoc.",
                "",
            ),
            Quality::General,
        );
    }

    #[test]
    fn a_general_keyword_span_is_not_a_call_with_args() {
        // `enum class`, `===`, `await` carry no call → not over-fit by code span.
        assert!(!has_call_with_args("Use `enum class` for type safety."));
        assert!(!has_call_with_args("Prefer `===` over `==` in JavaScript."));
        // Parenthetical prose is not a call.
        assert!(!has_call_with_args(
            "A flaky test (passes sometimes) is a real bug."
        ));
        // A call with arguments inside a span is.
        assert!(has_call_with_args("Avoid `zip(words, letters)` here."));
        assert!(has_call_with_args("`row.trim().split(\"x\")` is fragile."));
    }

    #[test]
    fn word_boundary_markers_do_not_fire_inside_unrelated_words() {
        // The substring-vs-word-boundary bug: "cwd"/"gradle" must not match inside
        // a larger word, and "uncertain"/"function" must never read as tooling.
        assert_eq!(
            classify_quality(
                &LessonCategory::Process,
                "Document uncertain assumptions and keep each function small and focused.",
                "",
            ),
            Quality::General,
        );
    }

    #[test]
    fn the_body_argument_is_also_scanned() {
        // A stored memory passes its body (summary empty); the classifier scans it.
        assert_eq!(
            classify_quality(
                &LessonCategory::ToolingNote,
                "",
                "Use ./gradlew on Windows."
            ),
            Quality::ToolingNoise,
        );
    }

    #[test]
    fn review_note_is_present_only_for_non_general() {
        assert!(Quality::General.review_note().is_none());
        assert!(Quality::OverFit.review_note().is_some());
        assert!(Quality::ToolingNoise.review_note().is_some());
    }
}
