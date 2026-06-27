//! Portable, versioned, self-describing memory bundle.
//!
//! A bundle is a self-contained pack of *accepted* memory entries that can be
//! moved to another machine (or shared with another person) and re-imported.
//! Unlike [`crate::ContextExporter`] (which renders memory as prose for a prompt,
//! one-way), a bundle is a faithful, re-importable serialization: it carries each
//! entry's body, scope, category, confidence, tags, related files/entities,
//! evidence, and supersede/contradict edges, plus a format version so an older
//! reader can reject a newer pack with a reason.
//!
//! The bundle reuses the canonical model serde (`MemoryEntry`) — there is no
//! second serialization of a lesson — and recovers full entries from the
//! Markdown source of truth via [`MarkdownMemoryFormat::parse`]. Bodies and
//! evidence labels are re-redacted on export (defense in depth on top of the
//! redaction already applied at capture time); the returned [`SecretScanReport`]
//! is the seam a caller uses to require an explicit confirm before sharing.
//!
//! Signing/verification layers on top in `signing` (subject 02); this module is
//! signature-agnostic and produces the canonical bytes that get signed.

use crate::{
    MarkdownMemoryFormat, MarkdownParseError, MemoryPersistence, MemoryPersistenceError,
    ProjectConfig, Redactor, StoreConfigError,
};
use localmind_core::{MemoryEntry, MemoryScope};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The bundle schema version. Bump on any breaking change to the on-disk shape;
/// a reader rejects a bundle whose `format_version` it does not understand.
pub const MEMORY_BUNDLE_FORMAT_VERSION: u32 = 1;

/// Which accepted-memory scopes an export includes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleScope {
    /// Project-scoped memory only.
    Project,
    /// Machine-wide global memory only.
    Global,
    /// Both project and global memory.
    Both,
}

impl BundleScope {
    /// Whether a memory of `scope` is included by this selection. Session, skill,
    /// and research scopes are never exported (only durable project/global memory).
    #[must_use]
    pub fn includes(self, scope: &MemoryScope) -> bool {
        match self {
            BundleScope::Project => matches!(scope, MemoryScope::Project),
            BundleScope::Global => matches!(scope, MemoryScope::GlobalUser),
            BundleScope::Both => {
                matches!(scope, MemoryScope::Project | MemoryScope::GlobalUser)
            }
        }
    }
}

/// Self-describing header for a bundle.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct BundleMetadata {
    /// Author identity that produced the pack (an id, not a credential). Filled by
    /// the signing layer / CLI; `"anonymous"` when unattributed.
    pub created_by: String,
    /// Which scopes were selected at export time.
    pub scope_selection: BundleScope,
    /// Number of entries in the bundle (== `entries.len()`).
    pub entry_count: usize,
    /// Total redaction hits applied across entry bodies and evidence on export.
    pub redaction_count: usize,
}

/// A portable, versioned pack of accepted memory entries.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct MemoryBundle {
    /// Schema version; see [`MEMORY_BUNDLE_FORMAT_VERSION`].
    pub format_version: u32,
    /// Self-describing header.
    pub metadata: BundleMetadata,
    /// The accepted memory entries, redacted, ordered by id.
    pub entries: Vec<MemoryEntry>,
}

impl MemoryBundle {
    /// Deterministic, content-addressable bytes for hashing and signing: entries
    /// sorted by id, compact JSON. Identical content yields identical bytes across
    /// runs and machines, so the digest/signature in subject 02 are stable.
    ///
    /// # Errors
    /// [`BundleError::Serialize`] if serialization fails.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, BundleError> {
        let mut sorted = self.clone();
        sorted
            .entries
            .sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        serde_json::to_vec(&sorted).map_err(BundleError::Serialize)
    }

    /// Pretty JSON for writing a (still unsigned) bundle to a file.
    ///
    /// # Errors
    /// [`BundleError::Serialize`] if serialization fails.
    pub fn to_pretty_json(&self) -> Result<String, BundleError> {
        serde_json::to_string_pretty(self).map_err(BundleError::Serialize)
    }

    /// Parse a bundle from JSON, rejecting an unknown (newer) format version with
    /// a reason rather than mis-reading it.
    ///
    /// # Errors
    /// [`BundleError::Deserialize`] on malformed JSON, or
    /// [`BundleError::UnsupportedVersion`] when `format_version` is newer than
    /// this build understands.
    pub fn from_json(text: &str) -> Result<Self, BundleError> {
        let bundle: MemoryBundle = serde_json::from_str(text).map_err(BundleError::Deserialize)?;
        if bundle.format_version > MEMORY_BUNDLE_FORMAT_VERSION {
            return Err(BundleError::UnsupportedVersion {
                found: bundle.format_version,
                supported: MEMORY_BUNDLE_FORMAT_VERSION,
            });
        }
        Ok(bundle)
    }
}

