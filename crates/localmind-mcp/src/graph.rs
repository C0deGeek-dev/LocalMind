//! Graph query tools for MCP hosts.
//!
//! Transport-agnostic tool contracts: typed requests and responses plus a
//! dispatcher over the project store. A host MCP server mounts these by name
//! and serializes the shapes as-is; names and shapes are original to
//! LocalMind.

use localmind_core::{GraphEndpoint, GraphNode, NodeKind};
use localmind_store::{GraphStore, GraphStoreError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Nodes reachable from a symbol within a hop bound.
pub const TOOL_SYMBOL_NEIGHBORHOOD: &str = "memory_symbol_neighborhood";
/// One shortest connection between two symbols.
pub const TOOL_SYMBOL_CONNECTION: &str = "memory_symbol_connection";
/// Tests attached to a symbol.
pub const TOOL_SYMBOL_COVERAGE: &str = "memory_symbol_coverage";
/// Accepted knowledge anchored to a symbol.
pub const TOOL_SYMBOL_KNOWLEDGE: &str = "memory_symbol_knowledge";

/// All graph tools this engine version offers, by name.
#[must_use]
pub fn tool_names() -> [&'static str; 4] {
    [
        TOOL_SYMBOL_NEIGHBORHOOD,
        TOOL_SYMBOL_CONNECTION,
        TOOL_SYMBOL_COVERAGE,
        TOOL_SYMBOL_KNOWLEDGE,
    ]
}

/// A request to any of the graph tools.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum GraphToolRequest {
    MemorySymbolNeighborhood {
        symbol: String,
        #[serde(default = "default_depth")]
        depth: u32,
    },
    MemorySymbolConnection {
        from: String,
        to: String,
        #[serde(default = "default_connection_bound")]
        max_hops: u32,
    },
    MemorySymbolCoverage {
        symbol: String,
    },
    MemorySymbolKnowledge {
        symbol: String,
    },
}

fn default_depth() -> u32 {
    2
}

fn default_connection_bound() -> u32 {
    6
}

/// A code node flattened for tool output.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SymbolSummary {
    pub kind: String,
    pub qualified_name: String,
    pub path: Option<String>,
    pub skeleton: Option<String>,
}

impl SymbolSummary {
    fn from_node(node: &GraphNode) -> Self {
        Self {
            kind: node.kind.as_str().to_string(),
            qualified_name: node.qualified_name.clone(),
            path: node.location.as_ref().map(|location| location.path.clone()),
            skeleton: node.skeleton.clone(),
        }
    }
}

/// Knowledge anchored to a symbol: the memory id and the anchor's confidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AnchoredKnowledge {
    pub memory_id: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum GraphToolResponse {
    Neighborhood {
        symbol: SymbolSummary,
        neighbors: Vec<SymbolSummary>,
    },
    Connection {
        steps: Vec<SymbolSummary>,
    },
    NoConnection,
    Coverage {
        symbol: SymbolSummary,
        tests: Vec<SymbolSummary>,
    },
    Knowledge {
        symbol: SymbolSummary,
        knowledge: Vec<AnchoredKnowledge>,
    },
}

