//! Edge resolution over parsed files.
//!
//! Containment is read straight off the parse, so those edges are parsed
//! fact. Call, import, and documentation links require matching names across
//! files; an exact, unambiguous match keeps full confidence, anything fuzzier
//! is tagged heuristic with reduced confidence, and ambiguity is dropped
//! rather than asserted — a wrong edge costs more than a missing one.
//!
//! Resolution runs against a [`ResolutionContext`], which can be built from a
//! full parse (initial ingest) or from the stored graph plus retained doc
//! texts (incremental reindex), so changed files resolve against the active
//! graph without reparsing the unchanged ones.

use crate::parse::ParsedFile;
use crate::CodeGraphError;
use localmind_core::{
    stable_node_id, Confidence, EdgeDerivation, EdgeKind, EvidenceKind, EvidenceRef, GraphEdge,
    GraphNodeId, NodeKind,
};
use localmind_store::GraphStore;
use std::collections::BTreeMap;

const EXACT_CONFIDENCE: f32 = 1.0;
const RESOLVED_IMPORT_CONFIDENCE: f32 = 0.9;
const HEURISTIC_CONFIDENCE: f32 = 0.6;
/// A same-file unique name match for a call: a strong heuristic, but still a
/// heuristic — the call site is parsed, the target binding is inferred.
const CALL_SAME_FILE_CONFIDENCE: f32 = 0.9;

/// A named item the resolver can target, wherever it came from.
#[derive(Clone, Debug)]
struct TargetRef {
    name: String,
    file: String,
    id: GraphNodeId,
}

/// A documentation (non-Rust) file the resolver can link items to.
#[derive(Clone, Debug)]
struct DocRef {
    relative: String,
    id: GraphNodeId,
    text: String,
}

/// The active graph as the resolver sees it: functions for call matching,
/// files for import matching, all items for doc-mention matching, and doc
/// texts for the mention scan.
pub struct ResolutionContext {
    functions: Vec<TargetRef>,
    files: Vec<(String, GraphNodeId)>,
    items: Vec<TargetRef>,
    docs: Vec<DocRef>,
}

impl ResolutionContext {
    /// Context for a full ingest: everything comes from the parse output.
    #[must_use]
    pub fn from_parsed(files: &[ParsedFile]) -> Self {
        let mut context = Self {
            functions: Vec::new(),
            files: Vec::new(),
            items: Vec::new(),
            docs: Vec::new(),
        };
        for file in files {
            context
                .files
                .push((file.file.relative.clone(), file.file_node.id.clone()));
            if !file.file.relative.ends_with(".rs") {
                context.docs.push(DocRef {
                    relative: file.file.relative.clone(),
                    id: file.file_node.id.clone(),
                    text: file.text.clone(),
                });
            }
            for item in &file.items {
                let target = TargetRef {
                    name: item.name.clone(),
                    file: file.file.relative.clone(),
                    id: item.id.clone(),
                };
                if item.kind == NodeKind::Function {
                    context.functions.push(target.clone());
                }
                context.items.push(target);
            }
        }
        context
    }

    /// Context for an incremental reindex: targets come from the active
    /// stored graph; doc texts are supplied by the caller (the store keeps no
    /// file bodies). Files being reindexed in the same run should be layered
    /// on top with [`ResolutionContext::admit_parsed`].
    pub fn from_store(
        store: &GraphStore,
        doc_texts: &[(String, String)],
    ) -> Result<Self, CodeGraphError> {
        let mut context = Self {
            functions: Vec::new(),
            files: Vec::new(),
            items: Vec::new(),
            docs: Vec::new(),
        };
        for file in store.nodes_by_kind(NodeKind::File)? {
            context
                .files
                .push((file.qualified_name.clone(), file.id.clone()));
        }
        for kind in [
            NodeKind::Function,
            NodeKind::Type,
            NodeKind::Module,
            NodeKind::Test,
        ] {
            for node in store.nodes_by_kind(kind)? {
                let Some(location) = node.location.as_ref() else {
                    continue;
                };
                let target = TargetRef {
                    name: node.name.clone(),
                    file: location.path.clone(),
                    id: node.id.clone(),
                };
                if kind == NodeKind::Function {
                    context.functions.push(target.clone());
                }
                context.items.push(target);
            }
        }
        for (relative, text) in doc_texts {
            context.docs.push(DocRef {
                relative: relative.clone(),
                id: stable_node_id(NodeKind::File, relative),
                text: text.clone(),
            });
        }
        Ok(context)
    }

