//! `localmind ingest docs`: chunk repository Markdown and embed it into the
//! project's semantic documentation index (`subject_kind = 'doc'`).
//!
//! The host walks the tree and splits each file into heading-scoped chunks;
//! the store embeds each chunk body (best-effort, via the configured endpoint)
//! and keeps its text so a semantic hit can be shown and cited. Re-ingest is
//! idempotent — a chunk id is `<relative-path>#<ordinal>`, so unchanged text is
//! a no-op and edited text re-embeds in place.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use localmind_store::MemoryPersistence;

/// Directory names never descended into during the walk. Dotdirs are also
/// skipped.
const SKIP_DIRS: &[&str] = &["target", "node_modules", "dist", "build", ".venv", "venv"];

/// Soft cap on a chunk body's length (characters). A heading section longer than
/// this is split on paragraph boundaries so each embedded passage stays focused.
const MAX_CHUNK_CHARS: usize = 1600;

pub fn docs(root: PathBuf, project: PathBuf) -> Result<()> {
    let persistence = MemoryPersistence::open_project(&project)
        .with_context(|| format!("opening memory store for project {}", project.display()))?;

    let mut files = Vec::new();
    collect_markdown(&root, &mut files)?;
    files.sort();
    println!("Markdown files: {} under {}", files.len(), root.display());

    let mut chunk_total = 0usize;
    let mut embedded = 0usize;
    for file in &files {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("reading {}", file.display()))?;
        let rel = relative(&root, file);
        for (ordinal, chunk) in chunk_markdown(&text).into_iter().enumerate() {
            let chunk_id = format!("{rel}#{ordinal}");
            let ord = i64::try_from(ordinal).unwrap_or(i64::MAX);
            let wrote = persistence.ingest_doc_chunk(
                &chunk_id,
                &rel,
                ord,
                chunk.heading.as_deref(),
                &chunk.body,
            )?;
            chunk_total += 1;
            if wrote {
                embedded += 1;
            }
        }
    }

    println!(
        "Ingested {chunk_total} chunks ({embedded} embedded) from {} files. Total chunks in index: {}.",
        files.len(),
        persistence.doc_chunk_count()?
    );
    if chunk_total > 0 && embedded == 0 {
        println!(
            "note: no embeddings written — configure [inference] embedding_base_url + embedding_model \
             in .localmind.toml and start `localbox embed-serve`, then re-run."
        );
    }
    Ok(())
}

/// One heading-scoped documentation chunk.
struct Chunk {
    heading: Option<String>,
    body: String,
}

/// Split Markdown into chunks at ATX headings (`# ...`), further splitting any
/// section whose body exceeds `MAX_CHUNK_CHARS` on paragraph boundaries. A
/// heading with no body contributes no chunk (nothing to embed).
fn chunk_markdown(text: &str) -> Vec<Chunk> {
    let mut sections: Vec<(Option<String>, String)> = Vec::new();
    let mut heading: Option<String> = None;
    let mut body = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_heading =
            trimmed.starts_with('#') && trimmed.trim_start_matches('#').starts_with(' ');
        if is_heading {
            if !body.trim().is_empty() {
                sections.push((heading.clone(), body.trim().to_string()));
            }
            heading = Some(trimmed.trim_start_matches('#').trim().to_string());
            body = String::new();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if !body.trim().is_empty() {
        sections.push((heading, body.trim().to_string()));
    }

    let mut chunks = Vec::new();
    for (heading, body) in sections {
        if body.chars().count() <= MAX_CHUNK_CHARS {
            chunks.push(Chunk { heading, body });
        } else {
            for piece in split_paragraphs(&body, MAX_CHUNK_CHARS) {
                chunks.push(Chunk {
                    heading: heading.clone(),
                    body: piece,
                });
            }
        }
    }
    chunks
}

/// Split an oversized section into pieces at blank-line (paragraph) boundaries,
/// each at most `max` characters. A single paragraph longer than `max` is kept
/// whole rather than cut mid-sentence.
fn split_paragraphs(body: &str, max: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    for para in body.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if !current.is_empty() && current.chars().count() + para.chars().count() > max {
            pieces.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
        if current.chars().count() >= max {
            pieces.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        pieces.push(current);
    }
    pieces
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
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
            collect_markdown(&entry.path(), out)?;
        } else if file_type.is_file() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            {
                out.push(path);
            }
        }
    }
    Ok(())
}
