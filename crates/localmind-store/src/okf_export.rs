//! Export accepted memory as an OKF (Open Knowledge Format) bundle directory.
//!
//! This reuses [`MemoryBundleExporter`](crate::MemoryBundleExporter) for the
//! safety-critical part — scope selection and defence-in-depth secret redaction —
//! then writes the redacted entries as a directory of OKF concept documents
//! instead of a signed JSON bundle. Each memory becomes one concept `.md`
//! ([`OkfFormat::to_okf`](crate::OkfFormat)), grouped into a per-`type` directory,
//! with an `index.md` in each directory (and at the root) for progressive
//! disclosure.
//!
//! The `index.md` files are **navigation only** — they carry no `type` field, so
//! a re-import skips them and only the concept files round-trip. This is what
//! keeps the concept bodies byte-lossless across an export→import cycle: the link
//! graph lives in the index files and in each concept's native
//! `supersedes`/`contradicts` front matter, never injected into a concept body.
//! Export is read-only over the store — it never mutates stored memory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{BundleError, BundleScope, MemoryBundleExporter, OkfFormat};
use localmind_core::MemoryEntry;
use serde::Serialize;
use thiserror::Error;

/// The author recorded on the intermediate bundle. An OKF bundle is unsigned, so
/// this is a fixed provenance label, not a key identity.
const OKF_EXPORT_AUTHOR: &str = "localmind-okf";

const MAX_LABEL_LEN: usize = 80;

/// What an OKF export wrote.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OkfExportReport {
    /// Concept documents written.
    pub total: usize,
    /// Apparent secrets redacted from entry bodies/evidence before writing.
    pub redactions: usize,
    /// Per-`type` directories created.
    pub categories: usize,
}

/// Exports a project's accepted memory as an OKF bundle directory.
pub struct OkfExporter {
    project_root: PathBuf,
}

