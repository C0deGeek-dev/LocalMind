//! Edge resolution over parsed files.
//!
//! Containment is read straight off the parse, so those edges are parsed
//! fact. Call, import, and documentation links require matching names across
//! files; an exact, unambiguous match keeps full confidence, anything fuzzier
//! is tagged heuristic with reduced confidence, and ambiguity is dropped
//! rather than asserted — a wrong edge costs more than a missing one.

use crate::parse::ParsedFile;
use crate::CodeGraphError;
use localmind_core::{
    Confidence, EdgeDerivation, EdgeKind, EvidenceKind, EvidenceRef, GraphEdge, GraphNode, NodeKind,
};

const EXACT_CONFIDENCE: f32 = 1.0;
const RESOLVED_IMPORT_CONFIDENCE: f32 = 0.9;
const HEURISTIC_CONFIDENCE: f32 = 0.6;

pub fn resolve_edges(files: &[ParsedFile]) -> Result<Vec<GraphEdge>, CodeGraphError> {
    let mut edges = Vec::new();
    containment_edges(files, &mut edges)?;
    test_edges(files, &mut edges)?;
    use_edges(files, &mut edges)?;
    documentation_edges(files, &mut edges)?;
    Ok(edges)
}

/// `file —implemented_by→ item` for every item extracted from the file.
fn containment_edges(
    files: &[ParsedFile],
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    for file in files {
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
    }
    Ok(())
}

/// `function —tested_by→ test` for calls made from test bodies. A same-file
/// call is parsed fact; a unique cross-file name match is a heuristic;
/// an ambiguous name resolves to nothing.
fn test_edges(files: &[ParsedFile], edges: &mut Vec<GraphEdge>) -> Result<(), CodeGraphError> {
    let functions: Vec<(&ParsedFile, &GraphNode)> = files
        .iter()
        .flat_map(|file| {
            file.items
                .iter()
                .filter(|item| item.kind == NodeKind::Function)
                .map(move |item| (file, item))
        })
        .collect();

    for file in files {
        for test in file.items.iter().filter(|item| item.kind == NodeKind::Test) {
            for call in file
                .calls
                .iter()
                .filter(|call| call.caller == test.qualified_name)
            {
                let same_file: Vec<&GraphNode> = functions
                    .iter()
                    .filter(|(owner, function)| {
                        owner.file.relative == file.file.relative && function.name == call.callee
                    })
                    .map(|(_, function)| *function)
                    .collect();
                let (target, derivation, confidence) = if let [target] = same_file.as_slice() {
                    (*target, EdgeDerivation::Parsed, EXACT_CONFIDENCE)
                } else {
                    let elsewhere: Vec<&GraphNode> = functions
                        .iter()
                        .filter(|(_, function)| function.name == call.callee)
                        .map(|(_, function)| *function)
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
    }
    Ok(())
}

/// `file —uses→ file` from `use` declarations. A path segment equal to
/// exactly one file stem resolves; a unique prefix relationship is tagged
/// heuristic; anything ambiguous is dropped.
fn use_edges(files: &[ParsedFile], edges: &mut Vec<GraphEdge>) -> Result<(), CodeGraphError> {
    for file in files {
        for use_path in &file.uses {
            let segments: Vec<&str> = use_path
                .path
                .split("::")
                .map(|segment| segment.trim().trim_end_matches(';'))
                .filter(|segment| {
                    !segment.is_empty() && !matches!(*segment, "crate" | "super" | "self")
                })
                .collect();

            let mut exact = Vec::new();
            let mut prefix = Vec::new();
            for candidate in files {
                if candidate.file.relative == file.file.relative {
                    continue;
                }
                let stem = module_stem(&candidate.file.relative);
                if segments.iter().any(|segment| *segment == stem) {
                    exact.push(candidate);
                } else if segments
                    .iter()
                    .any(|segment| segment.starts_with(&stem) || stem.starts_with(segment))
                {
                    prefix.push(candidate);
                }
            }

            let (target, derivation, confidence) = match (exact.as_slice(), prefix.as_slice()) {
                ([target], _) => (*target, EdgeDerivation::Parsed, RESOLVED_IMPORT_CONFIDENCE),
                ([], [target]) => (*target, EdgeDerivation::Heuristic, HEURISTIC_CONFIDENCE),
                _ => continue,
            };

            edges.push(GraphEdge::structural(
                EdgeKind::Uses,
                file.file_node.id.clone(),
                target.file_node.id.clone(),
                derivation,
                Confidence::new(confidence)?,
                EvidenceRef::new(
                    EvidenceKind::CodeParse,
                    format!("{}:{}", file.file.relative, use_path.line),
                ),
            ));
        }
    }
    Ok(())
}

/// `item —documented_in→ doc file` when a non-Rust file mentions the item's
/// name in backticks. Doc mentions are always a heuristic.
fn documentation_edges(
    files: &[ParsedFile],
    edges: &mut Vec<GraphEdge>,
) -> Result<(), CodeGraphError> {
    let docs: Vec<&ParsedFile> = files
        .iter()
        .filter(|file| !file.file.relative.ends_with(".rs"))
        .collect();
    if docs.is_empty() {
        return Ok(());
    }

    for file in files {
        for item in &file.items {
            let mention = format!("`{}`", item.name);
            for doc in &docs {
                if doc.text.contains(&mention) {
                    edges.push(GraphEdge::structural(
                        EdgeKind::DocumentedIn,
                        item.id.clone(),
                        doc.file_node.id.clone(),
                        EdgeDerivation::Heuristic,
                        Confidence::new(HEURISTIC_CONFIDENCE)?,
                        EvidenceRef::new(
                            EvidenceKind::CodeParse,
                            format!("{} mentions {}", doc.file.relative, mention),
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The module name a file would be imported as: `src/geometry.rs` →
/// `geometry`, `src/audio/mod.rs` → `audio`.
fn module_stem(relative: &str) -> String {
    let file_name = relative.rsplit('/').next().unwrap_or(relative);
    let stem = file_name.trim_end_matches(".rs");
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
        // A second `norm` in another file makes the cross-file call ambiguous.
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
        // Both the `norm` function and the `Point` type are mentioned.
        assert!(documented.len() >= 2);
        Ok(())
    }
}