#[derive(Debug, Error)]
pub enum GraphToolError {
    #[error("symbol {symbol:?} is not in the graph")]
    UnknownSymbol { symbol: String },
    #[error("symbol {symbol:?} is ambiguous; use its qualified name")]
    AmbiguousSymbol { symbol: String },
    #[error(transparent)]
    Store(#[from] GraphStoreError),
}

/// Dispatches one tool request against the project's graph store.
pub fn handle(
    store: &GraphStore,
    request: &GraphToolRequest,
) -> Result<GraphToolResponse, GraphToolError> {
    match request {
        GraphToolRequest::MemorySymbolNeighborhood { symbol, depth } => {
            let node = resolve_symbol(store, symbol)?;
            let neighbors = store
                .neighbors(&node.id, *depth)?
                .iter()
                .map(SymbolSummary::from_node)
                .collect();
            Ok(GraphToolResponse::Neighborhood {
                symbol: SymbolSummary::from_node(&node),
                neighbors,
            })
        }
        GraphToolRequest::MemorySymbolConnection { from, to, max_hops } => {
            let from = resolve_symbol(store, from)?;
            let to = resolve_symbol(store, to)?;
            let Some(path) = store.path_between(&from.id, &to.id, *max_hops)? else {
                return Ok(GraphToolResponse::NoConnection);
            };
            let mut steps = Vec::new();
            for id in path {
                if let Some(node) = store.node(&id)? {
                    steps.push(SymbolSummary::from_node(&node));
                }
            }
            Ok(GraphToolResponse::Connection { steps })
        }
        GraphToolRequest::MemorySymbolCoverage { symbol } => {
            let node = resolve_symbol(store, symbol)?;
            let tests = store
                .tests_of(&node.id)?
                .iter()
                .map(SymbolSummary::from_node)
                .collect();
            Ok(GraphToolResponse::Coverage {
                symbol: SymbolSummary::from_node(&node),
                tests,
            })
        }
        GraphToolRequest::MemorySymbolKnowledge { symbol } => {
            let node = resolve_symbol(store, symbol)?;
            let knowledge = store
                .memories_anchored_to(&node.id)?
                .iter()
                .filter_map(|edge| match &edge.from {
                    GraphEndpoint::Memory(memory_id) => Some(AnchoredKnowledge {
                        memory_id: memory_id.as_str().to_string(),
                        confidence: edge.confidence.value(),
                    }),
                    GraphEndpoint::Node(_) => None,
                })
                .collect();
            Ok(GraphToolResponse::Knowledge {
                symbol: SymbolSummary::from_node(&node),
                knowledge,
            })
        }
    }
}

/// Resolves a symbol string to exactly one active node. Plain names work when
/// unique; otherwise the qualified name disambiguates. Repository nodes are
/// not addressable here.
fn resolve_symbol(store: &GraphStore, symbol: &str) -> Result<GraphNode, GraphToolError> {
    let matches: Vec<GraphNode> = store
        .find_symbol(symbol)?
        .into_iter()
        .filter(|node| node.kind != NodeKind::Repository)
        .collect();
    match matches.len() {
        0 => Err(GraphToolError::UnknownSymbol {
            symbol: symbol.to_string(),
        }),
        1 => Ok(matches
            .into_iter()
            .next()
            .ok_or(GraphToolError::UnknownSymbol {
                symbol: symbol.to_string(),
            })?),
        _ => Err(GraphToolError::AmbiguousSymbol {
            symbol: symbol.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{handle, tool_names, GraphToolError, GraphToolRequest, GraphToolResponse};
    use localmind_core::{
        content_fingerprint, Confidence, EdgeDerivation, EdgeKind, EvidenceKind, EvidenceRef,
        GraphEdge, GraphNode, MemoryEntryId, NodeKind, SourceLocation,
    };
    use localmind_store::GraphStore;
    use std::fs;

    fn store_with_fixture() -> Result<(tempfile::TempDir, GraphStore), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        fs::write(
            temp_dir.path().join(".localmind.toml"),
            "[learning]\nenabled = true\n",
        )?;
        let store = GraphStore::open_project(temp_dir.path())?;

        let file = node(NodeKind::File, "src/geometry.rs");
        let function = node(NodeKind::Function, "src/geometry.rs::norm");
        let test = node(NodeKind::Test, "src/geometry.rs::tests::norm_is_positive");
        for graph_node in [&file, &function, &test] {
            store.upsert_node(graph_node)?;
        }
        store.upsert_edge(&edge(EdgeKind::ImplementedBy, &file, &function))?;
        store.upsert_edge(&edge(EdgeKind::TestedBy, &function, &test))?;
        store.upsert_edge(&GraphEdge::anchor(
            MemoryEntryId::new("memory-1"),
            function.id.clone(),
            Confidence::new(0.8)?,
            EvidenceRef::new(EvidenceKind::ManualNote, "hint"),
        ))?;
        Ok((temp_dir, store))
    }

    fn node(kind: NodeKind, qualified: &str) -> GraphNode {
        let name = qualified.rsplit("::").next().unwrap_or(qualified);
        let mut graph_node = GraphNode::new(
            kind,
            name,
            qualified,
            content_fingerprint(qualified),
            EvidenceRef::new(EvidenceKind::CodeParse, "span"),
            match Confidence::new(1.0) {
                Ok(value) => value,
                Err(_) => unreachable!("1.0 is in range"),
            },
        );
        graph_node = graph_node.with_location(SourceLocation {
            path: "src/geometry.rs".to_string(),
            byte_start: 0,
            byte_end: 1,
            line_start: 1,
            line_end: 1,
        });
        graph_node
    }

    fn edge(kind: EdgeKind, from: &GraphNode, to: &GraphNode) -> GraphEdge {
        GraphEdge::structural(
            kind,
            from.id.clone(),
            to.id.clone(),
            EdgeDerivation::Parsed,
            match Confidence::new(1.0) {
                Ok(value) => value,
                Err(_) => unreachable!("1.0 is in range"),
            },
            EvidenceRef::new(EvidenceKind::CodeParse, "span"),
        )
    }

    #[test]
    fn tool_names_are_stable_contracts() {
        assert_eq!(
            tool_names(),
            [
                "memory_symbol_neighborhood",
                "memory_symbol_connection",
                "memory_symbol_coverage",
                "memory_symbol_knowledge",
            ]
        );
    }

    #[test]
    fn requests_round_trip_through_their_wire_shape() -> Result<(), Box<dyn std::error::Error>> {
        let wire = r#"{"tool":"memory_symbol_neighborhood","symbol":"norm","depth":1}"#;
        let request: GraphToolRequest = serde_json::from_str(wire)?;
        assert!(matches!(
            request,
            GraphToolRequest::MemorySymbolNeighborhood { ref symbol, depth: 1 } if symbol == "norm"
        ));

        let defaulted: GraphToolRequest =
            serde_json::from_str(r#"{"tool":"memory_symbol_knowledge","symbol":"norm"}"#)?;
        assert!(matches!(
            defaulted,
            GraphToolRequest::MemorySymbolKnowledge { .. }
        ));
        Ok(())
    }

    #[test]
    fn neighborhood_coverage_and_knowledge_answer_for_a_symbol(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, store) = store_with_fixture()?;

        let response = handle(
            &store,
            &GraphToolRequest::MemorySymbolNeighborhood {
                symbol: "norm".to_string(),
                depth: 2,
            },
        )?;
        let GraphToolResponse::Neighborhood { symbol, neighbors } = response else {
            return Err("expected a neighborhood".into());
        };
        assert_eq!(symbol.qualified_name, "src/geometry.rs::norm");
        assert_eq!(neighbors.len(), 2);

        let response = handle(
            &store,
            &GraphToolRequest::MemorySymbolCoverage {
                symbol: "norm".to_string(),
            },
        )?;
        let GraphToolResponse::Coverage { tests, .. } = response else {
            return Err("expected coverage".into());
        };
        assert_eq!(tests.len(), 1);

        let response = handle(
            &store,
            &GraphToolRequest::MemorySymbolKnowledge {
                symbol: "norm".to_string(),
            },
        )?;
        let GraphToolResponse::Knowledge { knowledge, .. } = response else {
            return Err("expected knowledge".into());
        };
        assert_eq!(knowledge.len(), 1);
        assert_eq!(knowledge[0].memory_id, "memory-1");
        Ok(())
    }

    #[test]
    fn connections_walk_the_graph_within_the_bound() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, store) = store_with_fixture()?;

        let response = handle(
            &store,
            &GraphToolRequest::MemorySymbolConnection {
                from: "src/geometry.rs".to_string(),
                to: "norm_is_positive".to_string(),
                max_hops: 4,
            },
        )?;
        let GraphToolResponse::Connection { steps } = response else {
            return Err("expected a connection".into());
        };
        assert_eq!(steps.len(), 3);
        Ok(())
    }

    #[test]
    fn unknown_and_ambiguous_symbols_are_typed_errors() -> Result<(), Box<dyn std::error::Error>> {
        let (_dir, store) = store_with_fixture()?;
        store.upsert_node(&node(NodeKind::Function, "src/render.rs::norm"))?;

        assert!(matches!(
            handle(
                &store,
                &GraphToolRequest::MemorySymbolCoverage {
                    symbol: "missing".to_string()
                }
            ),
            Err(GraphToolError::UnknownSymbol { .. })
        ));
        assert!(matches!(
            handle(
                &store,
                &GraphToolRequest::MemorySymbolCoverage {
                    symbol: "norm".to_string()
                }
            ),
            Err(GraphToolError::AmbiguousSymbol { .. })
        ));

        // The qualified name still disambiguates.
        assert!(handle(
            &store,
            &GraphToolRequest::MemorySymbolCoverage {
                symbol: "src/geometry.rs::norm".to_string()
            }
        )
        .is_ok());
        Ok(())
    }
}
