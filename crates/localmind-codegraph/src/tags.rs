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
use crate::parse::{end_line_of, file_graph_node, line_of, node_text, parse_evidence};
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
    query: &Query,
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

    parsed.items = extract_items(query, tree.root_node(), file, text)?;
    Ok(parsed)
}

/// Walks the tag-query matches and builds one node per `@definition.*` whose
/// capture maps to a tracked node kind. De-duplicated by stable node id, since a
/// definition can satisfy more than one query pattern.
fn extract_items(
    query: &Query,
    root: Node<'_>,
    file: &AdmittedFile,
    text: &str,
) -> Result<Vec<GraphNode>, CodeGraphError> {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, root, text.as_bytes());

    let mut items: Vec<GraphNode> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(matched) = matches.next() {
        let mut name: Option<&str> = None;
        let mut definition: Option<(Node<'_>, NodeKind)> = None;
        for capture in matched.captures {
            let capture_name = capture_names
                .get(capture.index as usize)
                .copied()
                .unwrap_or("");
            if capture_name == "name" {
                name = Some(node_text(capture.node, text));
            } else if let Some(kind) = definition_kind(capture_name) {
                definition = Some((capture.node, kind));
            }
        }

        let (Some(name), Some((node, kind))) = (name, definition) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let item = definition_node(kind, name, node, file, text)?;
        if seen.insert(item.id.as_str().to_string()) {
            items.push(item);
        }
    }
    Ok(items)
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
