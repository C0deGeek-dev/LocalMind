//! `localmind graph reindex`: build the code graph over a repository tree.
//!
//! LocalMind's codegraph engine walks nothing itself — the host enumerates the
//! files it may read. This command does that walk (skipping VCS, build, and
//! vendored directories), admits them through an `IngestBoundary`, and drives
//! the resumable `Reindexer` to completion over the project's `GraphStore`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use localmind_codegraph::{IngestBoundary, Reindexer};
use localmind_store::GraphStore;

/// Directory names never descended into during the walk. Everything else that
/// begins with a dot is also skipped (VCS metadata, tool caches).
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
];

/// Plan actions applied per resumable batch.
const BATCH: usize = 256;

pub fn reindex(root: PathBuf, project: PathBuf) -> Result<()> {
    let store = GraphStore::open_project(&project)
        .with_context(|| format!("opening graph store for project {}", project.display()))?;
    let boundary = IngestBoundary::new(root.as_path(), Vec::new())
        .with_context(|| format!("building ingest boundary at {}", root.display()))?;

    let mut candidates = Vec::new();
    collect_files(&root, &mut candidates)?;
    println!(
        "Candidates: {} files under {}",
        candidates.len(),
        root.display()
    );

    let mut reindexer = Reindexer::new()?;
    let mut plan = reindexer.plan(&boundary, &candidates, &store)?;
    println!(
        "Plan: {} to index, {} to prune, {} unchanged, {} rejected",
        plan.index.len(),
        plan.prune.len(),
        plan.unchanged,
        plan.rejected.len()
    );

    let mut reindexed = 0usize;
    let mut pruned = 0usize;
    let mut edges = 0usize;
    while !plan.is_complete() {
        let report = reindexer.run(&boundary, &store, &mut plan, BATCH)?;
        reindexed += report.reindexed;
        pruned += report.pruned;
        edges += report.edges_written;
        println!(
            "  ... {reindexed} indexed, {pruned} pruned, {} remaining",
            plan.remaining()
        );
    }

    println!("Done: {reindexed} reindexed, {pruned} pruned, {edges} edges written.");
    Ok(())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            collect_files(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}
