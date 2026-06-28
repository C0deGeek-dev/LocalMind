//! Programming-language tagging for accepted memory.
//!
//! A lesson that is clearly about one language ("In Python, …") is noise in a
//! task in another language — a Python idiom injected into a Rust task degrades
//! the solution. Rather than retrieve top-N by term match and then drop the
//! off-language ones at read time (which spends the retrieval budget on rows
//! that get thrown away and judges relevance from a truncated snippet), the
//! language is detected once at write time from the full lesson body, stored on
//! `memory_index.language`, and filtered inside the FTS query so retrieval
//! returns N rows that are already language-relevant.
//!
//! The tag is intentionally conservative: a lesson is tagged only when it names
//! **exactly one** language. A lesson that names none is general; one that names
//! several is cross-cutting. Both are stored untagged (`NULL`) so they remain
//! eligible for every task — the filter excludes only single-language lessons
//! that do not match the task's language.
//!
//! Body text alone under-tags: the model often names a language only by idiom
//! (`sort.Strings`, not "Go"), so a clearly language-specific lesson reads as
//! untagged and leaks across languages. [`resolve_memory_language`] closes that
//! gap — a language-bound *category* (a code pattern, an anti-pattern, a
//! debugging recipe) inherits the language of the workspace it was learned in,
//! while cross-cutting categories (tooling, process) stay untagged.

use std::collections::HashMap;
use std::path::Path;

use localmind_core::LessonCategory;

/// The languages recognised for memory tagging and workspace detection: the
/// canonical name, the source extensions that signal it in a workspace, and the
/// lowercase prose keywords that name it in a lesson body. Keywords are matched
/// **whole-word** (see [`contains_word`]), so bare ambiguous names are safe — "go"
/// matches "Go:" but not "going", "java" never matches inside "javascript", and a
/// project name like "llama.cpp" does not read as C++. Distinctive markers
/// disambiguate the English-word languages (Go via `goroutine`/`go build`/`go:`,
/// C++ via `c++`/`g++`, not the bare token).
const LANGS: &[(&str, &[&str], &[&str])] = &[
    (
        "python",
        &["py", "pyi", "pyw"],
        &["python", "python3", "pytest", "virtualenv"],
    ),
    (
        "rust",
        &["rs"],
        &["rust", "cargo", "rustc", "clippy", "rustup"],
    ),
    (
        "javascript",
        &["js", "mjs", "cjs", "jsx"],
        &["javascript", "node.js", "nodejs", "npm"],
    ),
    ("typescript", &["ts", "tsx"], &["typescript", "tsc"]),
    (
        "go",
        &["go"],
        &[
            "golang",
            "goroutine",
            "go.mod",
            "go build",
            "go test",
            "go vet",
            "go run",
            "go mod",
            "go fmt",
            "fmt.errorf",
            "go:",
        ],
    ),
    (
        "cpp",
        &["cpp", "cc", "cxx", "hpp", "hh"],
        &["c++", "g++", "clang++"],
    ),
    ("csharp", &["cs"], &["c#", "csharp", "dotnet", ".net"]),
    ("java", &["java"], &["java"]),
    (
        "powershell",
        &["ps1", "psm1"],
        &["powershell", "pwsh", "cmdlet"],
    ),
    ("bash", &["sh", "bash"], &["bash", "pipefail", "shellcheck"]),
    ("ruby", &["rb"], &["ruby", "rails"]),
];

/// The languages a lesson's text clearly names, by whole-word keyword match.
fn languages_in_text(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    LANGS
        .iter()
        .filter(|(_, _, keywords)| keywords.iter().any(|kw| contains_word(&lower, kw)))
        .map(|(canon, _, _)| *canon)
        .collect()
}

/// Whole-word containment: `needle` must occur in `haystack` bounded by
/// non-alphanumeric characters (or the string ends). So "go" matches "Go:" /
/// "use go build" but not "going" or "cargo", "java" does not match inside
/// "javascript", and "cpp" inside "llama.cpp" is not C++ (that token is not a
/// keyword anyway). The needle's own punctuation (`+ # . :` and spaces) is part
/// of it; only the surrounding characters are boundary-checked. Caller lowercases.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let after_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// The single language a lesson is about, for the stored `language` tag, or
/// `None` when the lesson names no language (general) or more than one
/// (cross-cutting) — both stay untagged so they remain eligible for every task.
#[must_use]
pub fn lesson_language(body: &str) -> Option<&'static str> {
    let langs = languages_in_text(body);
    match langs.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// The canonical language a source-file extension signals, for the host's
/// workspace-language detection. `ext` is matched case-insensitively and may
/// include or omit a leading dot.
#[must_use]
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    LANGS
        .iter()
        .find(|(_, exts, _)| exts.contains(&ext.as_str()))
        .map(|(canon, _, _)| *canon)
}

/// Categories whose lessons are about a specific programming language even when
/// the body never names it — a Go stdlib idiom, a Rust borrow recipe, a language
/// test strategy. These inherit the session's language. Cross-cutting categories
/// (tooling, process, preferences, deployment, security, docs) are not language
/// bound and stay untagged unless the body itself names a language.
fn is_language_bound(category: &LessonCategory) -> bool {
    matches!(
        category,
        LessonCategory::CodePattern
            | LessonCategory::AntiPattern
            | LessonCategory::DebuggingRecipe
            | LessonCategory::TestingStrategy
    )
}

