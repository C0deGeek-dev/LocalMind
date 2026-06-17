//! Code-structure graph contracts.
//!
//! Nodes describe what a workspace *is* (repositories, files, modules, types,
//! functions, tests, dependencies); edges describe how those parts relate and
//! how learned knowledge anchors onto them. Extraction is deterministic and
//! offline; every node and edge carries the same provenance and confidence
//! vocabulary as lessons so the two sides of the graph stay one system.

use crate::{Confidence, EvidenceRef, GraphEdgeId, GraphNodeId, MemoryEntryId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Repository,
    File,
    Module,
    Type,
    Function,
    Test,
    Dependency,
}

impl NodeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::File => "file",
            Self::Module => "module",
            Self::Type => "type",
            Self::Function => "function",
            Self::Test => "test",
            Self::Dependency => "dependency",
        }
    }
}

/// Shape of a `NodeKind::Type` node, when known.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeShape {
    Struct,
    Enum,
    Trait,
    Union,
}

/// Repo-relative source position of a node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceLocation {
    /// Repo-relative path with forward slashes, regardless of host OS.
    pub path: String,
    pub byte_start: u64,
    pub byte_end: u64,
    pub line_start: u64,
    pub line_end: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GraphNode {
    pub id: GraphNodeId,
    pub kind: NodeKind,
    /// Unqualified name (`norm`, `Point`, `lib.rs`).
    pub name: String,
    /// Path-qualified name (`geometry::Point::norm`); equals `name` when there
    /// is no enclosing scope.
    pub qualified_name: String,
    pub type_shape: Option<TypeShape>,
    /// `None` for nodes without a single source position
    /// (`Repository`, `Dependency`).
    pub location: Option<SourceLocation>,
    /// Fingerprint of the node's source text; drives incremental reindex.
    pub content_hash: String,
    /// Signature-only declaration with the body elided, when the node has one.
    pub skeleton: Option<String>,
    pub provenance: EvidenceRef,
    pub confidence: Confidence,
    pub created_at: Option<OffsetDateTime>,
    /// Set instead of deleting the node, so provenance survives reindexes.
    pub superseded_at: Option<OffsetDateTime>,
}

impl GraphNode {
    #[must_use]
    pub fn new(
        kind: NodeKind,
        name: impl Into<String>,
        qualified_name: impl Into<String>,
        content_hash: impl Into<String>,
        provenance: EvidenceRef,
        confidence: Confidence,
    ) -> Self {
        let name = name.into();
        let qualified_name = qualified_name.into();
        let id = stable_node_id(kind, &qualified_name);

        Self {
            id,
            kind,
            name,
            qualified_name,
            type_shape: None,
            location: None,
            content_hash: content_hash.into(),
            skeleton: None,
            provenance,
            confidence,
            created_at: None,
            superseded_at: None,
        }
    }

    #[must_use]
    pub fn with_location(mut self, location: SourceLocation) -> Self {
        self.location = Some(location);
        self
    }

    #[must_use]
    pub fn with_skeleton(mut self, skeleton: impl Into<String>) -> Self {
        self.skeleton = Some(skeleton.into());
        self
    }

    #[must_use]
    pub fn with_type_shape(mut self, shape: TypeShape) -> Self {
        self.type_shape = Some(shape);
        self
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    ImplementedBy,
    TestedBy,
    DocumentedIn,
    Uses,
    /// One callable invokes another. The call site is read from the tree, but
    /// the target is matched by name, so a `Calls` edge is always
    /// [`EdgeDerivation::Heuristic`].
    Calls,
    BelongsToProject,
    /// Learned knowledge (an accepted memory entry) anchored to a code node.
    AnchoredTo,
}

impl EdgeKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImplementedBy => "implemented_by",
            Self::TestedBy => "tested_by",
            Self::DocumentedIn => "documented_in",
            Self::Uses => "uses",
            Self::Calls => "calls",
            Self::BelongsToProject => "belongs_to_project",
            Self::AnchoredTo => "anchored_to",
        }
    }
}

/// How an edge was established. A heuristic edge must never be presented as
/// parsed fact; resolvers that guess (for example prefix-matching an import to
/// a file) must say so.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeDerivation {
    /// Read directly from the syntax tree.
    Parsed,
    /// Produced by a resolver guess; confidence carries how strong.
    Heuristic,
}