    /// Replaces everything the context knows about a file with the fresh
    /// parse — used when a reindex run resolves files it just reparsed.
    pub fn admit_parsed(&mut self, file: &ParsedFile) {
        let relative = &file.file.relative;
        self.functions.retain(|target| &target.file != relative);
        self.items.retain(|target| &target.file != relative);
        self.files.retain(|(path, _)| path != relative);
        self.docs.retain(|doc| &doc.relative != relative);

        self.files
            .push((relative.clone(), file.file_node.id.clone()));
        if !relative.ends_with(".rs") {
            self.docs.push(DocRef {
                relative: relative.clone(),
                id: file.file_node.id.clone(),
                text: file.text.clone(),
            });
        }
        for item in &file.items {
            let target = TargetRef {
                name: item.name.clone(),
                file: relative.clone(),
                id: item.id.clone(),
            };
            if item.kind == NodeKind::Function {
                self.functions.push(target.clone());
            }
            self.items.push(target);
        }
    }
}

/// Resolves all edges for a full set of parsed files, deduplicated by stable
/// edge id.
pub fn resolve_edges(files: &[ParsedFile]) -> Result<Vec<GraphEdge>, CodeGraphError> {
    let context = ResolutionContext::from_parsed(files);
    let mut edges: BTreeMap<String, GraphEdge> = BTreeMap::new();
    for file in files {
        for edge in resolve_file_edges(file, &context)? {
            edges.insert(edge.id.as_str().to_string(), edge);
        }
    }
    Ok(edges.into_values().collect())
}

/// Resolves the edges contributed by one file against the given context:
/// containment, test coverage from this file's tests, imports from this
/// file's `use` declarations, and doc mentions in both directions.
pub fn resolve_file_edges(
    file: &ParsedFile,
    context: &ResolutionContext,
) -> Result<Vec<GraphEdge>, CodeGraphError> {
    let mut edges = Vec::new();
    containment_edges(file, &mut edges)?;
    test_edges(file, context, &mut edges)?;
    call_edges(file, context, &mut edges)?;
    use_edges(file, context, &mut edges)?;
    documentation_edges(file, context, &mut edges)?;
    Ok(edges)
}

/// `function —calls→ function` for calls made from this file's non-test
/// function bodies. The caller is a definition in this file; the callee is
/// matched by name — uniquely in this file (a strong heuristic) or uniquely
/// across the graph (a weaker one). Ambiguous names and self-calls resolve to
/// nothing. A `Calls` edge is always [`EdgeDerivation::Heuristic`]: the call
/// site is parsed fact, but which definition it binds to is inferred. Calls
/// from test bodies are covered by `tested_by` instead.
fn call_edges(
    file: &ParsedFile,
    context: &ResolutionContext,
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    for caller in file
        .items
        .iter()
        .filter(|item| item.kind == NodeKind::Function)
    {
        for call in file
            .calls
            .iter()
            .filter(|call| call.caller == caller.qualified_name)
        {
            let same_file: Vec<&TargetRef> = context
                .functions
                .iter()
                .filter(|function| {
                    function.file == file.file.relative && function.name == call.callee
                })
                .collect();
            let (target, confidence) = if let [target] = same_file.as_slice() {
                (*target, CALL_SAME_FILE_CONFIDENCE)
            } else {
                let elsewhere: Vec<&TargetRef> = context
                    .functions
                    .iter()
                    .filter(|function| function.name == call.callee)
                    .collect();
                match elsewhere.as_slice() {
                    [target] => (*target, HEURISTIC_CONFIDENCE),
                    _ => continue,
                }
            };

            // Skip self-recursion: a node calling itself adds no navigation.
            if target.id == caller.id {
                continue;
            }

            edges.push(GraphEdge::structural(
                EdgeKind::Calls,
                caller.id.clone(),
                target.id.clone(),
                EdgeDerivation::Heuristic,
                Confidence::new(confidence)?,
                EvidenceRef::new(
                    EvidenceKind::CodeParse,
                    format!("{}:{}", file.file.relative, call.line),
                ),
            ));
        }
    }
    Ok(())
}

