//! Incremental, bounded reindexing.
//!
//! Change detection is content-based: stored file fingerprints are compared
//! against the current candidate set, so only files whose text actually
//! changed are reparsed, and files that disappeared are superseded. The
//! engine stays VCS-agnostic — a host may narrow the candidate list using its
//! own git signal, but correctness never depends on it.
//!
//! A plan is data, not a running process: `run` consumes up to a batch limit
//! of actions and returns; calling it again resumes where it left off. That
//! makes background indexing bounded, interruptible, and resumable by
//! construction.

use crate::boundary::{BoundaryRejection, IngestBoundary};
use crate::ingest::repository_node;
use crate::parse::{ParsedFile, RustParser};
use crate::resolve::{resolve_file_edges, ResolutionContext};
use crate::CodeGraphError;
use localmind_core::{
    Confidence, EdgeDerivation, EdgeKind, EvidenceKind, EvidenceRef, GraphEdge, GraphEdgeId,
};
use localmind_store::GraphStore;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// The remaining work of one reindex. Resumable: completed actions are
/// removed as they finish, so a fresh `run` call continues the same plan.
#[derive(Debug)]
pub struct ReindexPlan {
    /// Absolute paths whose content changed (or are new) and need a reparse.
    pub index: Vec<PathBuf>,
    /// Repo-relative paths whose sources disappeared and need supersession.
    pub prune: Vec<String>,
    /// Retained text of every admitted non-Rust file, for doc-mention
    /// resolution without a second read.
    pub doc_texts: Vec<(String, String)>,
    /// Candidates the boundary refused.
    pub rejected: Vec<BoundaryRejection>,
    /// Files whose stored fingerprint already matches.
    pub unchanged: usize,
}

impl ReindexPlan {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.index.is_empty() && self.prune.is_empty()
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.index.len() + self.prune.len()
    }
}

/// What one `run` call did.
#[derive(Debug, Default)]
pub struct ReindexBatchReport {
    pub pruned: usize,
    pub reindexed: usize,
    pub edges_written: usize,
    pub edges_revived: usize,
}

pub struct Reindexer {
    parser: RustParser,
}

impl Reindexer {
    pub fn new() -> Result<Self, CodeGraphError> {
        Ok(Self {
            parser: RustParser::new()?,
        })
    }

    /// Compares the candidate set against the stored graph and produces the
    /// minimal work plan. Reads every admitted candidate once to fingerprint
    /// it; reparse cost is only paid later for files that actually changed.
    pub fn plan(
        &self,
        boundary: &IngestBoundary,
        candidates: &[PathBuf],
        store: &GraphStore,
    ) -> Result<ReindexPlan, CodeGraphError> {
        let stored: Vec<(String, String)> = store.active_file_hashes()?;
        let mut plan = ReindexPlan {
            index: Vec::new(),
            prune: Vec::new(),
            doc_texts: Vec::new(),
            rejected: Vec::new(),
            unchanged: 0,
        };

        let mut admitted_relatives = BTreeSet::new();
        for candidate in candidates {
            let admitted = match boundary.admit(candidate) {
                Ok(admitted) => admitted,
                Err(rejection) => {
                    plan.rejected.push(rejection);
                    continue;
                }
            };
            let text = fs::read_to_string(&admitted.absolute).map_err(|source| {
                CodeGraphError::ReadSource {
                    path: admitted.absolute.clone(),
                    source,
                }
            })?;
            admitted_relatives.insert(admitted.relative.clone());
            if !admitted.relative.ends_with(".rs") {
                plan.doc_texts
                    .push((admitted.relative.clone(), text.clone()));
            }

            let fingerprint = localmind_core::content_fingerprint(&text);
            let stored_hash = stored
                .iter()
                .find(|(path, _)| path == &admitted.relative)
                .map(|(_, hash)| hash.as_str());
            if stored_hash == Some(fingerprint.as_str()) {
                plan.unchanged += 1;
            } else {
                plan.index.push(admitted.absolute.clone());
            }
        }

        for (path, _) in &stored {
            if !admitted_relatives.contains(path) {
                plan.prune.push(path.clone());
            }
        }
        Ok(plan)
    }