/// The language to tag an accepted memory with at write time. The body wins when
/// it names a single language explicitly (most specific); otherwise a
/// language-bound category inherits `session_language` — the dominant language of
/// the workspace the lesson was learned in — because the model routinely names a
/// language only by idiom (`sort.Strings`, not "Go"). A cross-cutting lesson, or
/// one with no language signal at all, stays untagged (`None`) and eligible for
/// every task.
#[must_use]
pub fn resolve_memory_language(
    category: &LessonCategory,
    body: &str,
    session_language: Option<&str>,
) -> Option<String> {
    if let Some(explicit) = lesson_language(body) {
        return Some(explicit.to_string());
    }
    if is_language_bound(category) {
        return session_language.map(str::to_string);
    }
    None
}

/// The workspace's dominant programming language by source-file extension, or
/// `None` when there is no clear signal (empty/mixed). A bounded, shallow scan
/// that skips dependency and build directories — owned here so the workspace
/// signal and the stored lesson tag share one source of truth.
#[must_use]
pub fn detect_workspace_language(root: &Path) -> Option<&'static str> {
    /// Directories that never carry the project's own source signal.
    const SKIP_DIRS: &[&str] = &[
        "target",
        "node_modules",
        "build",
        "dist",
        "venv",
        "__pycache__",
        "vendor",
    ];
    /// Cap on files inspected, so a large repo does not stall the scan.
    const MAX_FILES: usize = 2_000;

    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        if seen >= MAX_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if seen >= MAX_FILES {
                break;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(entry.path());
            } else {
                seen += 1;
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if let Some(canon) = language_for_extension(ext) {
                    *counts.entry(canon).or_default() += 1;
                }
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(canon, _)| canon)
}

#[cfg(test)]
mod tests {
    use super::{
        detect_workspace_language, language_for_extension, lesson_language, resolve_memory_language,
    };
    use localmind_core::LessonCategory;

    #[test]
    fn single_language_lessons_are_tagged() {
        assert_eq!(
            lesson_language("Always use None for mutable default arguments in Python"),
            Some("python")
        );
        assert_eq!(
            lesson_language("Prefer iterators over index loops in Rust"),
            Some("rust")
        );
        assert_eq!(
            lesson_language("Avoid == in JavaScript; use ==="),
            Some("javascript")
        );
        // "java" is not mistaken for the "javascript" lesson above.
        assert_eq!(
            lesson_language("Close a Java Stream in try-with-resources"),
            Some("java")
        );
    }

    #[test]
    fn recognises_go_csharp_powershell_bash_by_marker() {
        assert_eq!(
            lesson_language("Use a goroutine and `go build` to compile a module"),
            Some("go")
        );
        // The seed convention "Go: ..." is caught by the `go:` marker, not bare "go".
        assert_eq!(
            lesson_language("Go: read a panic message bottom-up"),
            Some("go")
        );
        assert_eq!(
            lesson_language("Never block on .Result in C# async code"),
            Some("csharp")
        );
        assert_eq!(
            lesson_language("In PowerShell a cmdlet error is non-terminating by default"),
            Some("powershell")
        );
        assert_eq!(
            lesson_language("Start a bash script with set -o pipefail"),
            Some("bash")
        );
    }

    #[test]
    fn word_boundaries_avoid_false_positives() {
        // A project name, not the C++ language.
        assert_eq!(
            lesson_language("Serve a model with llama.cpp's server"),
            None
        );
        // "rust" inside "frustrated"/"trust" must not tag Rust.
        assert_eq!(
            lesson_language("Do not get frustrated; trust the process"),
            None
        );
        // bare "go" inside "going" must not tag Go (no marker present).
        assert_eq!(lesson_language("We are going to deploy soon"), None);
    }

    #[test]
    fn general_and_cross_cutting_lessons_are_untagged() {
        // Names no language → general.
        assert_eq!(lesson_language("Run the tests before declaring done"), None);
        // Names two → cross-cutting; stays eligible everywhere.
        assert_eq!(
            lesson_language("Rust's iter::zip mirrors Python's zip"),
            None
        );
    }

    #[test]
    fn extensions_map_to_canonical_languages() {
        assert_eq!(language_for_extension("rs"), Some("rust"));
        assert_eq!(language_for_extension(".py"), Some("python"));
        assert_eq!(language_for_extension("JSX"), Some("javascript"));
        assert_eq!(language_for_extension("hpp"), Some("cpp"));
        assert_eq!(language_for_extension("go"), Some("go"));
        assert_eq!(language_for_extension("cs"), Some("csharp"));
        assert_eq!(language_for_extension("ps1"), Some("powershell"));
        assert_eq!(language_for_extension("sh"), Some("bash"));
        assert_eq!(language_for_extension("txt"), None);
    }

    #[test]
    fn language_bound_category_inherits_session_language() {
        // Body names no language but it is a Go stdlib idiom; AntiPattern is
        // language-bound, so it inherits the session's language (the gap that let
        // `sort.Strings` lessons leak into other languages untagged).
        assert_eq!(
            resolve_memory_language(
                &LessonCategory::AntiPattern,
                "Use sort.Strings on a copy to avoid mutating the input slice.",
                Some("go")
            ),
            Some("go".to_string())
        );
        // An explicit language in the body wins over the session language.
        assert_eq!(
            resolve_memory_language(
                &LessonCategory::AntiPattern,
                "In Rust, prefer iterators over index loops.",
                Some("go")
            ),
            Some("rust".to_string())
        );
        // A cross-cutting category (tooling) stays untagged even in a go session.
        assert_eq!(
            resolve_memory_language(
                &LessonCategory::ToolingNote,
                "Rewrite the whole file when an incremental edit fails on hidden chars.",
                Some("go")
            ),
            None
        );
    }

    #[test]
    fn detects_the_workspace_dominant_language() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.path().join("util.rs"), "fn b() {}").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/gen.py"), "x=1").unwrap();
        assert_eq!(detect_workspace_language(dir.path()), Some("rust"));
    }
}
