//! `localmind ingest docs`: chunk repository Markdown and embed it into the
//! project's semantic documentation index (`subject_kind = 'doc'`).
//!
//! The chunk+ingest core lives in `localmind_store::ingest_docs` so other hosts
//! can reuse it; this command is the thin CLI wrapper that prints a summary.

use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn docs(root: PathBuf, project: PathBuf) -> Result<()> {
    let summary = localmind_store::ingest_docs(&root, &project).with_context(|| {
        format!(
            "ingesting docs under {} into project {}",
            root.display(),
            project.display()
        )
    })?;

    println!("Markdown files: {} under {}", summary.files, root.display());
    println!(
        "Ingested {} chunks ({} embedded) from {} files. Total chunks in index: {}.",
        summary.chunks, summary.embedded, summary.files, summary.total_in_index
    );
    if summary.chunks > 0 && summary.embedded == 0 {
        println!(
            "note: no embeddings written — configure [inference] embedding_base_url + embedding_model \
             in .localmind.toml and start `localbox embed-serve`, then re-run."
        );
    }
    Ok(())
}
