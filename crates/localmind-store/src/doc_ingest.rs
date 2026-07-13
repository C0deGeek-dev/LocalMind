//! Chunk repository Markdown and ingest it into a project's semantic
//! documentation index (`doc_chunk`, `subject_kind = 'doc'`).
//!
//! This is the reusable core behind `localmind ingest docs`: the tree is walked,
//! each file is split into heading-scoped chunks, and every chunk body is stored
//! (embedded best-effort via the configured endpoint). Re-ingest is idempotent —
//! a chunk id is `<relative-path>#<ordinal>`, so unchanged text is a no-op and
//! edited text re-embeds in place. Keeping it here (not in the CLI) lets other
//! hosts — e.g. LocalPilot ingesting its own research reports — reuse the exact
//! same chunker and store path rather than re-implementing it.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::memory_persistence::{MemoryPersistence, MemoryPersistenceError};

/// Directory names never descended into during the walk. Dotdirs are also
/// skipped.
const SKIP_DIRS: &[&str] = &["target", "node_modules", "dist", "build", ".venv", "venv"];

/// Soft cap on a chunk body's length (characters). A heading section longer than
/// this is split on paragraph boundaries so each embedded passage stays focused.
const MAX_CHUNK_CHARS: usize = 1600;

/// What a documentation-ingest run touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocIngestSummary {
    /// Markdown files walked and read.
    pub files: usize,
    /// Chunks derived and written (inserted or updated).
    pub chunks: usize,
    /// Chunks whose body was (re-)embedded — a subset of `chunks`; `0` when no
    /// embedding endpoint is configured or reachable.
    pub embedded: usize,
    /// Total chunks in the project's `doc_chunk` index after this run.
    pub total_in_index: i64,
}

/// Failure while ingesting documentation chunks.
#[derive(Debug, Error)]
pub enum DocIngestError {
    #[error("reading directory {path:?}: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("reading file {path:?}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] MemoryPersistenceError),
}

/// Walk `root` for Markdown, chunk it, and ingest every chunk into `project`'s
/// documentation index. Opens the project store itself; use
/// [`ingest_docs_into`] when a caller already holds an open store.
pub fn ingest_docs(root: &Path, project: &Path) -> Result<DocIngestSummary, DocIngestError> {
    let persistence = MemoryPersistence::open_project(project)?;
    ingest_docs_into(root, &persistence)
}

/// Ingest Markdown under `root` into an already-open project store.
pub fn ingest_docs_into(
    root: &Path,
    persistence: &MemoryPersistence,
) -> Result<DocIngestSummary, DocIngestError> {
    let mut files = Vec::new();
    collect_markdown(root, &mut files)?;
    files.sort();

    let mut chunks = 0usize;
    let mut embedded = 0usize;
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|source| DocIngestError::ReadFile {
            path: file.clone(),
            source,
        })?;
        let rel = relative(root, file);
        let (wrote, vectored) = ingest_doc_text(persistence, &rel, &text, true)?;
        chunks += wrote;
        embedded += vectored;
    }

    Ok(DocIngestSummary {
        files: files.len(),
        chunks,
        embedded,
        total_in_index: persistence.doc_chunk_count()?,
    })
}