/// What a pre-export secret scan found, so a caller can require explicit confirm
/// before sharing a pack.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SecretScanReport {
    /// Total redaction hits across all exported entry bodies and evidence.
    pub redactions: usize,
    /// Number of entries that had at least one redaction applied.
    pub entries_with_redactions: usize,
}

impl SecretScanReport {
    /// Whether the scan redacted anything — the signal a caller checks before
    /// asking the user to confirm an export that touched apparent secrets.
    #[must_use]
    pub fn found_secrets(&self) -> bool {
        self.redactions > 0
    }
}

/// The result of an export: the bundle plus its pre-export secret-scan report.
#[derive(Clone, Debug)]
pub struct ExportOutcome {
    /// The produced (unsigned) bundle.
    pub bundle: MemoryBundle,
    /// The pre-export secret scan.
    pub scan: SecretScanReport,
}

/// Exports accepted memory from a project's store into a portable bundle.
pub struct MemoryBundleExporter {
    persistence: MemoryPersistence,
    excluded_paths: Vec<String>,
}

impl MemoryBundleExporter {
    /// Open the exporter against an opted-in project. Opens the project store
    /// (and the machine-wide global store when the project allows it), so global
    /// memory can be exported too.
    ///
    /// # Errors
    /// [`BundleError`] if the project config is missing/invalid or the store
    /// cannot be opened.
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self, BundleError> {
        let config = ProjectConfig::discover(project_root).map_err(BundleError::Config)?;
        let excluded_paths = config.config.learning.excluded_paths.clone();
        let persistence = MemoryPersistence::open_project(&config.project_root)?;
        Ok(Self {
            persistence,
            excluded_paths,
        })
    }

    /// Export the selected scope of accepted memory, re-redacting each entry, into
    /// a deterministic bundle. Only `active` (accepted) memory is included.
    ///
    /// # Errors
    /// [`BundleError`] if the store cannot be read, a memory file cannot be read
    /// or parsed, or serialization fails.
    pub fn export(
        &self,
        scope: BundleScope,
        created_by: &str,
    ) -> Result<ExportOutcome, BundleError> {
        let redactor = Redactor::new(self.excluded_paths.clone());
        let records = self.persistence.list_memory()?;
        let mut entries = Vec::new();
        let mut scan = SecretScanReport::default();

        for record in records {
            let Some(entry_scope) = scope_from_label(&record.scope) else {
                // A scope this build doesn't model is never exported.
                continue;
            };
            if !scope.includes(&entry_scope) {
                continue;
            }
            let text =
                fs::read_to_string(&record.path).map_err(|source| BundleError::ReadEntry {
                    path: record.path.clone(),
                    source,
                })?;
            let mut entry =
                MarkdownMemoryFormat::parse(&text).map_err(|source| BundleError::ParseEntry {
                    path: record.path.clone(),
                    source,
                })?;
            let mut entry_redactions = 0;
            redact_in_place(&redactor, &mut entry.body, &mut entry_redactions);
            for evidence in &mut entry.evidence {
                redact_in_place(&redactor, &mut evidence.label, &mut entry_redactions);
                if let Some(uri) = &mut evidence.uri {
                    redact_in_place(&redactor, uri, &mut entry_redactions);
                }
            }
            if entry_redactions > 0 {
                scan.entries_with_redactions += 1;
                scan.redactions += entry_redactions;
            }
            entries.push(entry);
        }

        entries.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        let metadata = BundleMetadata {
            created_by: created_by.to_string(),
            scope_selection: scope,
            entry_count: entries.len(),
            redaction_count: scan.redactions,
        };
        Ok(ExportOutcome {
            bundle: MemoryBundle {
                format_version: MEMORY_BUNDLE_FORMAT_VERSION,
                metadata,
                entries,
            },
            scan,
        })
    }
}

