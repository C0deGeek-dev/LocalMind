//! Architecture overview over a real indexed multi-language workspace.

use localmind_codegraph::{compute_overview, IngestBoundary, Ingester, OverviewOptions};
use localmind_store::GraphStore;
use std::fs;
use std::path::{Path, PathBuf};

fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or(());
    }
    if fs::write(&path, body).is_err() {
        unreachable!("fixture write must succeed");
    }
}

fn index() -> (tempfile::TempDir, GraphStore) {
    let temp_dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => unreachable!("temp dir: {error}"),
    };
    let root = temp_dir.path();
    let _ = fs::write(root.join(".localmind.toml"), "[learning]\nenabled = true\n");
    // A small workspace: a hub function many others call (hotspot), an entry
    // point that calls into the hub, and a second language.
    write(
        root,
        "src/core.rs",
        "pub fn hub() -> u8 { 1 }\n\
         fn a() { hub(); }\n\
         fn b() { hub(); }\n\
         pub fn run() { a(); b(); }\n",
    );
    write(
        root,
        "app/main.py",
        "def start():\n    return helper()\n\ndef helper():\n    return 1\n",
    );

    let boundary = match IngestBoundary::new(root, Vec::new()) {
        Ok(boundary) => boundary,
        Err(error) => unreachable!("boundary: {error}"),
    };
    let store = match GraphStore::open_project(root) {
        Ok(store) => store,
        Err(error) => unreachable!("store: {error}"),
    };
    let candidates: Vec<PathBuf> = ["src/core.rs", "app/main.py"]
        .iter()
        .map(|relative| root.join(relative))
        .collect();
    let mut ingester = match Ingester::new() {
        Ok(ingester) => ingester,
        Err(error) => unreachable!("ingester: {error}"),
    };
    if let Err(error) = ingester.ingest(&boundary, &candidates, &store) {
        unreachable!("ingest: {error}");
    }
    (temp_dir, store)
}

#[test]
fn overview_reports_languages_and_file_counts() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = index();
    let overview = compute_overview(&store, OverviewOptions::default())?;

    assert_eq!(overview.file_count, 2);
    let languages: Vec<&str> = overview
        .languages
        .iter()
        .map(|stat| stat.language.as_str())
        .collect();
    assert!(languages.contains(&"rust"));
    assert!(languages.contains(&"python"));
    assert!(overview.symbol_count >= 6);
    Ok(())
}

#[test]
fn overview_reports_top_packages() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = index();
    let overview = compute_overview(&store, OverviewOptions::default())?;

    let packages: Vec<&str> = overview
        .top_packages
        .iter()
        .map(|stat| stat.path.as_str())
        .collect();
    assert!(packages.contains(&"src"));
    assert!(packages.contains(&"app"));
    Ok(())
}

#[test]
fn overview_ranks_call_hotspots_and_entry_points() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = index();
    let overview = compute_overview(&store, OverviewOptions::default())?;

    // `hub` is called by a, b (and run via a/b) → highest fan-in.
    let top = overview.hotspots.first().ok_or("expected a hotspot")?;
    assert!(
        top.qualified_name.ends_with("::hub"),
        "hub should be the top hotspot, got {top:?}"
    );
    assert!(top.in_degree >= 2);

    // `run` and `start` call others but nothing calls them → entry points.
    let entries: Vec<&str> = overview
        .entry_points
        .iter()
        .map(|stat| stat.qualified_name.as_str())
        .collect();
    assert!(
        entries.iter().any(|name| name.ends_with("::run")),
        "run should be an entry point, got {entries:?}"
    );
    Ok(())
}

#[test]
fn overview_is_bounded_by_top_n() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = index();
    let overview = compute_overview(&store, OverviewOptions { top_n: 1 })?;

    assert!(overview.top_packages.len() <= 1);
    assert!(overview.hotspots.len() <= 1);
    assert!(overview.entry_points.len() <= 1);
    Ok(())
}