impl EdgeDerivation {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Heuristic => "heuristic",
        }
    }
}

/// An edge endpoint: either a code-structure node or an accepted memory entry
/// (lesson, bug, decision) joining the learned side of the graph.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEndpoint {
    Node(GraphNodeId),
    Memory(MemoryEntryId),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GraphEdge {
    pub id: GraphEdgeId,
    pub from: GraphEndpoint,
    pub to: GraphEndpoint,
    pub kind: EdgeKind,
    pub derivation: EdgeDerivation,
    pub confidence: Confidence,
    /// The span or import that justifies the edge.
    pub evidence: EvidenceRef,
    pub created_at: Option<OffsetDateTime>,
    pub superseded_at: Option<OffsetDateTime>,
}

impl GraphEdge {
    /// A code-structure edge between two graph nodes.
    #[must_use]
    pub fn structural(
        kind: EdgeKind,
        from: GraphNodeId,
        to: GraphNodeId,
        derivation: EdgeDerivation,
        confidence: Confidence,
        evidence: EvidenceRef,
    ) -> Self {
        let id = stable_edge_id(kind, from.as_str(), to.as_str());

        Self {
            id,
            from: GraphEndpoint::Node(from),
            to: GraphEndpoint::Node(to),
            kind,
            derivation,
            confidence,
            evidence,
            created_at: None,
            superseded_at: None,
        }
    }

    /// Anchors an accepted memory entry to the code node it is about.
    #[must_use]
    pub fn anchor(
        memory: MemoryEntryId,
        node: GraphNodeId,
        confidence: Confidence,
        evidence: EvidenceRef,
    ) -> Self {
        let id = stable_edge_id(EdgeKind::AnchoredTo, memory.as_str(), node.as_str());

        Self {
            id,
            from: GraphEndpoint::Memory(memory),
            to: GraphEndpoint::Node(node),
            kind: EdgeKind::AnchoredTo,
            derivation: EdgeDerivation::Heuristic,
            confidence,
            evidence,
            created_at: None,
            superseded_at: None,
        }
    }
}

/// Stable node id: deterministic across reindexes as long as the node keeps
/// its kind and qualified name, so anchored knowledge survives reindexing.
#[must_use]
pub fn stable_node_id(kind: NodeKind, qualified_name: &str) -> GraphNodeId {
    let hash = fingerprint64(&[kind.as_str(), qualified_name]);
    GraphNodeId::new(format!("cgn-{hash:016x}"))
}

/// Stable edge id derived from the edge kind and its endpoint ids.
#[must_use]
pub fn stable_edge_id(kind: EdgeKind, from: &str, to: &str) -> GraphEdgeId {
    let hash = fingerprint64(&[kind.as_str(), from, to]);
    GraphEdgeId::new(format!("cge-{hash:016x}"))
}

/// Deterministic fingerprint of source text for change detection. The graph is
/// derived data, so a collision costs at worst a missed or spurious reindex of
/// one node, never lost knowledge.
#[must_use]
pub fn content_fingerprint(text: &str) -> String {
    let hash = fingerprint64(&[text]);
    format!("{hash:016x}")
}

