//! Deterministic architecture overview computed over the graph.
//!
//! A single bounded pass over the active nodes and edges yields the orientation
//! an agent would otherwise reconstruct by reading files: the language and file
//! breakdown, the busiest packages, the call-fan-in hotspots, and the
//! entry-point surface. Output is counts and top-N lists only — no prose, no
//! full-graph dump — so it stays a small, stable token footprint. This is the
//! structured input the cold-start primer distils.

use crate::language::Language;
use crate::CodeGraphError;
use localmind_core::{EdgeKind, GraphEndpoint, NodeKind};
use localmind_store::GraphStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How much of each top-N list to keep. Bounds the overview's size regardless
/// of repository scale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverviewOptions {
    pub top_n: usize,
}

impl Default for OverviewOptions {
    fn default() -> Self {
        Self { top_n: 10 }
    }
}

/// File count for one detected language.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LanguageStat {
    pub language: String,
    pub file_count: usize,
}

/// File count for one package (the directory a file lives in).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PackageStat {
    pub path: String,
    pub file_count: usize,
}

/// A symbol with its call fan-in and fan-out, for hotspots and entry points.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SymbolStat {
    pub qualified_name: String,
    pub kind: String,
    /// Inbound `Calls` edges (how many call sites resolve to this symbol).
    pub in_degree: usize,
    /// Outbound `Calls` edges (how many distinct callees this symbol resolves to).
    pub out_degree: usize,
}

/// A deterministic, bounded snapshot of a repository's shape.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchitectureOverview {
    pub file_count: usize,
    pub symbol_count: usize,
    /// Languages by file count, descending then by name.
    pub languages: Vec<LanguageStat>,
    /// Busiest packages by file count, descending then by path (top-N).
    pub top_packages: Vec<PackageStat>,
    /// Functions nothing in the repo calls but that call something — the
    /// orchestration/entry surface — ranked by fan-out (top-N).
    pub entry_points: Vec<SymbolStat>,
    /// The most-called symbols by `Calls` fan-in (top-N).
    pub hotspots: Vec<SymbolStat>,
}

/// Computes the overview from the active graph in the store. Reads node kinds
/// and active edges once; everything else is in-memory tallying and sorting.
pub fn compute_overview(
    store: &GraphStore,
    options: OverviewOptions,
) -> Result<ArchitectureOverview, CodeGraphError> {
    let files = store.nodes_by_kind(NodeKind::File)?;
    let mut language_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut package_counts: BTreeMap<String, usize> = BTreeMap::new();
    for file in &files {
        let language = Language::from_path(&file.qualified_name)
            .map(|language| language.as_str().to_string())
            .unwrap_or_else(|| "other".to_string());
        *language_counts.entry(language).or_default() += 1;
        *package_counts
            .entry(package_of(&file.qualified_name))
            .or_default() += 1;
    }

    let mut symbols = Vec::new();
    for kind in [
        NodeKind::Function,
        NodeKind::Type,
        NodeKind::Module,
        NodeKind::Test,
    ] {
        symbols.extend(store.nodes_by_kind(kind)?);
    }
    let symbol_count = symbols.len();

    let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
    let mut out_degree: BTreeMap<String, usize> = BTreeMap::new();
    for edge in store.active_edges()? {
        if edge.kind != EdgeKind::Calls {
            continue;
        }
        if let GraphEndpoint::Node(to) = &edge.to {
            *in_degree.entry(to.as_str().to_string()).or_default() += 1;
        }
        if let GraphEndpoint::Node(from) = &edge.from {
            *out_degree.entry(from.as_str().to_string()).or_default() += 1;
        }
    }

    let stat = |node: &localmind_core::GraphNode| SymbolStat {
        qualified_name: node.qualified_name.clone(),
        kind: node.kind.as_str().to_string(),
        in_degree: in_degree.get(node.id.as_str()).copied().unwrap_or(0),
        out_degree: out_degree.get(node.id.as_str()).copied().unwrap_or(0),
    };

    let mut hotspots: Vec<SymbolStat> = symbols
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Function | NodeKind::Type))
        .map(&stat)
        .filter(|symbol| symbol.in_degree > 0)
        .collect();
    hotspots.sort_by(|a, b| {
        b.in_degree
            .cmp(&a.in_degree)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    hotspots.truncate(options.top_n);

    let mut entry_points: Vec<SymbolStat> = symbols
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .map(&stat)
        .filter(|symbol| symbol.in_degree == 0 && symbol.out_degree > 0)
        .collect();
    entry_points.sort_by(|a, b| {
        b.out_degree
            .cmp(&a.out_degree)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    entry_points.truncate(options.top_n);

    Ok(ArchitectureOverview {
        file_count: files.len(),
        symbol_count,
        languages: sorted_by_count(language_counts, |language, file_count| LanguageStat {
            language,
            file_count,
        }),
        top_packages: {
            let mut packages = sorted_by_count(package_counts, |path, file_count| PackageStat {
                path,
                file_count,
            });
            packages.truncate(options.top_n);
            packages
        },
        entry_points,
        hotspots,
    })
}

/// The package a file belongs to: its directory, or `"."` for a root file.
fn package_of(relative: &str) -> String {
    match relative.rsplit_once('/') {
        Some((directory, _)) if !directory.is_empty() => directory.to_string(),
        _ => ".".to_string(),
    }
}

/// Turns a count map into a vec sorted by count descending, then by key.
fn sorted_by_count<T>(
    counts: BTreeMap<String, usize>,
    make: impl Fn(String, usize) -> T,
) -> Vec<T> {
    let mut pairs: Vec<(String, usize)> = counts.into_iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs
        .into_iter()
        .map(|(key, count)| make(key, count))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::ArchitectureOverview;

    #[test]
    fn overview_serializes_stably() -> Result<(), Box<dyn std::error::Error>> {
        let overview = ArchitectureOverview::default();
        let json = serde_json::to_string(&overview)?;
        let back: ArchitectureOverview = serde_json::from_str(&json)?;
        assert_eq!(overview, back);
        Ok(())
    }

    #[test]
    fn package_of_handles_root_and_nested() {
        assert_eq!(super::package_of("src/app/main.rs"), "src/app");
        assert_eq!(super::package_of("main.rs"), ".");
    }
}