/// Redact `text` in place, accumulating the number of replacements applied.
fn redact_in_place(redactor: &Redactor, text: &mut String, total: &mut usize) {
    let report = redactor.redact(text);
    let hits: usize = report.redactions.iter().map(|r| r.replacements).sum();
    if hits > 0 {
        *total += hits;
        *text = report.redacted_text;
    }
}

/// Map a memory-index scope label (`{:?}` of [`MemoryScope`]) back to the enum.
fn scope_from_label(label: &str) -> Option<MemoryScope> {
    match label {
        "GlobalUser" => Some(MemoryScope::GlobalUser),
        "Project" => Some(MemoryScope::Project),
        "Session" => Some(MemoryScope::Session),
        "Skill" => Some(MemoryScope::Skill),
        "Research" => Some(MemoryScope::Research),
        _ => None,
    }
}

/// Errors producing or reading a memory bundle.
#[derive(Debug, Error)]
pub enum BundleError {
    /// The project config is missing or invalid.
    #[error(transparent)]
    Config(#[from] StoreConfigError),
    /// The accepted-memory store could not be opened or read.
    #[error(transparent)]
    Persistence(#[from] MemoryPersistenceError),
    /// A memory's Markdown file could not be read.
    #[error("failed to read memory file {path:?}: {source}")]
    ReadEntry {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A memory's Markdown file could not be parsed back into an entry.
    #[error("failed to parse memory file {path:?}: {source}")]
    ParseEntry {
        path: PathBuf,
        source: MarkdownParseError,
    },
    /// The bundle JSON could not be produced.
    #[error("failed to serialize bundle: {0}")]
    Serialize(serde_json::Error),
    /// The bundle JSON could not be parsed.
    #[error("failed to deserialize bundle: {0}")]
    Deserialize(serde_json::Error),
    /// The bundle's format version is newer than this build understands.
    #[error("bundle format version {found} is newer than this build supports ({supported}); update LocalMind")]
    UnsupportedVersion { found: u32, supported: u32 },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::MemoryPersistence;
    use localmind_core::{
        Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry, MemoryEntryId,
        MemoryScope, MemoryStatus,
    };

    /// A project with global memory rooted *inside* the tempdir, so the test is
    /// hermetic (never touches the real home directory) and global scope is allowed.
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

    fn entry(id: &str, scope: MemoryScope, body: &str) -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new(id),
            scope,
            body: body.to_string(),
            category: LessonCategory::Process,
            confidence: Confidence::new(0.8).unwrap(),
            source_session: None,
            evidence: vec![EvidenceRef::new(EvidenceKind::ManualNote, "a note")],
            tags: vec!["accepted".to_string()],
            related_files: vec!["src/lib.rs".to_string()],
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
        }
    }

    fn seed_two_scopes(root: &Path) {
        let store = MemoryPersistence::open_project(root).unwrap();
        store
            .persist_memory_entry(&entry(
                "mem-project",
                MemoryScope::Project,
                "run the integration suite after an exporter change",
            ))
            .unwrap();
        store
            .persist_memory_entry(&entry(
                "mem-global",
                MemoryScope::GlobalUser,
                "prefer a guard clause over deep nesting",
            ))
            .unwrap();
    }

    #[test]
    fn bundle_round_trips_through_json_losslessly() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        seed_two_scopes(&root);

        let outcome = MemoryBundleExporter::open_project(&root)
            .unwrap()
            .export(BundleScope::Both, "tester")
            .unwrap();
        let bundle = outcome.bundle;
        assert_eq!(bundle.format_version, MEMORY_BUNDLE_FORMAT_VERSION);
        assert_eq!(bundle.entries.len(), 2);

        let json = bundle.to_pretty_json().unwrap();
        let back = MemoryBundle::from_json(&json).unwrap();
        assert_eq!(back, bundle, "bundle survives a JSON round-trip");
        // The lessons are reconstructible: bodies + scopes + edges preserved.
        let bodies: Vec<_> = back.entries.iter().map(|e| e.body.as_str()).collect();
        assert!(bodies.iter().any(|b| b.contains("integration suite")));
        assert!(bodies.iter().any(|b| b.contains("guard clause")));
    }