/// FNV-1a 64-bit over the parts with a separator byte between them, matching
/// the id-fingerprint convention used elsewhere in the workspace.
fn fingerprint64(parts: &[&str]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash ^= u64::from(0x1f_u8);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{
        content_fingerprint, stable_node_id, EdgeDerivation, EdgeKind, GraphEdge, GraphEndpoint,
        GraphNode, NodeKind, SourceLocation, TypeShape,
    };
    use crate::{Confidence, EvidenceKind, EvidenceRef, MemoryEntryId};

    fn evidence() -> EvidenceRef {
        EvidenceRef::new(EvidenceKind::Other("code_parse".to_string()), "span")
    }

    fn confidence() -> Confidence {
        match Confidence::new(1.0) {
            Ok(value) => value,
            Err(_) => unreachable!("1.0 is in range"),
        }
    }

    fn function_node(qualified_name: &str) -> GraphNode {
        GraphNode::new(
            NodeKind::Function,
            "norm",
            qualified_name,
            content_fingerprint("fn norm() {}"),
            evidence(),
            confidence(),
        )
    }

    #[test]
    fn node_ids_are_stable_and_kind_scoped() {
        let first = function_node("geometry::Point::norm");
        let second = function_node("geometry::Point::norm");
        assert_eq!(first.id, second.id);

        let other_name = function_node("geometry::Point::dot");
        assert_ne!(first.id, other_name.id);

        let other_kind = stable_node_id(NodeKind::Test, "geometry::Point::norm");
        assert_ne!(first.id, other_kind);
    }

    #[test]
    fn fingerprint_separator_keeps_adjacent_parts_apart() {
        let joined = stable_node_id(NodeKind::Module, "ab");
        let split = stable_node_id(NodeKind::Module, "a::b");
        assert_ne!(joined, split);
    }

    #[test]
    fn node_serializes_with_vision_vocabulary() -> Result<(), Box<dyn std::error::Error>> {
        let node = function_node("geometry::Point::norm")
            .with_location(SourceLocation {
                path: "src/geometry.rs".to_string(),
                byte_start: 120,
                byte_end: 180,
                line_start: 9,
                line_end: 12,
            })
            .with_skeleton("pub fn norm(&self) -> f64");

        let json = serde_json::to_string(&node)?;
        assert!(json.contains("\"kind\":\"function\""));
        assert!(json.contains("cgn-"));

        let back: GraphNode = serde_json::from_str(&json)?;
        assert_eq!(back, node);
        Ok(())
    }

    #[test]
    fn type_shape_rides_on_type_nodes() {
        let node = GraphNode::new(
            NodeKind::Type,
            "Point",
            "geometry::Point",
            content_fingerprint("pub struct Point;"),
            evidence(),
            confidence(),
        )
        .with_type_shape(TypeShape::Struct);

        assert_eq!(node.type_shape, Some(TypeShape::Struct));
    }

    #[test]
    fn structural_edges_connect_nodes_with_vision_edge_names(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let function = function_node("geometry::Point::norm");
        let test = GraphNode::new(
            NodeKind::Test,
            "norm_works",
            "geometry::tests::norm_works",
            content_fingerprint("#[test] fn norm_works() {}"),
            evidence(),
            confidence(),
        );

        let edge = GraphEdge::structural(
            EdgeKind::TestedBy,
            function.id.clone(),
            test.id.clone(),
            EdgeDerivation::Parsed,
            confidence(),
            evidence(),
        );

        let json = serde_json::to_string(&edge)?;
        assert!(json.contains("\"kind\":\"tested_by\""));
        assert!(json.contains("\"derivation\":\"parsed\""));
        assert_eq!(edge.from, GraphEndpoint::Node(function.id));
        assert_eq!(edge.to, GraphEndpoint::Node(test.id));
        Ok(())
    }

    #[test]
    fn calls_edges_serialize_as_snake_case_and_round_trip() -> Result<(), Box<dyn std::error::Error>>
    {
        let caller = function_node("geometry::draw");
        let callee = function_node("geometry::norm");
        let edge = GraphEdge::structural(
            EdgeKind::Calls,
            caller.id.clone(),
            callee.id.clone(),
            EdgeDerivation::Heuristic,
            Confidence::new(0.9)?,
            evidence(),
        );

        let json = serde_json::to_string(&edge)?;
        assert!(json.contains("\"kind\":\"calls\""));
        assert!(json.contains("\"derivation\":\"heuristic\""));
        let back: GraphEdge = serde_json::from_str(&json)?;
        assert_eq!(back, edge);
        assert_eq!(EdgeKind::Calls.as_str(), "calls");
        Ok(())
    }

    #[test]
    fn anchor_edges_join_memory_to_code() {
        let node = function_node("geometry::Point::norm");
        let edge = GraphEdge::anchor(
            MemoryEntryId::new("memory-1"),
            node.id.clone(),
            confidence(),
            evidence(),
        );

        assert_eq!(edge.kind, EdgeKind::AnchoredTo);
        assert_eq!(
            edge.from,
            GraphEndpoint::Memory(MemoryEntryId::new("memory-1"))
        );
        assert_eq!(edge.to, GraphEndpoint::Node(node.id));
    }

    #[test]
    fn content_fingerprint_tracks_text_changes() {
        let before = content_fingerprint("fn norm() {}");
        let after = content_fingerprint("fn norm() { 1.0 }");
        assert_ne!(before, after);
        assert_eq!(before, content_fingerprint("fn norm() {}"));
    }
}