    /// Executes up to `batch_limit` plan actions (prunes first, then
    /// reparses) and removes them from the plan. Call again to resume.
    pub fn run(
        &mut self,
        boundary: &IngestBoundary,
        store: &GraphStore,
        plan: &mut ReindexPlan,
        batch_limit: usize,
    ) -> Result<ReindexBatchReport, CodeGraphError> {
        let mut report = ReindexBatchReport::default();
        let mut budget = batch_limit;
        let mut superseded_edges: Vec<GraphEdgeId> = Vec::new();

        while budget > 0 && !plan.prune.is_empty() {
            let path = plan.prune.remove(0);
            store.supersede_path(&path)?;
            report.pruned += 1;
            budget -= 1;
        }

        let mut reparsed: Vec<ParsedFile> = Vec::new();
        while budget > 0 && !plan.index.is_empty() {
            let absolute = plan.index.remove(0);
            let admitted = match boundary.admit(&absolute) {
                Ok(admitted) => admitted,
                Err(rejection) => {
                    plan.rejected.push(rejection);
                    continue;
                }
            };
            let text = fs::read_to_string(&admitted.absolute).map_err(|source| {
                CodeGraphError::ReadSource {
                    path: admitted.absolute.clone(),
                    source,
                }
            })?;
            superseded_edges.extend(store.supersede_path(&admitted.relative)?);
            let parsed = self.parser.parse_file(&admitted, &text)?;
            store.upsert_node(&parsed.file_node)?;
            for item in &parsed.items {
                store.upsert_node(item)?;
            }
            reparsed.push(parsed);
            report.reindexed += 1;
            budget -= 1;
        }

        if !reparsed.is_empty() {
            let repository = repository_node(boundary)?;
            store.upsert_node(&repository)?;
            // The store already holds the fresh nodes, so the context built
            // from it sees the post-batch world.
            let context = ResolutionContext::from_store(store, &plan.doc_texts)?;
            for parsed in &reparsed {
                let mut edges = resolve_file_edges(parsed, &context)?;
                edges.push(GraphEdge::structural(
                    EdgeKind::BelongsToProject,
                    parsed.file_node.id.clone(),
                    repository.id.clone(),
                    EdgeDerivation::Parsed,
                    Confidence::new(1.0)?,
                    EvidenceRef::new(EvidenceKind::CodeParse, parsed.file.relative.clone()),
                ));
                for edge in &edges {
                    store.upsert_edge(edge)?;
                }
                report.edges_written += edges.len();
            }
        }

        // Edges from unchanged files (including knowledge anchors) were
        // superseded together with the old rows; bring back the ones whose
        // endpoints exist unchanged in the fresh graph.
        for edge_id in superseded_edges {
            if store.revive_edge_if_anchored(&edge_id)? {
                report.edges_revived += 1;
            }
        }

        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::Reindexer;
    use crate::{IngestBoundary, Ingester};
    use localmind_core::{
        Confidence, EvidenceKind, EvidenceRef, GraphEdge, MemoryEntryId, NodeKind,
    };
    use localmind_store::GraphStore;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn write_tree(root: &Path, geometry_body: &str, with_render: bool) {
        fs::create_dir_all(root.join("src")).unwrap_or(());
        fs::create_dir_all(root.join("docs")).unwrap_or(());
        if fs::write(root.join("src/geometry.rs"), geometry_body).is_err() {
            unreachable!("fixture write must succeed");
        }
        if with_render {
            if fs::write(
                root.join("src/render.rs"),
                "use crate::geometry::Point;\n\npub fn scale() -> f64 { 1.0 }\n",
            )
            .is_err()
            {
                unreachable!("fixture write must succeed");
            }
        } else {
            fs::remove_file(root.join("src/render.rs")).unwrap_or(());
        }
        if fs::write(
            root.join("docs/guide.md"),
            "# Guide\n\nSee `norm` for distance math.\n",
        )
        .is_err()
        {
            unreachable!("fixture write must succeed");
        }
    }

    const GEOMETRY_V1: &str = r#"
pub struct Point { x: f64, y: f64 }

pub fn norm(point: &Point) -> f64 {
    (point.x * point.x + point.y * point.y).sqrt()
}
"#;

    const GEOMETRY_V2: &str = r#"
pub struct Point { x: f64, y: f64 }

pub fn norm(point: &Point) -> f64 {
    (point.x * point.x + point.y * point.y).sqrt()
}

pub fn manhattan(point: &Point) -> f64 {
    point.x.abs() + point.y.abs()
}
"#;

    fn candidates(root: &Path) -> Vec<PathBuf> {
        ["src/geometry.rs", "src/render.rs", "docs/guide.md"]
            .iter()
            .map(|relative| root.join(relative))
            .filter(|path| path.exists())
            .collect()
    }

    fn open_store(root: &Path) -> GraphStore {
        if fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n").is_err() {
            unreachable!("config write must succeed");
        }
        match GraphStore::open_project(root) {
            Ok(store) => store,
            Err(error) => unreachable!("store must open: {error}"),
        }
    }