    #[test]
    fn export_is_deterministic_and_content_addressable() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        seed_two_scopes(&root);
        let exporter = MemoryBundleExporter::open_project(&root).unwrap();

        let first = exporter.export(BundleScope::Both, "tester").unwrap();
        let second = exporter.export(BundleScope::Both, "tester").unwrap();
        assert_eq!(
            first.bundle.canonical_bytes().unwrap(),
            second.bundle.canonical_bytes().unwrap(),
            "the same content yields the same canonical bytes"
        );
        // Entries are ordered by id, so the layout is stable regardless of store
        // iteration order.
        let ids: Vec<_> = first
            .bundle
            .entries
            .iter()
            .map(|e| e.id.as_str().to_string())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn scope_selection_filters_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        seed_two_scopes(&root);
        let exporter = MemoryBundleExporter::open_project(&root).unwrap();

        let project_only = exporter.export(BundleScope::Project, "t").unwrap().bundle;
        assert_eq!(project_only.entries.len(), 1);
        assert_eq!(project_only.entries[0].scope, MemoryScope::Project);

        let global_only = exporter.export(BundleScope::Global, "t").unwrap().bundle;
        assert_eq!(global_only.entries.len(), 1);
        assert_eq!(global_only.entries[0].scope, MemoryScope::GlobalUser);
    }

    #[test]
    fn export_only_includes_accepted_active_memory() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let store = MemoryPersistence::open_project(&root).unwrap();
        store
            .persist_memory_entry(&entry("mem-a", MemoryScope::Project, "active lesson"))
            .unwrap();
        // Delete it: a non-active memory must not appear in the export.
        store
            .delete_memory(&MemoryEntryId::new("mem-a"), "test")
            .unwrap();
        store
            .persist_memory_entry(&entry(
                "mem-b",
                MemoryScope::Project,
                "the surviving lesson",
            ))
            .unwrap();

        let bundle = MemoryBundleExporter::open_project(&root)
            .unwrap()
            .export(BundleScope::Both, "t")
            .unwrap()
            .bundle;
        assert_eq!(bundle.entries.len(), 1);
        assert_eq!(bundle.entries[0].id.as_str(), "mem-b");
    }

    #[test]
    fn a_planted_secret_is_redacted_from_the_export_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = project(&dir);
        let store = MemoryPersistence::open_project(&root).unwrap();
        store
            .persist_memory_entry(&entry(
                "mem-leak",
                MemoryScope::Project,
                "the api key is sk-proj-abcdefghijklmnopqrstuvwxyz123456 do not commit it",
            ))
            .unwrap();

        let outcome = MemoryBundleExporter::open_project(&root)
            .unwrap()
            .export(BundleScope::Both, "t")
            .unwrap();
        assert!(outcome.scan.found_secrets(), "the scan flags the secret");
        assert!(outcome.scan.entries_with_redactions >= 1);
        let body = &outcome.bundle.entries[0].body;
        assert!(
            !body.contains("sk-proj-abcdefghijklmnopqrstuvwxyz123456"),
            "the secret must not survive into the bundle: {body}"
        );
        assert!(body.contains("[REDACTED:"));
        // The header carries the redaction count for the confirm seam.
        assert_eq!(
            outcome.bundle.metadata.redaction_count,
            outcome.scan.redactions
        );
    }

    #[test]
    fn from_json_rejects_a_newer_format_version() {
        let json = serde_json::json!({
            "format_version": MEMORY_BUNDLE_FORMAT_VERSION + 1,
            "metadata": {
                "created_by": "future",
                "scope_selection": "both",
                "entry_count": 0,
                "redaction_count": 0
            },
            "entries": []
        })
        .to_string();
        assert!(matches!(
            MemoryBundle::from_json(&json),
            Err(BundleError::UnsupportedVersion { .. })
        ));
    }
}