/// `file —implemented_by→ item` for every item extracted from the file.
fn containment_edges(file: &ParsedFile, edges: &mut Vec<GraphEdge>) -> Result<(), CodeGraphError> {
    for item in &file.items {
        edges.push(GraphEdge::structural(
            EdgeKind::ImplementedBy,
            file.file_node.id.clone(),
            item.id.clone(),
            EdgeDerivation::Parsed,
            Confidence::new(EXACT_CONFIDENCE)?,
            item.provenance.clone(),
        ));
    }
    Ok(())
}

/// `function —tested_by→ test` for calls made from this file's test bodies.
/// A same-file call is parsed fact; a unique cross-file name match is a
/// heuristic; an ambiguous name resolves to nothing.
fn test_edges(
    file: &ParsedFile,
    context: &ResolutionContext,
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    for test in file.items.iter().filter(|item| item.kind == NodeKind::Test) {
        for call in file
            .calls
            .iter()
            .filter(|call| call.caller == test.qualified_name)
        {
            let same_file: Vec<&TargetRef> = context
                .functions
                .iter()
                .filter(|function| {
                    function.file == file.file.relative && function.name == call.callee
                })
                .collect();
            let (target, derivation, confidence) = if let [target] = same_file.as_slice() {
                (*target, EdgeDerivation::Parsed, EXACT_CONFIDENCE)
            } else {
                let elsewhere: Vec<&TargetRef> = context
                    .functions
                    .iter()
                    .filter(|function| function.name == call.callee)
                    .collect();
                match elsewhere.as_slice() {
                    [target] => (*target, EdgeDerivation::Heuristic, HEURISTIC_CONFIDENCE),
                    _ => continue,
                }
            };

            edges.push(GraphEdge::structural(
                EdgeKind::TestedBy,
                target.id.clone(),
                test.id.clone(),
                derivation,
                Confidence::new(confidence)?,
                EvidenceRef::new(
                    EvidenceKind::CodeParse,
                    format!("{}:{}", file.file.relative, call.line),
                ),
            ));
        }
    }
    Ok(())
}

/// `file —uses→ file` from this file's `use` declarations. A path segment
/// equal to exactly one file stem resolves; a unique prefix relationship is
/// tagged heuristic; anything ambiguous is dropped.
fn use_edges(
    file: &ParsedFile,
    context: &ResolutionContext,
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    for use_path in &file.uses {
        // Split on the path separators used across languages (`::` Rust, `.`
        // Python/Java, `/` JS/Go), dropping relative and empty markers.
        let segments: Vec<&str> = use_path
            .path
            .split([':', '.', '/', '\\'])
            .map(|segment| segment.trim().trim_end_matches(';'))
            .filter(|segment| {
                !segment.is_empty() && !matches!(*segment, "crate" | "super" | "self")
            })
            .collect();

        let mut exact = Vec::new();
        let mut prefix = Vec::new();
        for (relative, id) in &context.files {
            if relative == &file.file.relative {
                continue;
            }
            let stem = module_stem(relative);
            if segments.iter().any(|segment| *segment == stem) {
                exact.push(id);
            } else if segments
                .iter()
                .any(|segment| segment.starts_with(&stem) || stem.starts_with(segment))
            {
                prefix.push(id);
            }
        }

        let (target, derivation, confidence) = match (exact.as_slice(), prefix.as_slice()) {
            ([target], _) => (
                (*target).clone(),
                EdgeDerivation::Parsed,
                RESOLVED_IMPORT_CONFIDENCE,
            ),
            ([], [target]) => (
                (*target).clone(),
                EdgeDerivation::Heuristic,
                HEURISTIC_CONFIDENCE,
            ),
            _ => continue,
        };

        edges.push(GraphEdge::structural(
            EdgeKind::Uses,
            file.file_node.id.clone(),
            target,
            derivation,
            Confidence::new(confidence)?,
            EvidenceRef::new(
                EvidenceKind::CodeParse,
                format!("{}:{}", file.file.relative, use_path.line),
            ),
        ));
    }
    Ok(())
}