    #[test]
    fn unchanged_files_are_skipped_and_edits_are_reindexed(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = temp_dir.path();
        write_tree(root, GEOMETRY_V1, true);
        let store = open_store(root);
        let boundary = IngestBoundary::new(root, Vec::new())?;
        Ingester::new()?.ingest(&boundary, &candidates(root), &store)?;

        let reindexer = Reindexer::new()?;
        let plan = reindexer.plan(&boundary, &candidates(root), &store)?;
        assert_eq!(plan.unchanged, 3);
        assert!(plan.is_complete());

        write_tree(root, GEOMETRY_V2, true);
        let plan = reindexer.plan(&boundary, &candidates(root), &store)?;
        assert_eq!(plan.unchanged, 2);
        assert_eq!(plan.index.len(), 1);
        assert!(plan.prune.is_empty());
        Ok(())
    }

    #[test]
    fn deleted_files_are_pruned_via_supersession() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = temp_dir.path();
        write_tree(root, GEOMETRY_V1, true);
        let store = open_store(root);
        let boundary = IngestBoundary::new(root, Vec::new())?;
        Ingester::new()?.ingest(&boundary, &candidates(root), &store)?;

        write_tree(root, GEOMETRY_V1, false);
        let mut reindexer = Reindexer::new()?;
        let mut plan = reindexer.plan(&boundary, &candidates(root), &store)?;
        assert_eq!(plan.prune, vec!["src/render.rs".to_string()]);
        reindexer.run(&boundary, &store, &mut plan, usize::MAX)?;

