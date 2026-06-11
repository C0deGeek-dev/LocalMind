//! AST extraction for Rust sources.
//!
//! One parse pass per file produces the file's graph nodes (file, modules,
//! types, functions, tests) plus the raw call sites and import paths the
//! resolver turns into edges. Everything here is read directly from the
//! syntax tree; nothing guesses.

use crate::{AdmittedFile, CodeGraphError};
use localmind_core::{
    content_fingerprint, Confidence, EvidenceKind, EvidenceRef, GraphNode, NodeKind,
    SourceLocation, TypeShape,
};
use tree_sitter::Node;

/// A call observed inside an item body; the resolver matches it to a target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallSite {
    /// Qualified name of the item whose body contains the call.
    pub caller: String,
    /// Last path segment of the callee as written at the call site.
    pub callee: String,
    pub line: u64,
}

/// A `use` declaration observed in the file, kept as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsePath {
    pub path: String,
    pub line: u64,
}

/// Everything extracted from one admitted file.
#[derive(Clone, Debug)]
pub struct ParsedFile {
    pub file: AdmittedFile,
    pub file_node: GraphNode,
    pub items: Vec<GraphNode>,
    pub calls: Vec<CallSite>,
    pub uses: Vec<UsePath>,
    /// Raw text, retained for cross-file resolution (doc mentions); never
    /// persisted onto the graph.
    pub text: String,
}

pub struct RustParser {
    parser: tree_sitter::Parser,
}

impl RustParser {
    pub fn new() -> Result<Self, CodeGraphError> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| CodeGraphError::Grammar(error.to_string()))?;
        Ok(Self { parser })
    }

    /// Parses one admitted file. Non-Rust files produce a file node only, so
    /// docs participate in the graph without being syntax-parsed.
    pub fn parse_file(
        &mut self,
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

        if !file.relative.ends_with(".rs") {
            return Ok(parsed);
        }

        let Some(tree) = self.parser.parse(text, None) else {
            // An unparsable file still contributes its file node.
            return Ok(parsed);
        };

        let mut scope = vec![file.relative.clone()];
        collect_items(tree.root_node(), text, file, &mut scope, &mut parsed)?;
        Ok(parsed)
    }
}

fn file_graph_node(file: &AdmittedFile, text: &str) -> Result<GraphNode, CodeGraphError> {
    let name = file
        .relative
        .rsplit('/')
        .next()
        .unwrap_or(file.relative.as_str())
        .to_string();
    let line_count = text.lines().count() as u64;
    Ok(GraphNode::new(
        NodeKind::File,
        name,
        file.relative.clone(),
        content_fingerprint(text),
        parse_evidence(&file.relative, 1, line_count.max(1)),
        Confidence::new(1.0)?,
    )
    .with_location(SourceLocation {
        path: file.relative.clone(),
        byte_start: 0,
        byte_end: text.len() as u64,
        line_start: 1,
        line_end: line_count.max(1),
    }))
}

fn collect_items(
    node: Node<'_>,
    text: &str,
    file: &AdmittedFile,
    scope: &mut Vec<String>,
    parsed: &mut ParsedFile,
) -> Result<(), CodeGraphError> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                let Some(name) = named_child_text(child, "name", text) else {
                    continue;
                };
                parsed.items.push(item_node(
                    NodeKind::Module,
                    &name,
                    None,
                    child,
                    text,
                    file,
                    scope,
                )?);
                if let Some(body) = child.child_by_field_name("body") {
                    scope.push(name);
                    collect_items(body, text, file, scope, parsed)?;
                    scope.pop();
                }
            }
            "struct_item" | "enum_item" | "trait_item" | "union_item" => {
                let Some(name) = named_child_text(child, "name", text) else {
                    continue;
                };
                let shape = match child.kind() {
                    "struct_item" => TypeShape::Struct,
                    "enum_item" => TypeShape::Enum,
                    "trait_item" => TypeShape::Trait,
                    _ => TypeShape::Union,
                };
                parsed.items.push(item_node(
                    NodeKind::Type,
                    &name,
                    Some(shape),
                    child,
                    text,
                    file,
                    scope,
                )?);
            }
            "function_item" => {
                let Some(name) = named_child_text(child, "name", text) else {
                    continue;
                };
                let kind = if has_test_attribute(child, text) {
                    NodeKind::Test
                } else {
                    NodeKind::Function
                };
                let item = item_node(kind, &name, None, child, text, file, scope)?;
                let caller = item.qualified_name.clone();
                parsed.items.push(item);
                if let Some(body) = child.child_by_field_name("body") {
                    collect_calls(body, text, &caller, &mut parsed.calls);
                }
            }
            "impl_item" => {
                if let Some(target) = named_child_text(child, "type", text) {
                    if let Some(body) = child.child_by_field_name("body") {
                        scope.push(target);
                        collect_items(body, text, file, scope, parsed)?;
                        scope.pop();
                    }
                }
            }
            "use_declaration" => {
                if let Some(argument) = child.child_by_field_name("argument") {
                    parsed.uses.push(UsePath {
                        path: node_text(argument, text).to_string(),
                        line: line_of(argument),
                    });
                }
            }
            _ => {
                // Containers such as `declaration_list` are transparent;
                // walk through anything that still has item children.
                if child.child_count() > 0 && child.kind() != "block" {
                    collect_items(child, text, file, scope, parsed)?;
                }
            }
        }
    }
    Ok(())
}

