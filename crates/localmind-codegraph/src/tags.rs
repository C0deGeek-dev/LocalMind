//! Tag-query symbol extraction, shared by every non-Rust language.
//!
//! A grammar's tag query (`tags.scm`) names the definitions and call sites in a
//! file with the standard tree-sitter captures (`@definition.function`,
//! `@definition.class`, `@reference.call`, with an inner `@name`). This module
//! runs that query over a parsed tree and turns each `@definition.*` match into
//! a graph node, mapping the capture to the existing [`NodeKind`] vocabulary.
//! This module extracts definition/symbol nodes only; calls and imports are the
//! resolver's job.

use crate::language::Language;
use crate::parse::{
    end_line_of, file_graph_node, line_of, node_text, parse_evidence, CallSite, UsePath,
};
use crate::{AdmittedFile, CodeGraphError, ParsedFile};
use localmind_core::{content_fingerprint, Confidence, GraphNode, NodeKind, SourceLocation};
use std::collections::BTreeSet;
use streaming_iterator::StreamingIterator;
use time::OffsetDateTime;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Parses one file with the language's grammar and extracts its definition
/// nodes via the compiled tag query. An unparsable file still contributes its
/// file node, mirroring the Rust path.
pub(crate) fn parse_with_tags(
    parser: &mut Parser,
    language: Language,
    tags: &Query,
    imports: Option<&Query>,
    file: &AdmittedFile,
    text: &str,
) -> Result<ParsedFile, CodeGraphError> {
    let file_node = file_graph_node(file, text)?;
    let mut parsed = ParsedFile {
        file: file.clone(),
        file_node,
        items: Vec::new(),
        calls: Vec::new(),
        uses: Vec::new(),
        text: text.to_string(),
    };

    parser
        .set_language(&language.grammar())
        .map_err(|error| CodeGraphError::Grammar(error.to_string()))?;
    let Some(tree) = parser.parse(text, None) else {
        return Ok(parsed);
    };

    let (items, calls) = extract(tags, tree.root_node(), file, text)?;
    parsed.items = items;
    parsed.calls = calls;
    if let Some(imports) = imports {
        parsed.uses = extract_imports(imports, tree.root_node(), text);
    }
    Ok(parsed)
}

/// Collects import targets (`@path`) the resolver matches to in-repo files,
/// stripping any surrounding string quotes. Relative markers and resolution are
/// the resolver's concern; this only records the module path as written.
fn extract_imports(query: &Query, root: Node<'_>, text: &str) -> Vec<UsePath> {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, text.as_bytes());
    let mut uses = Vec::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            if capture_names.get(capture.index as usize).copied() != Some("path") {
                continue;
            }
            let path = node_text(capture.node, text)
                .trim_matches(['"', '\'', '`'])
                .to_string();
            if !path.is_empty() {
                uses.push(UsePath {
                    path,
                    line: line_of(capture.node),
                });
            }
        }
    }
    uses
}

/// A call site before its caller is known: the callee name as written, and the
/// byte where the call starts (used to find the enclosing definition).
struct RawCall {
    callee: String,
    start_byte: usize,
    line: u64,
}

/// Walks the tag-query matches once, building one node per `@definition.*` whose
/// capture maps to a tracked node kind (de-duplicated by stable node id) and
/// collecting `@reference.call` sites. Each call is then attributed to the
/// innermost enclosing definition so the resolver can turn it into a `Calls`
/// edge; a call with no enclosing definition is dropped rather than guessed.
fn extract(
    query: &Query,
    root: Node<'_>,
    file: &AdmittedFile,
    text: &str,
) -> Result<(Vec<GraphNode>, Vec<CallSite>), CodeGraphError> {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, text.as_bytes());

    let mut items: Vec<GraphNode> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut raw_calls: Vec<RawCall> = Vec::new();
    while let Some(matched) = matches.next() {
        let mut name: Option<&str> = None;
        let mut definition: Option<(Node<'_>, NodeKind)> = None;
        let mut call: Option<Node<'_>> = None;
        for capture in matched.captures {
            let capture_name = capture_names
                .get(capture.index as usize)
                .copied()
                .unwrap_or("");
            if capture_name == "name" {
                name = Some(node_text(capture.node, text));
            } else if capture_name.starts_with("reference.call") {
                call = Some(capture.node);
            } else if let Some(kind) = definition_kind(capture_name) {
                definition = Some((capture.node, kind));
            }
        }

        let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
            continue;
        };
        if let Some((node, kind)) = definition {
            let item = definition_node(kind, name, node, file, text)?;
            if seen.insert(item.id.as_str().to_string()) {
                items.push(item);
            }
        } else if let Some(node) = call {
            raw_calls.push(RawCall {
                callee: name.to_string(),
                start_byte: node.start_byte(),
                line: line_of(node),
            });
        }
    }

    let calls = attribute_calls(&items, raw_calls);
    Ok((items, calls))
}

