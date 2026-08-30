//! Change-impact mapping: changed source spans → affected graph nodes.
//!
//! Given the line spans a diff touches, find the symbols that overlap them and
//! walk *inward* — callers of changed functions, importers of changed files —
//! over a bounded breadth-first search. Hop distance becomes a deterministic
//! risk tier, and a heuristic edge anywhere on the path is surfaced so risk is
//! never overstated. Output is top-N and risk-tiered, so it stays cold-start
//! cheap. The engine computes the mapping; the host supplies the diff spans.

use crate::CodeGraphError;
use localmind_core::{EdgeKind, GraphEndpoint, GraphNode, NodeKind};
use localmind_store::GraphStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// A contiguous range of changed lines in one repo-relative file (forward
/// slashes), as a diff would report it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangedSpan {
    pub path: String,
    pub line_start: u64,
    pub line_end: u64,
}

/// Bounds on the walk: how far to follow dependents, and how many to report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImpactOptions {
    pub max_depth: u32,
    pub max_results: usize,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_results: 20,
        }
    }
}

/// How exposed an impacted symbol is, by hop distance from the change.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    /// A directly changed symbol.
    Direct,
    /// One hop from the change (a direct caller/importer).
    High,
    /// Two hops.
    Medium,
    /// Three or more hops.
    Low,
}

impl RiskTier {
    fn for_hop(hops: u32) -> Self {
        match hops {
            0 => RiskTier::Direct,
            1 => RiskTier::High,
            2 => RiskTier::Medium,
            _ => RiskTier::Low,
        }
    }
}

/// One symbol in the impact result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImpactedSymbol {
    pub qualified_name: String,
    pub kind: String,
    pub hops: u32,
    pub risk: RiskTier,
    /// True when the shortest path that reached this symbol used a heuristic
    /// edge (e.g. a name-resolved `Calls` edge). Risk should be read with this.
    pub via_heuristic: bool,
}

/// The bounded impact of a set of changed spans.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChangeImpact {
    /// Symbols whose own span was changed.
    pub changed: Vec<ImpactedSymbol>,
    /// Symbols that depend on the changed ones, within the depth/result bounds.
    pub impacted: Vec<ImpactedSymbol>,
}

/// One reverse dependency edge: a dependent and whether the link is heuristic.
struct Dependent {
    id: String,
    heuristic: bool,
}