fn item_node(
    kind: NodeKind,
    name: &str,
    shape: Option<TypeShape>,
    node: Node<'_>,
    text: &str,
    file: &AdmittedFile,
    scope: &[String],
) -> Result<GraphNode, CodeGraphError> {
    let qualified_name = qualified(scope, name);
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
    .with_skeleton(skeleton_of(node, text));
    if let Some(shape) = shape {
        graph_node = graph_node.with_type_shape(shape);
    }
    Ok(graph_node)
}

/// Calls written inside macro invocations (`assert!(foo())`) are token trees
/// in the grammar, not `call_expression` nodes, so they are not extracted;
/// scanning raw tokens would be guessing, and a missing edge costs less than
/// a wrong one.
fn collect_calls(node: Node<'_>, text: &str, caller: &str, calls: &mut Vec<CallSite>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "call_expression" {
            if let Some(function) = child.child_by_field_name("function") {
                let written = node_text(function, text);
                let callee = written
                    .rsplit("::")
                    .next()
                    .unwrap_or(written)
                    .trim()
                    .to_string();
                if !callee.is_empty() {
                    calls.push(CallSite {
                        caller: caller.to_string(),
                        callee,
                        line: line_of(child),
                    });
                }
            }
        }
        collect_calls(child, text, caller, calls);
    }
}

/// The declaration with its body elided: everything up to the body's opening
/// brace, then `{ … }` (or the bare signature when there is no body).
fn skeleton_of(node: Node<'_>, text: &str) -> String {
    let body_start = node
        .child_by_field_name("body")
        .map(|body| body.start_byte());
    let source = node_text(node, text);
    match body_start {
        Some(start) => {
            let head_len = start - node.start_byte();
            let head = source[..head_len].trim_end();
            format!("{head} {{ … }}")
        }
        None => source.trim_end_matches(';').trim_end().to_string(),
    }
}

fn has_test_attribute(node: Node<'_>, text: &str) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(previous) = sibling {
        match previous.kind() {
            "attribute_item" => {
                let attribute = node_text(previous, text);
                if attribute.contains("test") {
                    return true;
                }
                sibling = previous.prev_sibling();
            }
            "line_comment" | "block_comment" => sibling = previous.prev_sibling(),
            _ => return false,
        }
    }
    false
}

fn qualified(scope: &[String], name: &str) -> String {
    let mut parts: Vec<&str> = scope.iter().map(String::as_str).collect();
    parts.push(name);
    parts.join("::")
}

fn named_child_text(node: Node<'_>, field: &str, text: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| node_text(child, text).to_string())
}