/// Chunk one Markdown document's text and ingest it under `rel_path`, replacing
/// whatever was stored for that path: chunks are upserted in ordinal order and
/// the stale tail beyond the new chunk count is pruned (a shrunk file does not
/// leave its old passages behind). Text that yields no chunks removes the
/// path's previous chunks entirely. `embed` gates the best-effort vector write,
/// so a host that promises no ingest-time embedding cost can keep that promise.
/// Returns `(chunks written, chunks embedded)`.
///
/// This is the entry for hosts that hold the document text already — e.g. a
/// walker with its own ignore rules and redaction — rather than a tree for
/// [`ingest_docs`] to walk itself.
pub fn ingest_doc_text(
    persistence: &MemoryPersistence,
    rel_path: &str,
    text: &str,
    embed: bool,
) -> Result<(usize, usize), DocIngestError> {
    let mut written = 0usize;
    let mut embedded = 0usize;
    let chunks = chunk_markdown(text);
    for (ordinal, chunk) in chunks.iter().enumerate() {
        let chunk_id = format!("{rel_path}#{ordinal}");
        let ord = i64::try_from(ordinal).unwrap_or(i64::MAX);
        let wrote = persistence.ingest_doc_chunk(
            &chunk_id,
            rel_path,
            ord,
            chunk.heading.as_deref(),
            &chunk.body,
            embed,
        )?;
        written += 1;
        if wrote {
            embedded += 1;
        }
    }
    persistence.prune_doc_chunks_from(rel_path, i64::try_from(chunks.len()).unwrap_or(i64::MAX))?;
    Ok((written, embedded))
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

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), DocIngestError> {
    let entries = std::fs::read_dir(dir).map_err(|source| DocIngestError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DocIngestError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| DocIngestError::ReadDir {
                path: dir.to_path_buf(),
                source,
            })?;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn chunks_split_at_headings_and_skip_empty_sections() {
        let md = "# One\n\nAlpha body.\n\n# Empty\n\n# Two\n\nBeta body.\n";
        let chunks = chunk_markdown(md);
        assert_eq!(
            chunks.len(),
            2,
            "the heading with no body contributes nothing"
        );
        assert_eq!(chunks[0].heading.as_deref(), Some("One"));
        assert_eq!(chunks[0].body, "Alpha body.");
        assert_eq!(chunks[1].heading.as_deref(), Some("Two"));
        assert_eq!(chunks[1].body, "Beta body.");
    }

    #[test]
    fn oversized_section_splits_on_paragraph_boundaries() {
        let para = "x".repeat(MAX_CHUNK_CHARS - 10);
        let md = format!("# Big\n\n{para}\n\n{para}\n");
        let chunks = chunk_markdown(&md);
        assert!(
            chunks.len() >= 2,
            "an oversized section is split into pieces"
        );
        assert!(chunks.iter().all(|c| c.heading.as_deref() == Some("Big")));
    }

    #[test]
    fn relative_uses_forward_slashes() {
        let root = Path::new("/proj/docs");
        let file = Path::new("/proj/docs/sub/page.md");
        assert_eq!(relative(root, file), "sub/page.md");
    }

    /// A project store in a temp dir (empty config = defaults, no embeddings).
    fn open_temp_project(dir: &tempfile::TempDir) -> MemoryPersistence {
        std::fs::write(dir.path().join(".localmind.toml"), "").unwrap();
        MemoryPersistence::open_project(dir.path()).unwrap()
    }

    #[test]
    fn reingesting_shrunk_text_prunes_the_stale_tail() {
        let dir = tempfile::tempdir().unwrap();
        let persistence = open_temp_project(&dir);

        let long = "# One\n\nAlpha.\n\n# Two\n\nBeta.\n\n# Three\n\nGamma.\n";
        let (written, _) = ingest_doc_text(&persistence, "guide.md", long, true).unwrap();
        assert_eq!(written, 3);
        assert_eq!(persistence.doc_chunks_for("guide.md").unwrap().len(), 3);

        let short = "# One\n\nAlpha only now.\n";
        let (written, _) = ingest_doc_text(&persistence, "guide.md", short, true).unwrap();
        assert_eq!(written, 1);
        let chunks = persistence.doc_chunks_for("guide.md").unwrap();
        assert_eq!(chunks.len(), 1, "ordinals beyond the new count are pruned");
        assert_eq!(chunks[0].2, "Alpha only now.");
    }

    #[test]
    fn text_with_no_chunks_removes_the_previous_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let persistence = open_temp_project(&dir);

        ingest_doc_text(&persistence, "gone.md", "# Note\n\nBody.\n", true).unwrap();
        assert_eq!(persistence.doc_chunks_for("gone.md").unwrap().len(), 1);

        let (written, _) =
            ingest_doc_text(&persistence, "gone.md", "# Heading only\n", true).unwrap();
        assert_eq!(written, 0);
        assert!(
            persistence.doc_chunks_for("gone.md").unwrap().is_empty(),
            "a file that no longer yields chunks keeps no stale passages"
        );
    }

    #[test]
    fn deleting_a_doc_file_removes_all_its_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let persistence = open_temp_project(&dir);

        ingest_doc_text(
            &persistence,
            "a.md",
            "# A\n\nAlpha.\n\n# B\n\nBeta.\n",
            true,
        )
        .unwrap();
        ingest_doc_text(&persistence, "b.md", "# C\n\nGamma.\n", true).unwrap();

        let removed = persistence.delete_doc_file("a.md").unwrap();
        assert_eq!(removed, 2);
        assert!(persistence.doc_chunks_for("a.md").unwrap().is_empty());
        assert_eq!(
            persistence.doc_chunks_for("b.md").unwrap().len(),
            1,
            "other files are untouched"
        );
    }
}