impl OkfExporter {
    /// Open the exporter for a project root (must hold an opted-in `.localmind.toml`).
    #[must_use]
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        Self {
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    /// Write the selected scope of accepted memory as an OKF bundle under
    /// `out_dir`. Reuses the signed-bundle exporter's selection + redaction, then
    /// renders each redacted entry as an OKF concept.
    ///
    /// # Errors
    /// [`OkfExportError`] if the store cannot be opened, selection fails, or a
    /// file cannot be written.
    pub fn export(
        &self,
        out_dir: &Path,
        scope: BundleScope,
    ) -> Result<OkfExportReport, OkfExportError> {
        // Reuse selection + defence-in-depth redaction; discard the signature.
        let exporter = MemoryBundleExporter::open_project(&self.project_root)?;
        let outcome = exporter.export(scope, OKF_EXPORT_AUTHOR)?;
        let entries = &outcome.bundle.entries;

        std::fs::create_dir_all(out_dir)?;

        // Group concepts into per-type directories for progressive disclosure.
        let mut by_dir: BTreeMap<String, Vec<&MemoryEntry>> = BTreeMap::new();
        for entry in entries {
            by_dir.entry(type_dir(entry)).or_default().push(entry);
        }

        for (dir_name, dir_entries) in &by_dir {
            let dir = out_dir.join(dir_name);
            std::fs::create_dir_all(&dir)?;
            let mut links = Vec::new();
            for entry in dir_entries {
                let file_name = format!("{}.md", slug(entry.id.as_str()));
                std::fs::write(dir.join(&file_name), OkfFormat::to_okf(entry))?;
                links.push((file_name, label(entry)));
            }
            // Category index links only to files just written — never dangling.
            write_index(&dir.join("index.md"), dir_name, &links)?;
        }

        // Root index links to each category index.
        let category_links: Vec<(String, String)> = by_dir
            .keys()
            .map(|dir_name| (format!("{dir_name}/index.md"), dir_name.clone()))
            .collect();
        write_index(&out_dir.join("index.md"), "OKF bundle", &category_links)?;

        Ok(OkfExportReport {
            total: entries.len(),
            redactions: outcome.scan.redactions,
            categories: by_dir.len(),
        })
    }
}

/// The per-type directory name for an entry.
fn type_dir(entry: &MemoryEntry) -> String {
    let name = slug(&crate::okf::okf_type(&entry.category));
    if name.is_empty() {
        "concepts".to_string()
    } else {
        name
    }
}

/// A short human label for an index link: the entry's first non-empty body line.
fn label(entry: &MemoryEntry) -> String {
    let first = entry
        .body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let text = first.strip_prefix("# ").unwrap_or(first).trim();
    let text = if text.is_empty() {
        entry.id.as_str()
    } else {
        text
    };
    if text.chars().count() <= MAX_LABEL_LEN {
        text.to_string()
    } else {
        text.chars().take(MAX_LABEL_LEN).collect()
    }
}

/// Write a navigation `index.md`: a heading plus a relative link per child. It
/// carries **no** `type` field, so a re-import skips it (it is not a concept).
fn write_index(path: &Path, title: &str, links: &[(String, String)]) -> Result<(), OkfExportError> {
    let mut out = format!("# {title}\n\n");
    for (target, text) in links {
        out.push_str(&format!("- [{text}]({target})\n"));
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// A filesystem/URL-safe slug: lowercased, non-alphanumerics collapsed to single
/// hyphens, trimmed. Empty input yields an empty string (the caller substitutes a
/// default).
fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_dash = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Errors exporting an OKF bundle.
#[derive(Debug, Error)]
pub enum OkfExportError {
    /// The store could not be opened or the selection failed.
    #[error(transparent)]
    Bundle(#[from] BundleError),
    /// A bundle file or directory could not be written.
    #[error("failed to write OKF bundle: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{MemoryPersistence, OkfImporter, ReviewQueue};
    use localmind_core::{
        CandidateLesson, Confidence, LessonCategory, LessonId, ReviewAction, ReviewDecision,
        ReviewItemId, SessionId, SuggestedAction,
    };

    fn project(dir: &tempfile::TempDir) -> PathBuf {
        let root = dir.path().to_path_buf();
        let global = root.join("global-store").join("memory");
        std::fs::write(
            root.join(".localmind.toml"),
            format!(
                "[learning]\nenabled = true\nglobal_memory_root = {:?}\n",
                global.to_string_lossy()
            ),
        )
        .unwrap();
        root
    }

    fn candidate(id: &str, category: LessonCategory, body: &str) -> CandidateLesson {
        CandidateLesson::new(
            LessonId::new(id),
            body.to_string(),
            category,
            Confidence::new(0.8).unwrap(),
            SuggestedAction::PromoteToMemory,
        )
    }

    /// Seed accepted memory through the real review → promote path.
    fn seed(root: &Path, entries: &[(&str, LessonCategory, &str)]) {
        let queue = ReviewQueue::open_project(root).unwrap();
        let candidates: Vec<_> = entries
            .iter()
            .map(|(id, category, body)| candidate(id, category.clone(), body))
            .collect();
        queue
            .enqueue_candidates(&SessionId::new("seed"), &candidates)
            .unwrap();
        let persistence = MemoryPersistence::open_project(root).unwrap();
        for (id, _, _) in entries {
            queue
                .decide(ReviewDecision {
                    item_id: ReviewItemId::new(*id),
                    action: ReviewAction::Accept,
                    reviewer: "tester".to_string(),
                    decided_at: None,
                    note: None,
                    replacement_summary: None,
                    evidence: Vec::new(),
                })
                .unwrap();
            persistence
                .promote_review_item(&ReviewItemId::new(*id))
                .unwrap();
        }
    }

    #[test]
    fn export_writes_a_bundle_whose_concepts_reimport_and_whose_indexes_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        seed(
            &root,
            &[
                (
                    "mem-a",
                    LessonCategory::ProjectConvention,
                    "prefer ripgrep over grep when searching this repo",
                ),
                (
                    "mem-b",
                    LessonCategory::DebuggingRecipe,
                    "bisect the failing commit range to localise a regression",
                ),
            ],
        );

        let out = root.join("okf-out");
        let report = OkfExporter::new(&root)
            .export(&out, BundleScope::Both)
            .unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.categories, 2);
        assert!(out.join("index.md").exists(), "root index written");

        // Re-import: the two concepts parse; the index.md files (no `type`) are
        // skipped — proving the round-trip carries concepts, not navigation.
        let reimport = OkfImporter::new(&root).import(&out, false).unwrap();
        assert_eq!(reimport.total, 2, "two concepts re-import");
        assert!(reimport.skipped >= 1, "index.md files are skipped");
    }

    #[test]
    fn export_over_empty_memory_writes_an_empty_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let out = root.join("okf-out");
        let report = OkfExporter::new(&root)
            .export(&out, BundleScope::Both)
            .unwrap();
        assert_eq!(report.total, 0);
        assert!(out.join("index.md").exists());
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(slug("BigQuery Table"), "bigquery-table");
        assert_eq!(slug("mem-a"), "mem-a");
        assert_eq!(slug("Other(\"x: y\")"), "other-x-y");
    }
}
