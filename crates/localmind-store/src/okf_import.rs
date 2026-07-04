//! Import an OKF (Open Knowledge Format) bundle directory as review candidates.
//!
//! This mirrors [`BundleImporter`](crate::BundleImporter) for the OKF path, with
//! one deliberate difference: an OKF bundle carries **no signature**, so it is
//! always treated as [`ImportTrust::Untrusted`] and every concept is flagged for
//! careful review. The safety contract is otherwise identical and reused
//! wholesale:
//!
//! - every concept is enqueued as a *review candidate* through the existing
//!   [`ReviewQueue`], **never** written straight to active memory;
//! - the existing dedup ladder makes a re-import idempotent;
//! - the write-time quality gate (D-LM-0024) and the semantic/lexical dedup run
//!   at the *accept* seam, not here, so they are inherited for free;
//! - an accepted concept is embedded into the memory `vector_index` at promotion
//!   via the existing path; embedding at import time is deliberately avoided
//!   because it would bypass the review gate.
//!
//! A `--dry-run` (the CLI default) reports what an apply *would* enqueue without
//! writing. Files that are not conformant OKF concepts (no `type` field, missing
//! front matter) are skipped and counted, never fatal.

use std::path::{Path, PathBuf};

use crate::{dedup, ImportTrust, OkfFormat, ReviewQueue, ReviewQueueError, StoreConfigError};
use localmind_core::{
    CandidateDestination, CandidateLesson, EvidenceKind, EvidenceRef, LessonId, MemoryScope,
    ReviewState, SessionId, SuggestedAction,
};
use serde::Serialize;
use thiserror::Error;

/// What an OKF import did (or, on a dry run, would do).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OkfImportReport {
    /// Always [`ImportTrust::Untrusted`]: an OKF bundle is unsigned.
    pub trust: ImportTrust,
    /// Conformant concepts parsed from the bundle directory.
    pub total: usize,
    /// Concepts newly enqueued as review candidates (or that would be).
    pub added: usize,
    /// Concepts collapsed into an existing pending candidate by dedup.
    pub duplicate: usize,
    /// Files skipped because they are not conformant OKF concepts.
    pub skipped: usize,
    /// Whether changes were written (`false` for a dry run).
    pub applied: bool,
}

/// Imports an OKF bundle directory into a project's review-gated store.
pub struct OkfImporter {
    project_root: PathBuf,
}

impl OkfImporter {
    /// Open the importer for a project root (must hold an opted-in `.localmind.toml`).
    #[must_use]
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        Self {
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    /// Read an OKF bundle directory and enqueue its concepts as review
    /// candidates. With `apply = false` (dry run) nothing is written and the
    /// report predicts what an apply would do.
    ///
    /// # Errors
    /// [`OkfImportError`] if the bundle directory cannot be read or the review
    /// queue cannot be opened or written. A non-conformant file is skipped, not
    /// an error.
    pub fn import(
        &self,
        bundle_dir: &Path,
        apply: bool,
    ) -> Result<OkfImportReport, OkfImportError> {
        let mut files = Vec::new();
        collect_markdown(bundle_dir, &mut files)?;
        // Deterministic order so re-imports and reports are stable.
        files.sort();

        let mut skipped = 0;
        let mut candidates = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path)?;
            match OkfFormat::from_okf(&text) {
                Ok(entry) => candidates.push(to_candidate(&entry, bundle_dir)),
                // A file that is not a conformant OKF concept (no `type`, or no
                // front matter) is skipped, never fatal.
                Err(_) => skipped += 1,
            }
        }
        let total = candidates.len();

        if !apply {
            let (added, duplicate) = self.predict(&candidates)?;
            return Ok(OkfImportReport {
                trust: ImportTrust::Untrusted,
                total,
                added,
                duplicate,
                skipped,
                applied: false,
            });
        }

        let queue = ReviewQueue::open_project(&self.project_root)?;
        let session = SessionId::new("import-okf");
        let inserted = queue.enqueue_candidates(&session, &candidates)?;
        Ok(OkfImportReport {
            trust: ImportTrust::Untrusted,
            total,
            added: inserted,
            duplicate: total - inserted,
            skipped,
            applied: true,
        })
    }

    /// Predict (added, duplicate) for a dry run by replaying the enqueue dedup
    /// ladder against the current pending review items, without writing. Mirrors
    /// [`BundleImporter`](crate::BundleImporter)'s dry-run prediction.
    fn predict(&self, candidates: &[CandidateLesson]) -> Result<(usize, usize), OkfImportError> {
        let queue = ReviewQueue::open_project(&self.project_root)?;
        let mut seen: Vec<(String, String)> = queue
            .list()?
            .into_iter()
            .filter(|item| item.state == ReviewState::Pending)
            .map(|item| {
                let summary = item.candidate.summary().to_string();
                (dedup::canonical_hash(&summary), summary)
            })
            .collect();

        let mut added = 0;
        let mut duplicate = 0;
        for candidate in candidates {
            let summary = candidate.summary();
            let hash = dedup::canonical_hash(summary);
            let is_dup = seen.iter().any(|(seen_hash, seen_summary)| {
                seen_hash == &hash || dedup::is_near_duplicate(seen_summary, summary)
            });
            if is_dup {
                duplicate += 1;
            } else {
                added += 1;
                seen.push((hash, summary.to_string()));
            }
        }
        Ok((added, duplicate))
    }
}

