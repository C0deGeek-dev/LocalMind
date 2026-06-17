//! Change-impact over a real indexed graph: changed spans → affected symbols,
//! risk-tiered, derivation-honest, and bounded.

use localmind_codegraph::{
    compute_impact, ChangedSpan, ImpactOptions, IngestBoundary, Ingester, RiskTier,
};
use localmind_store::GraphStore;
use std::fs;
use std::path::{Path, PathBuf};

const CORE: &str = "\
pub fn hub() -> u8 { 1 }
fn a() { hub(); }
fn b() { a(); }
";

fn index(root: &Path) -> GraphStore {
    let _ = fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n");
    let _ = fs::create_dir_all(root.join("src"));
    if fs::write(root.join("src/core.rs"), CORE).is_err() {
        unreachable!("fixture write must succeed");
    }
    let boundary = match IngestBoundary::new(root, Vec::new()) {
        Ok(boundary) => boundary,
        Err(error) => unreachable!("boundary: {error}"),
    };
    let store = match GraphStore::open_project(root) {
        Ok(store) => store,
        Err(error) => unreachable!("store: {error}"),
    };
    let candidates: Vec<PathBuf> = vec![root.join("src/core.rs")];
    let mut ingester = match Ingester::new() {
        Ok(ingester) => ingester,
        Err(error) => unreachable!("ingester: {error}"),
    };
    if let Err(error) = ingester.ingest(&boundary, &candidates, &store) {
        unreachable!("ingest: {error}");
    }
    store
}

fn span(line_start: u64, line_end: u64) -> ChangedSpan {
    ChangedSpan {
        path: "src/core.rs".to_string(),
        line_start,
        line_end,
    }
}

#[test]
fn changed_span_maps_to_overlapping_symbol() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = index(dir.path());

    // Line 1 is `hub`.
    let impact = compute_impact(&store, &[span(1, 1)], ImpactOptions::default())?;
    assert!(
        impact
            .changed
            .iter()
            .any(|symbol| symbol.qualified_name.ends_with("::hub")),
        "the changed span over hub must map to hub; got {:?}",
        impact.changed
    );
    Ok(())
}

#[test]
fn impact_walks_callers_with_risk_tiers() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = index(dir.path());

    // Change `hub` (line 1). a() calls hub (1 hop), b() calls a (2 hops).
    let impact = compute_impact(&store, &[span(1, 1)], ImpactOptions::default())?;

    let a = impact
        .impacted
        .iter()
        .find(|symbol| symbol.qualified_name.ends_with("::a"))
        .ok_or("a must be impacted (direct caller)")?;
    assert_eq!(a.hops, 1);
    assert_eq!(a.risk, RiskTier::High);

    let b = impact
        .impacted
        .iter()
        .find(|symbol| symbol.qualified_name.ends_with("::b"))
        .ok_or("b must be impacted (transitive caller)")?;
    assert_eq!(b.hops, 2);
    assert_eq!(b.risk, RiskTier::Medium);
    Ok(())
}

#[test]
fn impact_paths_through_calls_are_flagged_heuristic() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = index(dir.path());

    // Callers are reached over `Calls` edges, which are always heuristic.
    let impact = compute_impact(&store, &[span(1, 1)], ImpactOptions::default())?;
    assert!(
        impact.impacted.iter().all(|symbol| symbol.via_heuristic),
        "caller impact rides heuristic Calls edges and must be flagged"
    );
    Ok(())
}

#[test]
fn impact_is_bounded_by_depth_and_results() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = index(dir.path());

    // Depth 1 stops at direct callers: a is in, b (2 hops) is out.
    let shallow = compute_impact(
        &store,
        &[span(1, 1)],
        ImpactOptions {
            max_depth: 1,
            max_results: 20,
        },
    )?;
    assert!(shallow.impacted.iter().all(|symbol| symbol.hops <= 1));
    assert!(!shallow
        .impacted
        .iter()
        .any(|symbol| symbol.qualified_name.ends_with("::b")));

    // Result cap is honoured.
    let capped = compute_impact(
        &store,
        &[span(1, 1)],
        ImpactOptions {
            max_depth: 5,
            max_results: 1,
        },
    )?;
    assert!(capped.impacted.len() <= 1);
    Ok(())
}