        let files = store.nodes_by_kind(NodeKind::File)?;
        assert!(files
            .iter()
            .all(|file| file.qualified_name != "src/render.rs"));
        // The row survives with provenance; only traversal excludes it.
        let superseded = localmind_core::stable_node_id(NodeKind::File, "src/render.rs");
        let kept = store.node(&superseded)?.ok_or("pruned row must remain")?;
        assert!(kept.superseded_at.is_some());
        assert_eq!(
            kept.provenance.kind,
            localmind_core::EvidenceKind::CodeParse
        );
        Ok(())
    }

    #[test]
    fn batched_runs_interrupt_and_resume() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = temp_dir.path();
        write_tree(root, GEOMETRY_V1, true);
        let store = open_store(root);
        let boundary = IngestBoundary::new(root, Vec::new())?;
        Ingester::new()?.ingest(&boundary, &candidates(root), &store)?;

        write_tree(root, GEOMETRY_V2, true);
        if fs::write(
            root.join("docs/guide.md"),
            "# Guide\n\nSee `norm` and `manhattan`.\n",
        )
        .is_err()
        {
            unreachable!("fixture write must succeed");
        }

        let mut reindexer = Reindexer::new()?;
        let mut plan = reindexer.plan(&boundary, &candidates(root), &store)?;
        assert_eq!(plan.remaining(), 2);

        let first = reindexer.run(&boundary, &store, &mut plan, 1)?;
        assert_eq!(first.reindexed, 1);
        assert!(!plan.is_complete());

        let second = reindexer.run(&boundary, &store, &mut plan, 1)?;
        assert_eq!(second.reindexed, 1);
        assert!(plan.is_complete());

        let functions = store.nodes_by_kind(NodeKind::Function)?;
        assert!(functions
            .iter()
            .any(|node| node.qualified_name == "src/geometry.rs::manhattan"));
        Ok(())
    }

    #[test]
    fn knowledge_anchored_to_unchanged_symbols_survives_reindex(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = temp_dir.path();
        write_tree(root, GEOMETRY_V1, true);
        let store = open_store(root);
        let boundary = IngestBoundary::new(root, Vec::new())?;
        Ingester::new()?.ingest(&boundary, &candidates(root), &store)?;

        let norm_id = localmind_core::stable_node_id(NodeKind::Function, "src/geometry.rs::norm");
        let anchor = GraphEdge::anchor(
            MemoryEntryId::new("memory-1"),
            norm_id.clone(),
            Confidence::new(0.8)?,
            EvidenceRef::new(EvidenceKind::ManualNote, "lesson about norm"),
        );
        store.upsert_edge(&anchor)?;

        // Edit the file around the unchanged `norm` symbol and reindex.
        write_tree(root, GEOMETRY_V2, true);
        let mut reindexer = Reindexer::new()?;
        let mut plan = reindexer.plan(&boundary, &candidates(root), &store)?;
        let report = reindexer.run(&boundary, &store, &mut plan, usize::MAX)?;

        assert!(report.edges_revived >= 1);
        let revived = store.edge(&anchor.id)?.ok_or("anchor edge missing")?;
        assert!(revived.superseded_at.is_none());
        Ok(())
    }

    #[test]
    fn incremental_reindex_matches_a_fresh_ingest() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = temp_dir.path();
        write_tree(root, GEOMETRY_V1, true);
        let store = open_store(root);
        let boundary = IngestBoundary::new(root, Vec::new())?;
        Ingester::new()?.ingest(&boundary, &candidates(root), &store)?;

        // Edit one file, update the doc, delete another file — then reindex
        // incrementally in deliberately tiny batches.
        write_tree(root, GEOMETRY_V2, false);
        if fs::write(
            root.join("docs/guide.md"),
            "# Guide\n\nSee `norm` and `manhattan`.\n",
        )
        .is_err()
        {
            unreachable!("fixture write must succeed");
        }
        let mut reindexer = Reindexer::new()?;
        let mut plan = reindexer.plan(&boundary, &candidates(root), &store)?;
        while !plan.is_complete() {
            reindexer.run(&boundary, &store, &mut plan, 1)?;
        }

        // A from-scratch ingest of the final tree into a fresh store is the
        // reference graph.
        let reference_dir = tempfile::tempdir()?;
        let reference_root = reference_dir.path();
        write_tree(reference_root, GEOMETRY_V2, false);
        if fs::write(
            reference_root.join("docs/guide.md"),
            "# Guide\n\nSee `norm` and `manhattan`.\n",
        )
        .is_err()
        {
            unreachable!("fixture write must succeed");
        }
        let reference_store = open_store(reference_root);
        let reference_boundary = IngestBoundary::new(reference_root, Vec::new())?;
        Ingester::new()?.ingest(
            &reference_boundary,
            &candidates(reference_root),
            &reference_store,
        )?;

        // Repository nodes differ by temp-dir name; compare everything else.
        let incremental_nodes: Vec<(String, String)> = store
            .active_node_summaries()?
            .into_iter()
            .filter(|(id, _)| !is_repository(&store, id))
            .collect();
        let reference_nodes: Vec<(String, String)> = reference_store
            .active_node_summaries()?
            .into_iter()
            .filter(|(id, _)| !is_repository(&reference_store, id))
            .collect();
        assert_eq!(incremental_nodes, reference_nodes);

        let incremental_edges: Vec<String> = store
            .active_edge_ids()?
            .into_iter()
            .filter(|id| !touches_repository(&store, id))
            .collect();
        let reference_edges: Vec<String> = reference_store
            .active_edge_ids()?
            .into_iter()
            .filter(|id| !touches_repository(&reference_store, id))
            .collect();
        assert_eq!(incremental_edges, reference_edges);
        Ok(())
    }

    fn is_repository(store: &GraphStore, id: &str) -> bool {
        store
            .node(&localmind_core::GraphNodeId::new(id))
            .ok()
            .flatten()
            .map(|node| node.kind == NodeKind::Repository)
            .unwrap_or(false)
    }

    fn touches_repository(store: &GraphStore, id: &str) -> bool {
        store
            .edge(&localmind_core::GraphEdgeId::new(id))
            .ok()
            .flatten()
            .map(|edge| edge.kind == localmind_core::EdgeKind::BelongsToProject)
            .unwrap_or(false)
    }
}