/// Computes the bounded change impact for the given changed spans.
pub fn compute_impact(
    store: &GraphStore,
    spans: &[ChangedSpan],
    options: ImpactOptions,
) -> Result<ChangeImpact, CodeGraphError> {
    let nodes = store.active_nodes()?;
    let node_by_id: HashMap<&str, &GraphNode> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let visited = reachable_dependents(store, &nodes, spans, options)?;

    let mut changed: Vec<ImpactedSymbol> = visited
        .iter()
        .filter(|(_, (hops, _))| *hops == 0)
        .filter_map(|(id, (_, heuristic))| {
            node_by_id
                .get(id.as_str())
                .filter(|node| is_symbol(node.kind))
                .map(|node| symbol(node, 0, *heuristic))
        })
        .collect();

    let mut impacted: Vec<ImpactedSymbol> = visited
        .iter()
        .filter(|(_, (hops, _))| *hops > 0)
        .filter_map(|(id, (hops, heuristic))| {
            node_by_id
                .get(id.as_str())
                .filter(|node| is_symbol(node.kind))
                .map(|node| symbol(node, *hops, *heuristic))
        })
        .collect();
    impacted.sort_by(|a, b| {
        a.hops
            .cmp(&b.hops)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    impacted.truncate(options.max_results);

    changed.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    Ok(ChangeImpact { changed, impacted })
}

/// The shared reverse-dependency walk: seed from nodes whose source span overlaps
/// a changed span (hop 0), then BFS inward over incoming `Calls`/`Uses` edges up
/// to `max_depth`. Returns each reached node id mapped to `(hops, via_heuristic)`.
/// Both [`compute_impact`] and change-aware staleness build on this.
pub(crate) fn reachable_dependents(
    store: &GraphStore,
    nodes: &[GraphNode],
    spans: &[ChangedSpan],
    options: ImpactOptions,
) -> Result<BTreeMap<String, (u32, bool)>, CodeGraphError> {
    // Reverse dependency map: a changed node's dependents are its callers
    // (incoming `Calls`) and, for files, its importers (incoming `Uses`).
    let mut dependents: HashMap<String, Vec<Dependent>> = HashMap::new();
    for edge in store.active_edges()? {
        if !matches!(edge.kind, EdgeKind::Calls | EdgeKind::Uses) {
            continue;
        }
        let (GraphEndpoint::Node(from), GraphEndpoint::Node(to)) = (&edge.from, &edge.to) else {
            continue;
        };
        dependents
            .entry(to.as_str().to_string())
            .or_default()
            .push(Dependent {
                id: from.as_str().to_string(),
                heuristic: edge.derivation == localmind_core::EdgeDerivation::Heuristic,
            });
    }

    let mut visited: BTreeMap<String, (u32, bool)> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for node in nodes {
        if node
            .location
            .as_ref()
            .is_some_and(|location| spans.iter().any(|span| overlaps(location, span)))
        {
            visited.insert(node.id.as_str().to_string(), (0, false));
            queue.push_back(node.id.as_str().to_string());
        }
    }

    while let Some(current) = queue.pop_front() {
        let (hops, current_heuristic) = visited.get(&current).copied().unwrap_or((0, false));
        if hops >= options.max_depth {
            continue;
        }
        let Some(parents) = dependents.get(&current) else {
            continue;
        };
        for parent in parents {
            let next_heuristic = current_heuristic || parent.heuristic;
            let next_hops = hops + 1;
            if !visited.contains_key(&parent.id) {
                visited.insert(parent.id.clone(), (next_hops, next_heuristic));
                queue.push_back(parent.id.clone());
            }
        }
    }
    Ok(visited)
}

/// Expose the risk tier for a hop distance to sibling modules (staleness).
pub(crate) fn risk_for_hop(hops: u32) -> RiskTier {
    RiskTier::for_hop(hops)
}

fn symbol(node: &GraphNode, hops: u32, via_heuristic: bool) -> ImpactedSymbol {
    ImpactedSymbol {
        qualified_name: node.qualified_name.clone(),
        kind: node.kind.as_str().to_string(),
        hops,
        risk: RiskTier::for_hop(hops),
        via_heuristic,
    }
}

/// Symbols (not files, repositories, or dependencies) are the impact unit.
fn is_symbol(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Type | NodeKind::Module | NodeKind::Test
    )
}

/// Whether a node's source location overlaps a changed span (same path, with
/// intersecting line ranges).
fn overlaps(location: &localmind_core::SourceLocation, span: &ChangedSpan) -> bool {
    location.path == span.path
        && location.line_start <= span.line_end
        && span.line_start <= location.line_end
}

#[cfg(test)]
mod tests {
    use super::{ChangeImpact, RiskTier};

    #[test]
    fn impact_serializes_stably() -> Result<(), Box<dyn std::error::Error>> {
        let impact = ChangeImpact::default();
        let json = serde_json::to_string(&impact)?;
        let back: ChangeImpact = serde_json::from_str(&json)?;
        assert_eq!(impact, back);
        Ok(())
    }

    #[test]
    fn risk_tiers_map_from_hops() {
        assert_eq!(RiskTier::for_hop(0), RiskTier::Direct);
        assert_eq!(RiskTier::for_hop(1), RiskTier::High);
        assert_eq!(RiskTier::for_hop(2), RiskTier::Medium);
        assert_eq!(RiskTier::for_hop(5), RiskTier::Low);
    }
}
