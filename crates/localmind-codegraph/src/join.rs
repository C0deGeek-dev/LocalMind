//! The join: anchoring accepted memory to code nodes.
//!
//! When a lesson, bug, or decision is accepted, the host passes its hints
//! (related files, related entities) and the engine resolves them against the
//! active graph with the same discipline as any other edge: an exact
//! qualified-name match anchors with high confidence, a unique plain-name
//! match anchors as a weaker heuristic, and ambiguity anchors nothing.

use crate::CodeGraphError;
use localmind_core::{
    Confidence, EvidenceKind, EvidenceRef, GraphEdge, GraphNode, MemoryEntryId, NodeKind,
};
use localmind_store::GraphStore;

const QUALIFIED_MATCH_CONFIDENCE: f32 = 0.9;
const NAME_MATCH_CONFIDENCE: f32 = 0.6;

/// What one anchoring attempt produced.
#[derive(Debug, Default)]
pub struct AnchorReport {
    pub anchored: usize,
    pub unresolved: Vec<String>,
}

/// Anchors a memory entry to the code nodes its hints resolve to. Hints are
/// matched against active nodes: first by qualified name or source path
/// (high confidence), then by unique plain name (heuristic). A hint matching
/// several nodes anchors nothing — a wrong anchor misleads retrieval forever.
pub fn anchor_memory(
    store: &GraphStore,
    memory_id: &MemoryEntryId,
    hints: &[String],
) -> Result<AnchorReport, CodeGraphError> {
    let mut candidates: Vec<GraphNode> = Vec::new();
    for kind in [
        NodeKind::File,
        NodeKind::Module,
        NodeKind::Type,
        NodeKind::Function,
        NodeKind::Test,
    ] {
        candidates.extend(store.nodes_by_kind(kind)?);
    }

    let mut report = AnchorReport::default();
    for hint in hints {
        let hint = hint.trim();
        if hint.is_empty() {
            continue;
        }
        let normalized = hint.replace('\\', "/");

        let qualified: Vec<&GraphNode> = candidates
            .iter()
            .filter(|node| {
                node.qualified_name == normalized
                    || node
                        .location
                        .as_ref()
                        .map(|location| location.path == normalized)
                        .unwrap_or(false)
            })
            .collect();
        let (target, confidence) = match qualified.as_slice() {
            [target] => (*target, QUALIFIED_MATCH_CONFIDENCE),
            [] => {
                let named: Vec<&GraphNode> = candidates
                    .iter()
                    .filter(|node| node.name == normalized)
                    .collect();
                match named.as_slice() {
                    [target] => (*target, NAME_MATCH_CONFIDENCE),
                    _ => {
                        report.unresolved.push(hint.to_string());
                        continue;
                    }
                }
            }
            _ => {
                report.unresolved.push(hint.to_string());
                continue;
            }
        };

        let edge = GraphEdge::anchor(
            memory_id.clone(),
            target.id.clone(),
            Confidence::new(confidence)?,
            EvidenceRef::new(
                EvidenceKind::ManualNote,
                format!("accepted memory hint {hint}"),
            ),
        );
        store.upsert_edge(&edge)?;
        report.anchored += 1;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::anchor_memory;
    use localmind_core::{
        content_fingerprint, Confidence, EvidenceKind, EvidenceRef, GraphNode, MemoryEntryId,
        NodeKind,
    };
    use localmind_store::GraphStore;
    use std::fs;

    fn store() -> Result<(tempfile::TempDir, GraphStore), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let store = GraphStore::open_project(temp_dir.path())?;
        Ok((temp_dir, store))
    }

    fn function(qualified: &str) -> GraphNode {
        let name = qualified.rsplit("::").next().unwrap_or(qualified);
        GraphNode::new(
            NodeKind::Function,
            name,
            qualified,
            content_fingerprint(qualified),
            EvidenceRef::new(EvidenceKind::CodeParse, "span"),
            match Confidence::new(1.0) {
                Ok(value) => value,
                Err(_) => unreachable!("1.0 is in range"),
            },
        )
    }

    #[test]
    fn qualified_hints_anchor_with_high_confidence() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, store) = store()?;
        let target = function("src/geometry.rs::norm");
        store.upsert_node(&target)?;

        let memory_id = MemoryEntryId::new("memory-1");
        let report = anchor_memory(&store, &memory_id, &["src/geometry.rs::norm".to_string()])?;

        assert_eq!(report.anchored, 1);
        let anchors = store.anchors_of_memory(&memory_id)?;
        assert_eq!(anchors.len(), 1);
        assert!((anchors[0].confidence.value() - 0.9).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn unique_plain_names_anchor_as_heuristic() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, store) = store()?;
        store.upsert_node(&function("src/geometry.rs::norm"))?;

        let memory_id = MemoryEntryId::new("memory-1");
        let report = anchor_memory(&store, &memory_id, &["norm".to_string()])?;

        assert_eq!(report.anchored, 1);
        let anchors = store.anchors_of_memory(&memory_id)?;
        assert!((anchors[0].confidence.value() - 0.6).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn ambiguous_hints_anchor_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, store) = store()?;
        store.upsert_node(&function("src/geometry.rs::norm"))?;
        store.upsert_node(&function("src/render.rs::norm"))?;

        let memory_id = MemoryEntryId::new("memory-1");
        let report = anchor_memory(&store, &memory_id, &["norm".to_string()])?;

        assert_eq!(report.anchored, 0);
        assert_eq!(report.unresolved, vec!["norm".to_string()]);
        assert!(store.anchors_of_memory(&memory_id)?.is_empty());
        Ok(())
    }
}
