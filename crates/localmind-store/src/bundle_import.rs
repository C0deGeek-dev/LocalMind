//! Import a verified memory bundle, scope-aware and review-gated.
//!
//! Importing is the safety-critical half of the round-trip. It is gated twice:
//!
//! 1. **Cryptographic verify** (subject "sign and verify"): a `Rejected` bundle
//!    never reaches the store; an `Untrusted` bundle (valid signature, unknown
//!    key) is allowed but flagged for heavier review; a `Trusted` bundle proceeds.
//! 2. **Human review** (D001): every entry is enqueued as a *review candidate* —
//!    never written straight to active memory. The existing dedup ladder makes a
//!    re-import idempotent, and a reviewer's supersede decision retires a stale
//!    target through the existing path.
//!
//! Entries are routed by scope: a project entry → the project store, a global
//! entry → the machine-wide global store (HarnessConvergence 05 / D-LM-0017), via
//! the candidate's `suggested_destination`. Import provenance (origin author,
//! trust class, bundle digest) rides along as evidence + the review session id.
//! A `--dry-run` (the default in the CLI) reports what *would* change without
//! writing anything.

use crate::{
    dedup, verify_signed, KeyStore, ReviewQueue, ReviewQueueError, SignedBundle, SigningError,
    StoreConfigError, TrustClass, VerificationOutcome,
};
use localmind_core::{
    CandidateDestination, CandidateLesson, EvidenceKind, EvidenceRef, LessonId, MemoryScope,
    ReviewState, SessionId, SuggestedAction,
};
use std::path::Path;
use thiserror::Error;

/// The trust class an import settled on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportTrust {
    /// Valid signature by a known key.
    Trusted,
    /// Valid signature by an unknown key (heavier review).
    Untrusted,
    /// Verification failed; nothing was imported. Carries a short reason label.
    Rejected(String),
}

/// What an import did (or, on a dry run, would do).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleImportReport {
    /// The trust class verification settled on.
    pub trust: ImportTrust,
    /// Entries in the bundle.
    pub total: usize,
    /// Entries newly enqueued as review candidates (or that would be).
    pub added: usize,
    /// Entries collapsed into an existing pending candidate by dedup.
    pub duplicate: usize,
    /// Entries not imported because the whole bundle was rejected.
    pub rejected: usize,
    /// Supersessions performed at import. Always 0: supersession is a *reviewer*
    /// decision applied through the existing review path, not an import action.
    pub superseded: usize,
    /// Whether changes were written (`false` for a dry run).
    pub applied: bool,
}

impl BundleImportReport {
    fn rejected_all(total: usize, reason: String) -> Self {
        Self {
            trust: ImportTrust::Rejected(reason),
            total,
            added: 0,
            duplicate: 0,
            rejected: total,
            superseded: 0,
            applied: false,
        }
    }
}

/// Imports verified bundles into a project's review-gated store.
pub struct BundleImporter {
    project_root: std::path::PathBuf,
}

