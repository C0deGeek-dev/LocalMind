//! Call and import edge fixtures: callers/callees and module dependencies,
//! across languages, with honest derivation and stable, supersedable ids.

use localmind_codegraph::{
    resolve_edges, AdmittedFile, CodeIntelligenceProvider, IngestBoundary, Ingester,
    NativeProvider, ParsedFile, Reindexer,
};
use localmind_core::{
    stable_edge_id, stable_node_id, EdgeDerivation, EdgeKind, GraphEdge, GraphEndpoint, NodeKind,
};
use localmind_store::GraphStore;
use std::fs;
use std::path::{Path, PathBuf};

fn parse(relative: &str, source: &str) -> ParsedFile {
    let file = AdmittedFile {
        absolute: PathBuf::from("unused"),
        relative: relative.to_string(),
    };
    let mut provider = match NativeProvider::new() {
        Ok(provider) => provider,
        Err(error) => unreachable!("provider must build: {error}"),
    };
    match provider.parse_file(&file, source) {
        Ok(parsed) => parsed,
        Err(error) => unreachable!("{relative} must parse: {error}"),
    }
}

fn of_kind(edges: &[GraphEdge], kind: EdgeKind) -> Vec<&GraphEdge> {
    edges.iter().filter(|edge| edge.kind == kind).collect()
}

#[test]
fn rust_call_edge_is_heuristic() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse("src/x.rs", "fn a() { b(); }\nfn b() {}\n");
    let edges = resolve_edges(&[parsed])?;

    let calls = of_kind(&edges, EdgeKind::Calls);
    assert_eq!(calls.len(), 1, "exactly one call edge a -> b");
    assert_eq!(calls[0].derivation, EdgeDerivation::Heuristic);
    assert!(calls[0].confidence.value() < 1.0);
    assert_eq!(
        calls[0].from,
        GraphEndpoint::Node(stable_node_id(NodeKind::Function, "src/x.rs::a"))
    );
    assert_eq!(
        calls[0].to,
        GraphEndpoint::Node(stable_node_id(NodeKind::Function, "src/x.rs::b"))
    );
    Ok(())
}

#[test]
fn python_call_edge_is_heuristic() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse(
        "app.py",
        "def a():\n    return b()\n\ndef b():\n    return 1\n",
    );
    let edges = resolve_edges(&[parsed])?;

    let calls = of_kind(&edges, EdgeKind::Calls);
    assert!(
        calls
            .iter()
            .any(|edge| edge.derivation == EdgeDerivation::Heuristic),
        "python call a -> b must be a heuristic edge; got {calls:?}"
    );
    Ok(())
}

#[test]
fn python_import_resolves_to_an_in_repo_file() -> Result<(), Box<dyn std::error::Error>> {
    let util = parse("util.py", "def helper():\n    return 1\n");
    let main = parse(
        "main.py",
        "from util import helper\n\ndef run():\n    return helper()\n",
    );
    let edges = resolve_edges(&[util, main])?;

    let uses = of_kind(&edges, EdgeKind::Uses);
    assert!(
        !uses.is_empty(),
        "the python import must produce a uses edge; got {edges:?}"
    );
    Ok(())
}

#[test]
fn javascript_import_resolves_to_an_in_repo_file() -> Result<(), Box<dyn std::error::Error>> {
    let util = parse("util.js", "export function helper() { return 1; }\n");
    let app = parse(
        "app.js",
        "import { helper } from \"./util\";\nfunction run() { return helper(); }\n",
    );
    let edges = resolve_edges(&[util, app])?;

    assert!(
        !of_kind(&edges, EdgeKind::Uses).is_empty(),
        "the js import must produce a uses edge; got {edges:?}"
    );
    Ok(())
}

#[test]
fn heuristic_calls_are_distinguishable_from_parsed_edges() -> Result<(), Box<dyn std::error::Error>>
{
    let parsed = parse("src/x.rs", "fn a() { b(); }\nfn b() {}\n");
    let edges = resolve_edges(&[parsed])?;

    // A call edge is heuristic; a containment edge is parsed fact. Derivation
    // rides on every edge, so retrieval can weight/label them differently.
    let call = of_kind(&edges, EdgeKind::Calls);
    let implemented = of_kind(&edges, EdgeKind::ImplementedBy);
    assert!(call
        .iter()
        .all(|e| e.derivation == EdgeDerivation::Heuristic));
    assert!(implemented
        .iter()
        .all(|e| e.derivation == EdgeDerivation::Parsed));
    assert!(!call.is_empty() && !implemented.is_empty());
    Ok(())
}

fn candidates(root: &Path, relatives: &[&str]) -> Vec<PathBuf> {
    relatives
        .iter()
        .map(|relative| root.join(relative))
        .filter(|path| path.exists())
        .collect()
}

#[test]
fn renaming_a_callee_retires_the_stale_edge_and_creates_a_new_one(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.path();
    fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/x.rs"), "fn a() { b(); }\nfn b() {}\n")?;

    let boundary = IngestBoundary::new(root, Vec::new())?;
    let store = GraphStore::open_project(root)?;
    Ingester::new()?.ingest(&boundary, &candidates(root, &["src/x.rs"]), &store)?;

    let a_id = stable_node_id(NodeKind::Function, "src/x.rs::a");
    let b_id = stable_node_id(NodeKind::Function, "src/x.rs::b");
    let old_edge = stable_edge_id(EdgeKind::Calls, a_id.as_str(), b_id.as_str());
    assert!(
        store
            .edge(&old_edge)?
            .is_some_and(|edge| edge.superseded_at.is_none()),
        "the a -> b call edge must be active after ingest"
    );

    // Rename the callee b to c and reindex.
    fs::write(root.join("src/x.rs"), "fn a() { c(); }\nfn c() {}\n")?;
    let mut reindexer = Reindexer::new()?;
    let mut plan = reindexer.plan(&boundary, &candidates(root, &["src/x.rs"]), &store)?;
    reindexer.run(&boundary, &store, &mut plan, usize::MAX)?;

    // The stale edge to the gone callee is retired, not dropped; a fresh edge
    // to the renamed callee exists.
    let retired = store.edge(&old_edge)?.ok_or("stale edge row must remain")?;
    assert!(
        retired.superseded_at.is_some(),
        "the edge to the renamed-away callee must be superseded"
    );
    let c_id = stable_node_id(NodeKind::Function, "src/x.rs::c");
    let new_edge = stable_edge_id(EdgeKind::Calls, a_id.as_str(), c_id.as_str());
    assert!(
        store
            .edge(&new_edge)?
            .is_some_and(|edge| edge.superseded_at.is_none()),
        "a fresh a -> c call edge must exist after the rename"
    );
    Ok(())
}
