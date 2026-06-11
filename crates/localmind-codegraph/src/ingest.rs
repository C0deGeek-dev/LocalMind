//! End-to-end ingest: admit → parse → resolve → persist.

use crate::boundary::{BoundaryRejection, IngestBoundary};
use crate::parse::{ParsedFile, RustParser};
use crate::resolve::resolve_edges;
use crate::CodeGraphError;
use localmind_core::{
    content_fingerprint, Confidence, EdgeDerivation, EdgeKind, EvidenceKind, EvidenceRef,
    GraphEdge, GraphNode, NodeKind,
};
use localmind_store::GraphStore;
use std::fs;
use std::path::PathBuf;

/// What one ingest run did. Rejections are part of the report, not errors:
/// the boundary refusing a file is the boundary working.
#[derive(Debug)]
pub struct IngestReport {
    pub files_indexed: usize,
    pub nodes_written: usize,
    pub edges_written: usize,
    pub rejected: Vec<BoundaryRejection>,
}

pub struct Ingester {
    parser: RustParser,
}

impl Ingester {
    pub fn new() -> Result<Self, CodeGraphError> {
        Ok(Self {
            parser: RustParser::new()?,
        })
    }

    /// Ingests the host-supplied candidate files into the graph store.
    /// Every admitted file is parsed and persisted together with a repository
    /// node and the edges the resolver derives.
    pub fn ingest(
        &mut self,
        boundary: &IngestBoundary,
        candidates: &[PathBuf],
        store: &GraphStore,
    ) -> Result<IngestReport, CodeGraphError> {
        let mut parsed_files: Vec<ParsedFile> = Vec::new();
        let mut rejected = Vec::new();

        for candidate in candidates {
            let admitted = match boundary.admit(candidate) {
                Ok(admitted) => admitted,
                Err(rejection) => {
                    rejected.push(rejection);
                    continue;
                }
            };
            let text = fs::read_to_string(&admitted.absolute).map_err(|source| {
                CodeGraphError::ReadSource {
                    path: admitted.absolute.clone(),
                    source,
                }
            })?;
            parsed_files.push(self.parser.parse_file(&admitted, &text)?);
        }

        let repository = repository_node(boundary)?;
        let mut edges = resolve_edges(&parsed_files)?;
        for file in &parsed_files {
            edges.push(GraphEdge::structural(
                EdgeKind::BelongsToProject,
                file.file_node.id.clone(),
                repository.id.clone(),
                EdgeDerivation::Parsed,
                Confidence::new(1.0)?,
                EvidenceRef::new(EvidenceKind::CodeParse, file.file.relative.clone()),
            ));
        }

        let mut nodes_written = 0;
        store.upsert_node(&repository)?;
        nodes_written += 1;
        for file in &parsed_files {
            store.upsert_node(&file.file_node)?;
            nodes_written += 1;
            for item in &file.items {
                store.upsert_node(item)?;
                nodes_written += 1;
            }
        }
        for edge in &edges {
            store.upsert_edge(edge)?;
        }

        Ok(IngestReport {
            files_indexed: parsed_files.len(),
            nodes_written,
            edges_written: edges.len(),
            rejected,
        })
    }
}

pub(crate) fn repository_node(boundary: &IngestBoundary) -> Result<GraphNode, CodeGraphError> {
    let name = boundary
        .root()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    Ok(GraphNode::new(
        NodeKind::Repository,
        name.clone(),
        name.clone(),
        content_fingerprint(&name),
        EvidenceRef::new(EvidenceKind::CodeParse, name),
        Confidence::new(1.0)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::Ingester;
    use crate::IngestBoundary;
    use localmind_core::{EdgeDerivation, NodeKind};
    use localmind_store::GraphStore;
    use std::fs;

    #[test]
    fn ingests_a_fixture_crate_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = temp_dir.path();
        fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n")?;
        fs::create_dir_all(root.join("src"))?;
        fs::create_dir_all(root.join("docs"))?;
        fs::create_dir_all(root.join("vendor/private"))?;
        fs::write(
            root.join("src/geometry.rs"),
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
}
"#,
        )?;
        fs::write(
            root.join("src/render.rs"),
            "use crate::geometry::Point;\n\npub fn scale() -> f64 { 1.0 }\n",
        )?;
        fs::write(
            root.join("docs/guide.md"),
            "# Guide\n\nSee `norm` for distance math.\n",
        )?;
        fs::write(root.join("vendor/private/keys.rs"), "pub fn k() {}\n")?;

        let boundary = IngestBoundary::new(root, vec!["vendor/private".to_string()])?;
        let store = GraphStore::open_project(root)?;
        let mut ingester = Ingester::new()?;

        let candidates = vec![
            root.join("src/geometry.rs"),
            root.join("src/render.rs"),
            root.join("docs/guide.md"),
            root.join("vendor/private/keys.rs"),
        ];
        let report = ingester.ingest(&boundary, &candidates, &store)?;

        assert_eq!(report.files_indexed, 3);
        assert_eq!(report.rejected.len(), 1);
        // 1 repository + 3 files + Point + norm + tests module is not
        // extracted as a node kind we track, the test fn is: geometry has
        // Point (type), norm (fn), tests (module), norm_is_positive (test);
        // render has scale (fn).
        assert_eq!(report.nodes_written, 1 + 3 + 5);
        assert!(report.edges_written >= 8);

        // The excluded file must not appear anywhere in the graph.
        for kind in [
            NodeKind::File,
            NodeKind::Function,
            NodeKind::Type,
            NodeKind::Module,
            NodeKind::Test,
        ] {
            for node in store.nodes_by_kind(kind)? {
                assert!(!node.qualified_name.contains("vendor/private"));
            }
        }

        let functions = store.nodes_by_kind(NodeKind::Function)?;
        let norm = functions
            .iter()
            .find(|node| node.qualified_name == "src/geometry.rs::norm")
            .ok_or("norm function missing from store")?;
        assert_eq!(
            norm.skeleton.as_deref(),
            Some("pub fn norm(point: &Point) -> f64 { … }")
        );
        assert!(!norm.content_hash.is_empty());

        let tests = store.tests_of(&norm.id)?;
        assert_eq!(tests.len(), 1);

        // Confidence and derivation survive persistence: the test edge was a
        // same-file call, so it must be parsed fact at full confidence.
        let test_edge_id = localmind_core::stable_edge_id(
            localmind_core::EdgeKind::TestedBy,
            norm.id.as_str(),
            tests[0].id.as_str(),
        );
        let edge = store.edge(&test_edge_id)?.ok_or("tested_by edge missing")?;
        assert_eq!(edge.derivation, EdgeDerivation::Parsed);
        assert!((edge.confidence.value() - 1.0).abs() < f32::EPSILON);
        Ok(())
    }
}