/// `item —documented_in→ doc file` when a non-Rust file mentions the item's
/// name in backticks. Doc mentions are always a heuristic. Runs in both
/// directions: this file's items against all docs, and — when this file is
/// itself a doc — every known item against this doc.
fn documentation_edges(
    file: &ParsedFile,
    context: &ResolutionContext,
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    for item in &file.items {
        for doc in &context.docs {
            mention_edge(&item.name, &item.id, doc, edges)?;
        }
    }

    if !file.file.relative.ends_with(".rs") {
        let doc = DocRef {
            relative: file.file.relative.clone(),
            id: file.file_node.id.clone(),
            text: file.text.clone(),
        };
        for item in &context.items {
            mention_edge(&item.name, &item.id, &doc, edges)?;
        }
    }
    Ok(())
}

fn mention_edge(
    name: &str,
    id: &GraphNodeId,
    doc: &DocRef,
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    let mention = format!("`{name}`");
    if doc.text.contains(&mention) {
        edges.push(GraphEdge::structural(
            EdgeKind::DocumentedIn,
            id.clone(),
            doc.id.clone(),
            EdgeDerivation::Heuristic,
            Confidence::new(HEURISTIC_CONFIDENCE)?,
            EvidenceRef::new(
                EvidenceKind::CodeParse,
                format!("{} mentions {mention}", doc.relative),
            ),
        ));
    }
    Ok(())
}

