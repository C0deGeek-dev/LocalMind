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

/// The languages recognised for memory tagging and workspace detection: the
/// canonical name, the source extensions that signal it in a workspace, and the
/// lowercase prose keywords that name it in a lesson body. Deliberately small
/// and unambiguous — bare "go"/"c" are not matched (too many false positives in
/// prose), and "java" is disambiguated from "javascript" at match time.
const LANGS: &[(&str, &[&str], &[&str])] = &[
    ("python", &["py"], &["python"]),
    ("rust", &["rs"], &["rust"]),
    (
        "javascript",
        &["js", "mjs", "cjs", "jsx"],
        &["javascript", "node.js", "nodejs"],
    ),
    ("typescript", &["ts", "tsx"], &["typescript"]),
    ("go", &["go"], &["golang"]),
    ("cpp", &["cpp", "cc", "cxx", "hpp", "hh"], &["c++", "cpp"]),
    ("java", &["java"], &["java"]),
    ("ruby", &["rb"], &["ruby"]),
];

/// The languages a lesson's text clearly names. "java" counts only when the text
/// is not actually naming "javascript".
fn languages_in_text(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for (canon, _exts, keywords) in LANGS {
        let named = keywords.iter().any(|kw| lower.contains(kw));
        let confused_java = *canon == "java" && lower.contains("javascript");
        if named && !confused_java {
            found.push(*canon);
        }
    }
    found
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

#[cfg(test)]
mod tests {
    use super::{language_for_extension, lesson_language};

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
        assert_eq!(language_for_extension("txt"), None);
    }
}