impl BundleImporter {
    /// Open the importer for a project root (must hold an opted-in `.localmind.toml`).
    #[must_use]
    pub fn new(project_root: impl AsRef<Path>) -> Self {
        Self {
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    /// Verify and import a signed bundle. With `apply = false` (dry run) nothing is
    /// written and the report predicts what an apply would do; with `apply = true`
    /// entries are enqueued as review candidates (never promoted to active memory).
    ///
    /// # Errors
    /// [`BundleImportError`] if the key store or review queue cannot be opened or
    /// written. A *rejected* bundle is **not** an error — it returns a report whose
    /// trust is `Rejected` and whose store is untouched.
    pub fn import(
        &self,
        signed: &SignedBundle,
        apply: bool,
    ) -> Result<BundleImportReport, BundleImportError> {
        let total = signed.bundle.entries.len();

        // Gate 1: cryptographic verify. A rejected bundle never reaches the store.
        let keystore = KeyStore::open(&self.project_root)?;
        let trusted = keystore.trusted_keys()?;
        let (trust, _author) = match verify_signed(signed, &trusted) {
            VerificationOutcome::Rejected { reason } => {
                return Ok(BundleImportReport::rejected_all(
                    total,
                    reason.label().to_string(),
                ));
            }
            VerificationOutcome::Verified { class, author } => {
                let trust = match class {
                    TrustClass::Trusted => ImportTrust::Trusted,
                    TrustClass::Untrusted => ImportTrust::Untrusted,
                };
                (trust, author)
            }
        };

        let candidates = self.to_candidates(signed, &trust, &_author);

        if !apply {
            // Dry run: predict added/duplicate against the current pending queue,
            // mirroring the enqueue dedup ladder, without writing.
            let (added, duplicate) = self.predict(&candidates)?;
            return Ok(BundleImportReport {
                trust,
                total,
                added,
                duplicate,
                rejected: 0,
                superseded: 0,
                applied: false,
            });
        }

        // Gate 2: enqueue as review candidates (dedup is authoritative here).
        let queue = ReviewQueue::open_project(&self.project_root)?;
        let session = import_session_id(&_author);
        let inserted = queue.enqueue_candidates(&session, &candidates)?;
        Ok(BundleImportReport {
            trust,
            total,
            added: inserted,
            duplicate: total - inserted,
            rejected: 0,
            superseded: 0,
            applied: true,
        })
    }

    /// Map each bundle entry to a review candidate, routing by scope and carrying
    /// import provenance (origin author + trust + bundle digest) as evidence.
    fn to_candidates(
        &self,
        signed: &SignedBundle,
        trust: &ImportTrust,
        author: &str,
    ) -> Vec<CandidateLesson> {
        let digest = &signed.signature.digest;
        let trust_label = match trust {
            ImportTrust::Trusted => "trusted",
            ImportTrust::Untrusted => "untrusted",
            ImportTrust::Rejected(_) => "rejected",
        };
        signed
            .bundle
            .entries
            .iter()
            .map(|entry| {
                let mut candidate = CandidateLesson::new(
                    LessonId::new(entry.id.as_str()),
                    entry.body.clone(),
                    entry.category.clone(),
                    entry.confidence,
                    SuggestedAction::PromoteToMemory,
                );
                // Carry the entry's own evidence, then an import-provenance note.
                for evidence in &entry.evidence {
                    candidate = candidate.with_evidence(evidence.clone());
                }
                let note = format!(
                    "imported from author {author} (trust: {trust_label}); bundle digest {digest}"
                );
                candidate = candidate
                    .with_evidence(EvidenceRef::new(EvidenceKind::ManualNote, note).redacted());
                candidate.related_files = entry.related_files.clone();
                candidate.related_entities = entry.related_entities.clone();
                // Route by the entry's scope; the existing promote path has final
                // say toward global per the category classifier (D-LM-0017).
                candidate.suggested_destination = if entry.scope == MemoryScope::GlobalUser {
                    CandidateDestination::GlobalMemory
                } else {
                    CandidateDestination::ProjectMemory
                };
                candidate.rationale = Some(match trust {
                    ImportTrust::Untrusted => format!(
                        "imported from an UNTRUSTED author ({author}): review carefully before promotion"
                    ),
                    _ => format!("imported from trusted author {author}"),
                });
                candidate
            })
            .collect()
    }

    /// Predict (added, duplicate) for a dry run by replaying the enqueue dedup
    /// ladder against the current pending review items, without writing.
    fn predict(&self, candidates: &[CandidateLesson]) -> Result<(usize, usize), BundleImportError> {
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

/// A stable, provenance-bearing review session id for an import.
fn import_session_id(author: &str) -> SessionId {
    SessionId::new(format!("import-{author}"))
}

/// Errors importing a bundle (a *rejected* bundle is not an error — see
/// [`BundleImporter::import`]).
#[derive(Debug, Error)]
pub enum BundleImportError {
    /// The project config is missing or invalid.
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    /// The key store could not be opened or read.
    #[error(transparent)]
    Signing(#[from] SigningError),
    /// The review queue could not be opened or written.
    #[error(transparent)]
    Review(#[from] ReviewQueueError),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{
        sign_bundle, BundleMetadata, BundleScope, MemoryBundle, MemoryPersistence,
        MEMORY_BUNDLE_FORMAT_VERSION,
    };
    use ed25519_dalek::SigningKey;
    use localmind_core::{
        Confidence, LessonCategory, MemoryEntry, MemoryEntryId, MemoryStatus, ReviewAction,
        ReviewDecision, ReviewItemId,
    };

    fn project(dir: &tempfile::TempDir) -> std::path::PathBuf {
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

    fn entry(id: &str, scope: MemoryScope, category: LessonCategory, body: &str) -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new(id),
            scope,
            body: body.to_string(),
            category,
            confidence: Confidence::new(0.8).unwrap(),
            source_session: None,
            evidence: Vec::new(),
            tags: Vec::new(),
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        }
    }

    fn bundle(entries: Vec<MemoryEntry>) -> MemoryBundle {
        MemoryBundle {
            format_version: MEMORY_BUNDLE_FORMAT_VERSION,
            metadata: BundleMetadata {
                created_by: "machine-a".to_string(),
                scope_selection: BundleScope::Both,
                entry_count: entries.len(),
                redaction_count: 0,
            },
            entries,
        }
    }

    fn signed(entries: Vec<MemoryEntry>) -> SignedBundle {
        let key = SigningKey::from_bytes(&[11u8; 32]);
        sign_bundle(&bundle(entries), &key).unwrap()
    }

    #[test]
    fn a_rejected_bundle_never_reaches_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let mut tampered = signed(vec![entry(
            "mem-1",
            MemoryScope::Project,
            LessonCategory::ProjectConvention,
            "a poisoned lesson",
        )]);
        // Tamper after signing → digest mismatch → Rejected.
        tampered.bundle.entries[0].body = "an injected malicious lesson".to_string();

        let report = BundleImporter::new(&root).import(&tampered, true).unwrap();
        assert!(matches!(report.trust, ImportTrust::Rejected(_)));
        assert_eq!(report.added, 0);
        assert_eq!(report.rejected, 1);
        // Nothing was enqueued.
        let queue = ReviewQueue::open_project(&root).unwrap();
        assert!(queue.list().unwrap().is_empty());
    }

    #[test]
    fn an_untrusted_bundle_imports_flagged_for_review() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let pack = signed(vec![entry(
            "mem-x",
            MemoryScope::Project,
            LessonCategory::ProjectConvention,
            "a lesson from a stranger",
        )]);
        // No trust list → unknown key → Untrusted, but still imported (review-gated).
        let report = BundleImporter::new(&root).import(&pack, true).unwrap();
        assert_eq!(report.trust, ImportTrust::Untrusted);
        assert_eq!(report.added, 1);
        let items = ReviewQueue::open_project(&root).unwrap().list().unwrap();
        assert_eq!(items.len(), 1);
        // The untrusted provenance is visible to the reviewer.
        assert!(items[0]
            .candidate
            .rationale
            .as_deref()
            .unwrap()
            .contains("UNTRUSTED"));
        // Nothing was auto-promoted to active memory.
        let persistence = MemoryPersistence::open_project(&root).unwrap();
        assert!(persistence.list_memory().unwrap().is_empty());
    }

    #[test]
    fn scope_aware_routing_sends_global_to_global_and_project_to_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let pack = signed(vec![
            entry(
                "mem-global",
                MemoryScope::GlobalUser,
                LessonCategory::DebuggingRecipe,
                "a cross-project debugging recipe",
            ),
            entry(
                "mem-project",
                MemoryScope::Project,
                LessonCategory::ProjectConvention,
                "this repo formats with two-space indent",
            ),
        ]);
        let importer = BundleImporter::new(&root);
        assert_eq!(importer.import(&pack, true).unwrap().added, 2);

        // Accept + promote both, then check each landed in the right store.
        let queue = ReviewQueue::open_project(&root).unwrap();
        let persistence = MemoryPersistence::open_project(&root).unwrap();
        for id in ["mem-global", "mem-project"] {
            queue
                .decide(ReviewDecision {
                    item_id: ReviewItemId::new(id),
                    action: ReviewAction::Accept,
                    reviewer: "tester".to_string(),
                    decided_at: None,
                    note: None,
                    replacement_summary: None,
                    evidence: Vec::new(),
                })
                .unwrap();
            persistence
                .promote_review_item(&ReviewItemId::new(id))
                .unwrap();
        }
        let scopes: std::collections::BTreeMap<String, String> = persistence
            .list_memory()
            .unwrap()
            .into_iter()
            .map(|record| (record.memory_id.to_string(), record.scope))
            .collect();
        assert_eq!(
            scopes.get("mem-global").map(String::as_str),
            Some("GlobalUser")
        );
        assert_eq!(
            scopes.get("mem-project").map(String::as_str),
            Some("Project")
        );
    }

    #[test]
    fn re_import_is_idempotent_and_nothing_auto_promotes() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let pack = signed(vec![
            entry(
                "mem-a",
                MemoryScope::Project,
                LessonCategory::ProjectConvention,
                "prefer ripgrep over grep when searching",
            ),
            entry(
                "mem-b",
                MemoryScope::Project,
                LessonCategory::ProjectConvention,
                "run the integration suite after an exporter change",
            ),
        ]);
        let importer = BundleImporter::new(&root);
        let first = importer.import(&pack, true).unwrap();
        assert_eq!(first.added, 2);
        assert_eq!(first.duplicate, 0);

        // A second import of the same bundle is a no-op (dedup collapses both).
        let second = importer.import(&pack, true).unwrap();
        assert_eq!(second.added, 0);
        assert_eq!(second.duplicate, 2);

        // Still nothing in active memory — import never promotes.
        let persistence = MemoryPersistence::open_project(&root).unwrap();
        assert!(persistence.list_memory().unwrap().is_empty());
        // Exactly two review candidates, not four.
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
    fn dry_run_writes_nothing_but_reports_counts() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let pack = signed(vec![
            entry(
                "mem-1",
                MemoryScope::Project,
                LessonCategory::ProjectConvention,
                "a first lesson to preview",
            ),
            entry(
                "mem-2",
                MemoryScope::GlobalUser,
                LessonCategory::DebuggingRecipe,
                "a second lesson to preview",
            ),
        ]);
        let report = BundleImporter::new(&root).import(&pack, false).unwrap();
        assert!(!report.applied);
        assert_eq!(report.added, 2);
        assert_eq!(report.duplicate, 0);
        // The dry run wrote nothing.
        assert!(ReviewQueue::open_project(&root)
            .unwrap()
            .list()
            .unwrap()
            .is_empty());

        // A real apply then matches the dry-run prediction.
        let applied = BundleImporter::new(&root).import(&pack, true).unwrap();
        assert!(applied.applied);
        assert_eq!(applied.added, 2);
    }
}