/// Map a parsed OKF concept to an untrusted review candidate, routed by scope and
/// carrying unsigned-import provenance.
fn to_candidate(entry: &localmind_core::MemoryEntry, bundle_dir: &Path) -> CandidateLesson {
    let mut candidate = CandidateLesson::new(
        LessonId::new(entry.id.as_str()),
        entry.body.clone(),
        entry.category.clone(),
        entry.confidence,
        SuggestedAction::PromoteToMemory,
    );
    for evidence in &entry.evidence {
        candidate = candidate.with_evidence(evidence.clone());
    }
    let source = bundle_dir.display();
    let note = format!("imported from OKF bundle {source} (unsigned, untrusted)");
    candidate =
        candidate.with_evidence(EvidenceRef::new(EvidenceKind::ManualNote, note).redacted());
    candidate.related_files = entry.related_files.clone();
    candidate.related_entities = entry.related_entities.clone();
    candidate.suggested_destination = if entry.scope == MemoryScope::GlobalUser {
        CandidateDestination::GlobalMemory
    } else {
        CandidateDestination::ProjectMemory
    };
    candidate.rationale =
        Some("imported from an UNSIGNED OKF bundle: review carefully before promotion".to_string());
    candidate
}

/// Recursively collect `*.md` files under a directory.
fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), OkfImportError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Errors importing an OKF bundle (a non-conformant *file* is skipped, not an
/// error — see [`OkfImporter::import`]).
#[derive(Debug, Error)]
pub enum OkfImportError {
    /// The bundle directory or a concept file could not be read.
    #[error("failed to read OKF bundle: {0}")]
    Io(#[from] std::io::Error),
    /// The project config is missing or invalid.
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    /// The review queue could not be opened or written.
    #[error(transparent)]
    Review(#[from] ReviewQueueError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{MemoryPersistence, OkfFormat};
    use localmind_core::{
        Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryScope, MemoryStatus,
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

    /// A conformant foreign OKF concept, using inline-flow tags and a quoted
    /// scalar the canonical block reader would not accept.
    fn foreign_concept(title: &str, body: &str) -> String {
        format!("---\ntype: BigQuery Table\ntitle: \"{title}\"\ntags: [sales, revenue]\n---\n\n{body}\n")
    }

    fn write_bundle(dir: &Path) {
        std::fs::create_dir_all(dir.join("tables")).unwrap();
        std::fs::write(
            dir.join("tables").join("orders.md"),
            foreign_concept(
                "Orders",
                "Completed purchases with revenue, tax, and shipping columns.",
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("tables").join("customers.md"),
            foreign_concept(
                "Customers",
                "Account holders keyed by loyalty tier and signup region.",
            ),
        )
        .unwrap();
        // A non-conformant file: no front matter → skipped, never fatal.
        std::fs::write(dir.join("README.md"), "Just prose, not an OKF concept.\n").unwrap();
    }

    #[test]
    fn import_enqueues_untrusted_review_candidates_and_never_auto_promotes() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let bundle = root.join("okf-bundle");
        write_bundle(&bundle);

        let report = OkfImporter::new(&root).import(&bundle, true).unwrap();
        assert_eq!(report.trust, ImportTrust::Untrusted);
        assert_eq!(report.total, 2, "two conformant concepts");
        assert_eq!(report.added, 2);
        assert_eq!(report.skipped, 1, "the README is skipped");

        // Every concept is a pending review candidate flagged UNSIGNED — nothing
        // reached active memory.
        let items = ReviewQueue::open_project(&root).unwrap().list().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item
            .candidate
            .rationale
            .as_deref()
            .unwrap()
            .contains("UNSIGNED")));
        let persistence = MemoryPersistence::open_project(&root).unwrap();
        assert!(persistence.list_memory().unwrap().is_empty());
    }

    #[test]
    fn re_import_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let bundle = root.join("okf-bundle");
        write_bundle(&bundle);

        let importer = OkfImporter::new(&root);
        assert_eq!(importer.import(&bundle, true).unwrap().added, 2);
        let second = importer.import(&bundle, true).unwrap();
        assert_eq!(second.added, 0);
        assert_eq!(second.duplicate, 2);
        assert_eq!(
            ReviewQueue::open_project(&root)
                .unwrap()
                .list()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let bundle = root.join("okf-bundle");
        write_bundle(&bundle);

        let report = OkfImporter::new(&root).import(&bundle, false).unwrap();
        assert!(!report.applied);
        assert_eq!(report.added, 2);
        assert!(ReviewQueue::open_project(&root)
            .unwrap()
            .list()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_localmind_origin_okf_file_imports_through_the_native_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let bundle = root.join("okf-bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        // A LocalMind-origin OKF file (native keys present) exported by to_okf.
        let entry = MemoryEntry {
            id: MemoryEntryId::new("mem-1"),
            scope: MemoryScope::Project,
            body: "Prefer ripgrep over grep when searching.".to_string(),
            category: LessonCategory::ProjectConvention,
            confidence: Confidence::new(0.8).unwrap(),
            source_session: None,
            evidence: Vec::new(),
            tags: vec!["search".to_string()],
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        };
        std::fs::write(bundle.join("convention.md"), OkfFormat::to_okf(&entry)).unwrap();

        let report = OkfImporter::new(&root).import(&bundle, true).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.added, 1);
        assert_eq!(report.skipped, 0);
    }
}