/// Assigns each raw call to the innermost definition whose byte span contains
/// it (its caller). Calls outside every definition (top-level statements) are
/// dropped — a call with no honest caller is not invented.
fn attribute_calls(items: &[GraphNode], raw_calls: Vec<RawCall>) -> Vec<CallSite> {
    raw_calls
        .into_iter()
        .filter_map(|call| {
            let caller = items
                .iter()
                .filter(|item| {
                    item.location.as_ref().is_some_and(|location| {
                        (location.byte_start as usize) <= call.start_byte
                            && call.start_byte < (location.byte_end as usize)
                    })
                })
                .max_by_key(|item| item.location.as_ref().map(|l| l.byte_start).unwrap_or(0))?;
            Some(CallSite {
                caller: caller.qualified_name.clone(),
                callee: call.callee,
                line: call.line,
            })
        })
        .collect()
}

/// Maps a `@definition.*` capture name to the node kind we track, or `None` for
/// captures we deliberately do not surface as nodes yet (fields, constants,
/// macros, constructors). Honest by omission: an unclassified capture yields no
/// node rather than a mislabeled one.
fn definition_kind(capture_name: &str) -> Option<NodeKind> {
    let suffix = capture_name.strip_prefix("definition.")?;
    match suffix {
        "function" | "method" => Some(NodeKind::Function),
        "module" | "namespace" | "package" => Some(NodeKind::Module),
        "class" | "interface" | "struct" | "enum" | "trait" | "union" | "type" | "object"
        | "record" | "protocol" => Some(NodeKind::Type),
        _ => None,
    }
}

/// Builds a definition node with a flat `path::name` qualified name (the
/// tag-query path carries no enclosing scope, unlike the Rust walker). The node
/// itself is read directly from the tree, so it is full-confidence parsed fact;
/// only cross-file *edges* over these nodes are heuristic, and the resolver
/// stamps those.
fn definition_node(
    kind: NodeKind,
    name: &str,
    node: Node<'_>,
    file: &AdmittedFile,
    text: &str,
) -> Result<GraphNode, CodeGraphError> {
    let qualified_name = format!("{}::{}", file.relative, name);
    let source = node_text(node, text);
    let mut graph_node = GraphNode::new(
        kind,
        name,
        qualified_name,
        content_fingerprint(source),
        parse_evidence(&file.relative, line_of(node), end_line_of(node)),
        Confidence::new(1.0)?,
    )
    .with_location(SourceLocation {
        path: file.relative.clone(),
        byte_start: node.start_byte() as u64,
        byte_end: node.end_byte() as u64,
        line_start: line_of(node),
        line_end: end_line_of(node),
    })
    .with_skeleton(generic_skeleton(source));
    graph_node.created_at = Some(OffsetDateTime::now_utc());
    Ok(graph_node)
}

/// A language-agnostic skeleton: the definition's first non-empty line, trimmed
/// and length-bounded. Good enough to show a signature without a per-language
/// body-elision rule (which the Rust walker does have).
fn generic_skeleton(source: &str) -> String {
    const MAX: usize = 200;
    let line = source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .trim_end_matches('{')
        .trim_end();
    if line.chars().count() > MAX {
        line.chars().take(MAX).collect()
    } else {
        line.to_string()
    }
}