/// The module name a file would be imported as: `src/geometry.rs` →
/// `geometry`, `src/audio/mod.rs` → `audio`.
fn module_stem(relative: &str) -> String {
    let file_name = relative.rsplit('/').next().unwrap_or(relative);
    // Strip the final extension, language-agnostically (`geometry.rs` →
    // `geometry`, `utils.py` → `utils`, `App.tsx` → `App`).
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(file_name);
    if stem == "mod" || stem == "lib" || stem == "main" {
        let mut parts = relative.rsplit('/');
        parts.next();
        parts.next().unwrap_or(stem).to_string()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_edges;
    use crate::{AdmittedFile, RustParser};
    use localmind_core::{EdgeDerivation, EdgeKind, GraphEdge, GraphEndpoint};
    use std::path::PathBuf;

    fn parse(relative: &str, text: &str) -> crate::ParsedFile {
        let file = AdmittedFile {
            absolute: PathBuf::from("unused"),
            relative: relative.to_string(),
        };
        let mut parser = match RustParser::new() {
            Ok(parser) => parser,
            Err(error) => unreachable!("grammar must load: {error}"),
        };
        match parser.parse_file(&file, text) {
            Ok(parsed) => parsed,
            Err(error) => unreachable!("fixture must parse: {error}"),
        }
    }

    fn fixture() -> Vec<crate::ParsedFile> {
        vec![
            parse(
                "src/geometry.rs",
                r#"
pub struct Point { x: f64, y: f64 }

pub fn norm(point: &Point) -> f64 {
    (point.x * point.x + point.y * point.y).sqrt()
}

#[cfg(test)]
mod tests {
    #[test]
    fn norm_is_positive() {
        let value = super::norm(&super::Point { x: 3.0, y: 4.0 });
        assert!(value > 0.0);
    }

    #[test]
    fn renders_far_away() {
        let scale = crate::render::distance_scale();
        assert!(scale >= 0.0);
    }
}
"#,
            ),
            parse(
                "src/render.rs",
                r#"
use crate::geometry::Point;

pub fn distance_scale() -> f64 {
    1.0
}
"#,
            ),
            parse(
                "docs/guide.md",
                "# Guide\n\nThe `norm` helper and the `Point` type are documented here.\n",
            ),
        ]
    }

    fn edges_of_kind(edges: &[GraphEdge], kind: EdgeKind) -> Vec<&GraphEdge> {
        edges.iter().filter(|edge| edge.kind == kind).collect()
    }

    #[test]
    fn resolves_one_edge_of_each_kind() -> Result<(), Box<dyn std::error::Error>> {
        let files = fixture();
        let edges = resolve_edges(&files)?;

        assert!(!edges_of_kind(&edges, EdgeKind::ImplementedBy).is_empty());
        assert!(!edges_of_kind(&edges, EdgeKind::TestedBy).is_empty());
        assert!(!edges_of_kind(&edges, EdgeKind::Uses).is_empty());
        assert!(!edges_of_kind(&edges, EdgeKind::DocumentedIn).is_empty());
        Ok(())
    }

    #[test]
    fn same_file_test_calls_are_parsed_fact() -> Result<(), Box<dyn std::error::Error>> {
        let files = fixture();
        let edges = resolve_edges(&files)?;

        let tested = edges_of_kind(&edges, EdgeKind::TestedBy);
        let same_file = tested
            .iter()
            .find(|edge| edge.derivation == EdgeDerivation::Parsed)
            .ok_or("expected a parsed tested_by edge")?;
        assert!((same_file.confidence.value() - 1.0).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn cross_file_test_calls_are_tagged_heuristic() -> Result<(), Box<dyn std::error::Error>> {
        let files = fixture();
        let edges = resolve_edges(&files)?;

        let tested = edges_of_kind(&edges, EdgeKind::TestedBy);
        let cross_file = tested
            .iter()
            .find(|edge| edge.derivation == EdgeDerivation::Heuristic)
            .ok_or("expected a heuristic tested_by edge")?;
        assert!(cross_file.confidence.value() < 1.0);
        Ok(())
    }

    #[test]
    fn imports_resolve_to_the_matching_file() -> Result<(), Box<dyn std::error::Error>> {
        let files = fixture();
        let edges = resolve_edges(&files)?;

        let uses = edges_of_kind(&edges, EdgeKind::Uses);
        let import = uses.first().ok_or("expected a uses edge")?;
        assert_eq!(
            import.from,
            GraphEndpoint::Node(files[1].file_node.id.clone())
        );
        assert_eq!(
            import.to,
            GraphEndpoint::Node(files[0].file_node.id.clone())
        );
        assert_eq!(import.derivation, EdgeDerivation::Parsed);
        Ok(())
    }

    #[test]
    fn ambiguous_names_resolve_to_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let mut files = fixture();
        // A second `distance_scale` elsewhere makes the cross-file call
        // ambiguous, so it must produce no edge at all.
        files.push(parse(
            "src/other.rs",
            "pub fn distance_scale() -> f64 { 2.0 }\n",
        ));
        let edges = resolve_edges(&files)?;

        let tested = edges_of_kind(&edges, EdgeKind::TestedBy);
        assert!(tested
            .iter()
            .all(|edge| edge.derivation == EdgeDerivation::Parsed));
        Ok(())
    }

    #[test]
    fn doc_mentions_are_heuristic_edges() -> Result<(), Box<dyn std::error::Error>> {
        let files = fixture();
        let edges = resolve_edges(&files)?;

        let documented = edges_of_kind(&edges, EdgeKind::DocumentedIn);
        assert!(documented
            .iter()
            .all(|edge| edge.derivation == EdgeDerivation::Heuristic));
        // Both the `norm` function and the `Point` type are mentioned, and
        // the two scan directions must not double-count them.
        assert_eq!(documented.len(), 2);
        Ok(())
    }
}