fn node_text<'t>(node: Node<'_>, text: &'t str) -> &'t str {
    text.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

fn line_of(node: Node<'_>) -> u64 {
    node.start_position().row as u64 + 1
}

fn end_line_of(node: Node<'_>) -> u64 {
    node.end_position().row as u64 + 1
}

fn parse_evidence(path: &str, line_start: u64, line_end: u64) -> EvidenceRef {
    EvidenceRef::new(
        EvidenceKind::CodeParse,
        format!("{path}:{line_start}-{line_end}"),
    )
}

#[cfg(test)]
mod tests {
    use super::RustParser;
    use crate::AdmittedFile;
    use localmind_core::{NodeKind, TypeShape};
    use std::path::PathBuf;

    const SAMPLE: &str = r#"
pub struct Point {
    x: f64,
    y: f64,
}

impl Point {
    pub fn norm(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}

pub fn draw(point: &Point) -> f64 {
    point.norm() + helper()
}

fn helper() -> f64 {
    0.0
}

mod inner {
    pub enum Direction {
        North,
        South,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn norm_is_positive() {
        let value = super::draw(&super::Point { x: 3.0, y: 4.0 });
        assert!(value > 0.0);
    }
}
"#;

    fn parse_sample() -> super::ParsedFile {
        let file = AdmittedFile {
            absolute: PathBuf::from("unused"),
            relative: "src/geometry.rs".to_string(),
        };
        let mut parser = match RustParser::new() {
            Ok(parser) => parser,
            Err(error) => unreachable!("grammar must load: {error}"),
        };
        match parser.parse_file(&file, SAMPLE) {
            Ok(parsed) => parsed,
            Err(error) => unreachable!("sample must parse: {error}"),
        }
    }

    fn find<'p>(
        parsed: &'p super::ParsedFile,
        kind: NodeKind,
        qualified_name: &str,
    ) -> Option<&'p localmind_core::GraphNode> {
        parsed
            .items
            .iter()
            .find(|item| item.kind == kind && item.qualified_name == qualified_name)
    }

    #[test]
    fn extracts_file_types_functions_modules_and_tests() {
        let parsed = parse_sample();

        assert_eq!(parsed.file_node.kind, NodeKind::File);
        assert_eq!(parsed.file_node.qualified_name, "src/geometry.rs");

        let point = find(&parsed, NodeKind::Type, "src/geometry.rs::Point");
        assert_eq!(
            point.and_then(|node| node.type_shape),
            Some(TypeShape::Struct)
        );
        assert!(find(&parsed, NodeKind::Function, "src/geometry.rs::Point::norm").is_some());
        assert!(find(&parsed, NodeKind::Function, "src/geometry.rs::draw").is_some());
        assert!(find(&parsed, NodeKind::Module, "src/geometry.rs::inner").is_some());
        assert!(find(&parsed, NodeKind::Type, "src/geometry.rs::inner::Direction").is_some());
        assert!(find(
            &parsed,
            NodeKind::Test,
            "src/geometry.rs::tests::norm_is_positive"
        )
        .is_some());
    }

    #[test]
    fn skeletons_elide_bodies_but_keep_signatures() {
        let parsed = parse_sample();
        let norm = find(&parsed, NodeKind::Function, "src/geometry.rs::Point::norm");

        let skeleton = norm.and_then(|node| node.skeleton.as_deref());
        assert_eq!(skeleton, Some("pub fn norm(&self) -> f64 { … }"));
    }

    #[test]
    fn call_sites_record_caller_and_callee() {
        let parsed = parse_sample();

        assert!(parsed
            .calls
            .iter()
            .any(|call| { call.caller == "src/geometry.rs::draw" && call.callee == "helper" }));
        assert!(parsed.calls.iter().any(|call| {
            call.caller == "src/geometry.rs::tests::norm_is_positive" && call.callee == "draw"
        }));
    }

    #[test]
    fn nodes_carry_spans_hashes_and_parse_evidence() {
        let parsed = parse_sample();
        let draw = find(&parsed, NodeKind::Function, "src/geometry.rs::draw");

        let Some(draw) = draw else {
            unreachable!("draw must exist");
        };
        let Some(location) = draw.location.as_ref() else {
            unreachable!("draw must have a location");
        };
        assert_eq!(location.path, "src/geometry.rs");
        assert!(location.line_start < location.line_end);
        assert!(!draw.content_hash.is_empty());
        assert_eq!(
            draw.provenance.kind,
            localmind_core::EvidenceKind::CodeParse
        );
    }

    #[test]
    fn non_rust_files_contribute_a_file_node_only() {
        let file = AdmittedFile {
            absolute: PathBuf::from("unused"),
            relative: "docs/guide.md".to_string(),
        };
        let mut parser = match RustParser::new() {
            Ok(parser) => parser,
            Err(error) => unreachable!("grammar must load: {error}"),
        };
        let parsed = match parser.parse_file(&file, "# Guide\n\nMentions `draw`.\n") {
            Ok(parsed) => parsed,
            Err(error) => unreachable!("markdown must pass through: {error}"),
        };

        assert_eq!(parsed.file_node.kind, NodeKind::File);
        assert!(parsed.items.is_empty());
        assert!(parsed.calls.is_empty());
    }
}
