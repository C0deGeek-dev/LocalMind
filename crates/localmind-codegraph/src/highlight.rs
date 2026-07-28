//! Syntax highlighting: which byte ranges of a snippet the grammar's highlight
//! query claims, and under what capture name.
//!
//! The grammars are already compiled in and each ships a highlight query, so
//! producing the spans costs nothing further. What a *renderer* does with a
//! capture name — which colour, which weight, whether it distinguishes a method
//! from a function at all — is presentation and stays with the renderer. This
//! returns the names verbatim.
//!
//! Nothing here reports an error. A language whose query will not compile, a
//! parser that will not take its own grammar, a source that will not parse:
//! each yields no spans, because a snippet shown without colour is still the
//! snippet, and a caller that has to handle three failure modes will handle none
//! of them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

use crate::language::Language;

/// A byte range of the snippet and the capture name that claimed it.
///
/// Ranges are ordered and never overlap, so a caller can walk them alongside the
/// source without keeping its own interval bookkeeping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    /// The query's capture name, dotted and hierarchical as the grammar wrote it
    /// (`function.method`, `keyword.control`).
    pub capture: &'static str,
}

/// The highlight spans of `source` parsed as `language`.
///
/// Empty where the language cannot be highlighted, which is not distinguished
/// from a snippet with nothing to highlight — neither needs different handling.
#[must_use]
pub fn highlight(source: &str, language: Language) -> Vec<HighlightSpan> {
    if source.is_empty() {
        return Vec::new();
    }
    let Some(query) = compiled_query(language) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language.grammar()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    // Outermost first, so a nested capture overwrites the one containing it.
    // An interpolation inside a string is the case that matters: both match, and
    // the inner one is what the reader is looking at.
    let names = query.capture_names();
    let mut claims: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            let Some(name) = names.get(capture.index as usize) else {
                continue;
            };
            let range = capture.node.byte_range();
            if range.start < range.end && range.end <= source.len() {
                claims.push((range.start, range.end, *name));
            }
        }
    }
    claims.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    // Painted per byte and read back as runs. A snippet is small, and this is
    // what makes "inner wins" fall out of the ordering above instead of needing
    // an interval tree to enforce it.
    let mut painted: Vec<Option<&'static str>> = vec![None; source.len()];
    for (start, end, name) in claims {
        for slot in &mut painted[start..end] {
            *slot = Some(name);
        }
    }

    let mut spans: Vec<HighlightSpan> = Vec::new();
    let mut index = 0;
    while index < painted.len() {
        let Some(capture) = painted[index] else {
            index += 1;
            continue;
        };
        let start = index;
        while index < painted.len() && painted[index] == Some(capture) {
            index += 1;
        }
        spans.push(HighlightSpan {
            start,
            end: index,
            capture,
        });
    }
    spans
}

/// Compiled queries, kept for the life of the process.
///
/// Compiling a highlight query costs far more than running it, and a session
/// meets the same handful of languages repeatedly. A failure is cached as a
/// failure too, so a grammar whose query will not compile is not retried on
/// every call.
fn compiled_query(language: Language) -> Option<&'static Query> {
    static CACHE: OnceLock<Mutex<HashMap<Language, Option<&'static Query>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().ok()?;
    if let Some(entry) = cache.get(&language) {
        return *entry;
    }
    let combined = language.highlights_sources().join("\n");
    let compiled = Query::new(&language.grammar(), &combined)
        .ok()
        .map(|query| &*Box::leak(Box::new(query)));
    cache.insert(language, compiled);
    compiled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(source: &str, language: Language) -> Vec<(&str, &'static str)> {
        highlight(source, language)
            .into_iter()
            .map(|span| (&source[span.start..span.end], span.capture))
            .collect()
    }

    #[test]
    fn rust_keywords_strings_and_comments_are_claimed() {
        let found = captured("// a note\nfn main() { let x = \"hi\"; }", Language::Rust);
        assert!(
            found
                .iter()
                .any(|(text, capture)| *text == "// a note" && capture.starts_with("comment")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|(text, capture)| *text == "fn" && capture.starts_with("keyword")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|(text, capture)| *text == "\"hi\"" && capture.starts_with("string")),
            "{found:?}"
        );
    }

    #[test]
    fn python_is_claimed_too_so_this_is_not_a_rust_only_path() {
        let found = captured("def go():\n    return 42  # done", Language::Python);
        assert!(
            found.iter().any(|(_, c)| c.starts_with("keyword")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|(_, c)| c.starts_with("comment")),
            "{found:?}"
        );
    }

    /// Callers slice the source with these, so they must be ordered, disjoint,
    /// and on character boundaries — or the caller panics or renders text twice.
    #[test]
    fn spans_are_ordered_disjoint_and_on_character_boundaries() {
        let source = "// héllo ✨\nfn main() { let s = \"a\"; let n = 1; }";
        let spans = highlight(source, Language::Rust);
        assert!(!spans.is_empty());
        for pair in spans.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{pair:?}");
        }
        for span in &spans {
            assert!(span.start < span.end);
            assert!(span.end <= source.len());
            assert!(source.is_char_boundary(span.start), "{span:?}");
            assert!(source.is_char_boundary(span.end), "{span:?}");
        }
    }

    /// Half a function is what a streamed code block looks like for most of its
    /// life; tree-sitter recovers rather than refusing.
    #[test]
    fn a_snippet_that_does_not_parse_is_claimed_as_far_as_it_goes() {
        assert!(!highlight("fn main() { let x = ", Language::Rust).is_empty());
    }

    #[test]
    fn empty_source_claims_nothing() {
        assert!(highlight("", Language::Rust).is_empty());
    }

    /// Every language must produce something on a snippet of itself, which is
    /// what proves the query, the grammar, and the parser agree.
    #[test]
    fn every_language_claims_something_in_its_own_comment() {
        for language in Language::ALL {
            let source = match language {
                Language::Elixir | Language::Ruby | Language::PowerShell => "# a comment\n",
                Language::Python => "# a comment\ndef f():\n    pass\n",
                Language::Lua => "-- a comment\n",
                Language::OCaml => "(* a comment *)\n",
                // Outside its opening tag, PHP source is inline HTML: a comment
                // is only a comment once the language has started.
                Language::Php => "<?php\n// a comment\n$x = 1;\n",
                _ => "// a comment\n",
            };
            assert!(
                !highlight(source, *language).is_empty(),
                "{language:?} claimed nothing in its own comment"
            );
        }
    }
}
