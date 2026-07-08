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

/// File extensions admitted as candidates: the supported source languages plus
/// Markdown (for doc-mention resolution). Everything else — binaries, images,
/// lockfiles — is skipped, because the engine reads every admitted file as
/// UTF-8 text and a binary would abort the reindex.
const SOURCE_EXTS: &[&str] = &[
    "rs", "py", "go", "js", "mjs", "cjs", "jsx", "ts", "tsx", "cs", "java", "c", "h", "cpp", "cxx",
    "cc", "hpp", "hh", "rb", "php", "lua", "ml", "mli", "ex", "exs", "ps1", "psm1", "md",
];

fn is_source(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| SOURCE_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

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
            let path = entry.path();
            if is_source(&path) {
                out.push(path);
            }
        }
    }
    Ok(())
}
