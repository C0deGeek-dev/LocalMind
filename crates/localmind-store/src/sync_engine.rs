//! `localmind sync` — exchange encrypted memory through a dumb sync folder.
//!
//! LocalMind opens **no sockets**: the folder is carried by the user's chosen
//! transport (Syncthing / OneDrive / a share / a private git repo). A run does
//! two things:
//!
//! 1. **Export** the current syncable memory as a full snapshot op-bundle, signed
//!    and sealed to every enrolled device, written to one opaque per-device file
//!    (atomic temp-then-rename, so a peer never reads a half-written bundle).
//! 2. **Import** each peer's bundle: parse (skip partial/foreign files), decrypt
//!    with this device's key (skip if not a recipient), verify the signature
//!    against the enrolled devices (an **unknown signer is rejected fail-closed**),
//!    and route every op into the **review queue** — never straight to active
//!    memory. A same-memory divergence routes to review as a conflict (never
//!    last-writer-wins); a proposed deletion flags the memory for review (never
//!    auto-deletes). A per-peer cursor plus the local snapshot make re-runs
//!    idempotent and prevent echo loops.
//!
//! Auto-accepting synced ops from an enrolled device (bypassing manual review)
//! and *generating* tombstones on local delete are deliberately out of scope for
//! this layer — see the plan decision log; the safe review-everything posture is
//! what ships.

use std::path::{Path, PathBuf};

use crate::{
    author_fingerprint, BundleScope, EncryptedBundle, KeyStore, MemoryBundleExporter,
    MemoryPersistence, ReviewQueue, SignedSyncBundle, SyncCursor, SyncOp, TrustClass,
    VerificationOutcome,
};
use localmind_core::{
    CandidateDestination, CandidateLesson, EvidenceKind, EvidenceRef, LessonId, MemoryEntry,
    MemoryEntryId, MemoryScope, SessionId, SuggestedAction,
};
use serde::Serialize;
use std::collections::BTreeMap;
use thiserror::Error;

const BUNDLE_EXTENSION: &str = "sync";
const SYNC_STATE_DIR: &str = "sync";

/// What one `localmind sync` run did.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SyncRunReport {
    /// Ops written in this device's own exported snapshot.
    pub exported_ops: usize,
    /// Whether an encrypted bundle was written (false when there are no enrolled
    /// devices to encrypt to — fail-closed).
    pub exported: bool,
    /// Peer bundle files discovered (excluding this device's own).
    pub peers_scanned: usize,
    /// New review candidates enqueued from peers' ops.
    pub imported_candidates: usize,
    /// Ops that diverged from a local memory and were routed to review as a
    /// conflict (never last-writer-wins).
    pub conflicts: usize,
    /// Memories a peer proposed deleting, flagged for review (never auto-deleted).
    pub tombstones_flagged: usize,
    /// Peer bundles rejected because their signer is not an enrolled device.
    pub rejected_unknown_signer: usize,
    /// Peer bundle files skipped because they were unreadable, not addressed to
    /// this device, or still being written by the transport.
    pub skipped_files: usize,
}

/// A snapshot of sync state for `localmind sync status`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncStatus {
    pub folder: String,
    pub own_fingerprint: Option<String>,
    pub enrolled_devices: usize,
    pub peer_bundles: usize,
    pub tracked_peers: usize,
    pub pending_review: usize,
}

/// Drives one project's sync against a folder.
pub struct SyncEngine {
    project_root: PathBuf,
}

impl SyncEngine {
    #[must_use]
    pub fn open(project_root: impl AsRef<Path>) -> Self {
        Self {
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    /// Export this device's snapshot into `folder` and import every peer's
    /// bundle. Idempotent: re-running with no changes enqueues nothing new.
    ///
    /// # Errors
    /// [`SyncEngineError`] if the store or folder cannot be read/written.
    pub fn run(&self, folder: &Path) -> Result<SyncRunReport, SyncEngineError> {
        std::fs::create_dir_all(folder).map_err(|source| SyncEngineError::Folder {
            path: folder.to_path_buf(),
            source,
        })?;
        let store = KeyStore::open(&self.project_root)?;

        // The current syncable snapshot, redacted by the shared exporter, also
        // serves as the local view for conflict detection on import.
        let local_entries = self.syncable_snapshot()?;
        let local_by_id: BTreeMap<String, MemoryEntry> = local_entries
            .iter()
            .map(|entry| (entry.id.to_string(), entry.clone()))
            .collect();

        let mut report = SyncRunReport::default();

        // --- Export -------------------------------------------------------
        let signing = store.load_or_generate()?;
        let own_fingerprint = author_fingerprint(&signing.verifying_key().to_bytes());
        let devices = store.enrolled_devices()?;
        let ops: Vec<SyncOp> = local_entries.iter().map(snapshot_op).collect();
        report.exported_ops = ops.len();
        if devices.is_empty() {
            // Fail-closed: with no enrolled device to encrypt to, write nothing.
            report.exported = false;
        } else {
            let signed = SignedSyncBundle::sign(crate::SyncBundle::new(ops), &signing)?;
            let encrypted = EncryptedBundle::seal_to_devices(&signed, &devices)?;
            self.write_bundle(folder, &own_fingerprint, &encrypted)?;
            report.exported = true;
        }

        // --- Import -------------------------------------------------------
        let device_secret = store.load_or_generate_device_key()?;
        let trusted_keys = store.trusted_keys()?;
        let queue = ReviewQueue::open_project(&self.project_root)?;
        let persistence = MemoryPersistence::open_project(&self.project_root)?;

        for path in self.peer_bundles(folder, &own_fingerprint)? {
            let Ok(text) = std::fs::read_to_string(&path) else {
                report.skipped_files += 1;
                continue;
            };
            let Ok(encrypted) = EncryptedBundle::from_json(&text) else {
                // A partial write or a foreign file: skip, never fatal.
                report.skipped_files += 1;
                continue;
            };
            let Ok(signed) = encrypted.open(&device_secret) else {
                // Not a recipient (or undecryptable): not for us.
                report.skipped_files += 1;
                continue;
            };
            // Fail-closed: only an enrolled device's signature is accepted.
            match signed.verify(&trusted_keys) {
                VerificationOutcome::Verified {
                    class: TrustClass::Trusted,
                    ..
                } => {}
                _ => {
                    report.rejected_unknown_signer += 1;
                    continue;
                }
            }
            report.peers_scanned += 1;
            let author = signed.signature.author.clone();
            let mut cursor = self.load_cursor(&author)?;
            let mut candidates = Vec::new();
            for op in &signed.bundle.ops {
                self.route_op(
                    op,
                    &author,
                    &local_by_id,
                    &mut cursor,
                    &mut candidates,
                    &persistence,
                    &mut report,
                )?;
            }
            if !candidates.is_empty() {
                let session = SessionId::new(format!("sync-import-{author}"));
                let inserted = queue.enqueue_candidates(&session, &candidates)?;
                report.imported_candidates += inserted;
            }
            self.save_cursor(&author, &cursor)?;
        }

        Ok(report)
    }

    /// Report the current sync state without changing anything.
    ///
    /// # Errors
    /// [`SyncEngineError`] if the store or folder cannot be read.
    pub fn status(&self, folder: &Path) -> Result<SyncStatus, SyncEngineError> {
        let store = KeyStore::open(&self.project_root)?;
        let own_fingerprint = store.public_key()?.map(|key| author_fingerprint(&key));
        let enrolled = store.enrolled_devices()?.len();
        let peer_bundles = match &own_fingerprint {
            Some(fingerprint) => self.peer_bundles(folder, fingerprint)?.len(),
            None => 0,
        };
        let tracked_peers = self.tracked_peers()?;
        let pending_review = ReviewQueue::open_project(&self.project_root)?
            .summary()
            .map(|summary| summary.pending)
            .unwrap_or(0);
        Ok(SyncStatus {
            folder: folder.display().to_string(),
            own_fingerprint,
            enrolled_devices: enrolled,
            peer_bundles,
            tracked_peers,
            pending_review,
        })
    }

    /// The syncable, redacted snapshot: accepted memory whose disposition syncs.
    fn syncable_snapshot(&self) -> Result<Vec<MemoryEntry>, SyncEngineError> {
        let exporter = MemoryBundleExporter::open_project(&self.project_root)?;
        let outcome = exporter.export(BundleScope::Both, "sync")?;
        Ok(outcome
            .bundle
            .entries
            .into_iter()
            .filter(MemoryEntry::syncs)
            .collect())
    }

    /// Decide what one incoming op becomes: skip (already have it), a create/
    /// update/supersede review candidate, a conflict candidate, or a tombstone
    /// flag — advancing the peer cursor either way (echo prevention).
    #[allow(clippy::too_many_arguments)]
    fn route_op(
        &self,
        op: &SyncOp,
        author: &str,
        local_by_id: &BTreeMap<String, MemoryEntry>,
        cursor: &mut SyncCursor,
        candidates: &mut Vec<CandidateLesson>,
        persistence: &MemoryPersistence,
        report: &mut SyncRunReport,
    ) -> Result<(), SyncEngineError> {
        // Already imported this exact version from this peer → idempotent skip.
        if cursor.versions.get(&op.memory_id) == Some(&op.version) {
            return Ok(());
        }
        cursor
            .versions
            .insert(op.memory_id.clone(), op.version.clone());

        if matches!(op.kind, crate::OpKind::Tombstone) {
            // A proposed deletion never auto-deletes: flag the local memory for
            // review (reusing the standing route-to-review path, D-LM-0016).
            if local_by_id.contains_key(&op.memory_id) {
                persistence.flag_for_review(
                    &MemoryEntryId::new(op.memory_id.as_str()),
                    "deletion proposed by a synced device",
                )?;
                report.tombstones_flagged += 1;
            }
            return Ok(());
        }

        let Some(entry) = &op.entry else {
            return Ok(()); // a non-tombstone op with no payload is ignored
        };

        match local_by_id.get(&op.memory_id) {
            // Already have the identical memory → nothing to do.
            Some(local) if memory_matches(local, entry) => Ok(()),
            // A divergent local memory: route to review as a conflict under a
            // fresh id so promotion can never overwrite the local one (no LWW).
            Some(_) => {
                candidates.push(conflict_candidate(entry, author, &op.memory_id));
                report.conflicts += 1;
                Ok(())
            }
            // New (or a supersede/update we don't yet have) → a review candidate.
            None => {
                candidates.push(incoming_candidate(entry, author));
                Ok(())
            }
        }
    }

    fn write_bundle(
        &self,
        folder: &Path,
        own_fingerprint: &str,
        bundle: &EncryptedBundle,
    ) -> Result<(), SyncEngineError> {
        let json = bundle.to_pretty_json()?;
        let final_path = folder.join(format!("{own_fingerprint}.{BUNDLE_EXTENSION}"));
        // Atomic publish: write a temp file then rename, so a peer never reads a
        // half-written bundle.
        let tmp_path = folder.join(format!("{own_fingerprint}.{BUNDLE_EXTENSION}.tmp"));
        std::fs::write(&tmp_path, json).map_err(|source| SyncEngineError::Folder {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, &final_path).map_err(|source| SyncEngineError::Folder {
            path: final_path,
            source,
        })
    }

    /// Peer bundle files in the folder: `*.sync`, excluding this device's own and
    /// any in-progress `.tmp` writes.
    fn peer_bundles(
        &self,
        folder: &Path,
        own_fingerprint: &str,
    ) -> Result<Vec<PathBuf>, SyncEngineError> {
        let own_name = format!("{own_fingerprint}.{BUNDLE_EXTENSION}");
        let mut bundles = Vec::new();
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(bundles),
            Err(source) => {
                return Err(SyncEngineError::Folder {
                    path: folder.to_path_buf(),
                    source,
                })
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_bundle = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(BUNDLE_EXTENSION));
            let is_own = path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(own_name.as_str()));
            if is_bundle && !is_own {
                bundles.push(path);
            }
        }
        bundles.sort();
        Ok(bundles)
    }

    fn sync_state_dir(&self) -> PathBuf {
        self.project_root.join(".localmind").join(SYNC_STATE_DIR)
    }

    fn cursor_path(&self, peer: &str) -> PathBuf {
        self.sync_state_dir().join(format!("cursor-{peer}.json"))
    }

    fn load_cursor(&self, peer: &str) -> Result<SyncCursor, SyncEngineError> {
        match std::fs::read_to_string(self.cursor_path(peer)) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| SyncEngineError::Cursor(e.to_string()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SyncCursor::default()),
            Err(source) => Err(SyncEngineError::Folder {
                path: self.cursor_path(peer),
                source,
            }),
        }
    }

    fn save_cursor(&self, peer: &str, cursor: &SyncCursor) -> Result<(), SyncEngineError> {
        let dir = self.sync_state_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|source| SyncEngineError::Folder { path: dir, source })?;
        let json = serde_json::to_string_pretty(cursor)
            .map_err(|e| SyncEngineError::Cursor(e.to_string()))?;
        std::fs::write(self.cursor_path(peer), json).map_err(|source| SyncEngineError::Folder {
            path: self.cursor_path(peer),
            source,
        })
    }

    fn tracked_peers(&self) -> Result<usize, SyncEngineError> {
        let dir = self.sync_state_dir();
        let count = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("cursor-"))
                .count(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(source) => return Err(SyncEngineError::Folder { path: dir, source }),
        };
        Ok(count)
    }
}

use crate::sync_bundle::memory_version;

/// A full-snapshot op for one memory: a supersede when it retires another, else
/// a create (the import side decides create-vs-update against its local state).
fn snapshot_op(entry: &MemoryEntry) -> SyncOp {
    let kind = if entry.supersedes.is_empty() {
        crate::OpKind::Create
    } else {
        crate::OpKind::Supersede
    };
    let origin_device = entry
        .sync_meta
        .origin_env
        .as_ref()
        .map(|env| env.device_label.clone())
        .filter(|label| !label.is_empty())
        .unwrap_or_default();
    SyncOp::new(
        kind,
        entry.id.to_string(),
        memory_version(entry),
        origin_device,
        Some(entry.clone()),
    )
}

/// Whether a local memory already matches an incoming entry (same content
/// version), so an incoming op is a no-op rather than a conflict.
fn memory_matches(local: &MemoryEntry, incoming: &MemoryEntry) -> bool {
    memory_version(local) == memory_version(incoming)
}

/// A review candidate for an incoming memory from a trusted synced device.
fn incoming_candidate(entry: &MemoryEntry, author: &str) -> CandidateLesson {
    base_candidate(entry, LessonId::new(entry.id.as_str()), author, None)
}

/// A review candidate for a *conflicting* incoming memory: a fresh id so
/// promotion can never overwrite the diverging local memory (no last-writer-wins).
fn conflict_candidate(entry: &MemoryEntry, author: &str, memory_id: &str) -> CandidateLesson {
    let id = LessonId::new(format!(
        "sync-conflict-{memory_id}-{}",
        &memory_version(entry)[..8]
    ));
    base_candidate(
        entry,
        id,
        author,
        Some(format!(
            "CONFLICT: a synced device changed memory '{memory_id}' which also changed locally — \
             review both before reconciling (the local memory is left untouched)"
        )),
    )
}

fn base_candidate(
    entry: &MemoryEntry,
    id: LessonId,
    author: &str,
    conflict_note: Option<String>,
) -> CandidateLesson {
    let mut candidate = CandidateLesson::new(
        id,
        entry.body.clone(),
        entry.category.clone(),
        entry.confidence,
        SuggestedAction::PromoteToMemory,
    );
    for evidence in &entry.evidence {
        candidate = candidate.with_evidence(evidence.clone());
    }
    // Surface the origin in the review view: the sending device's fingerprint and,
    // when stamped, the machine the memory was originally written on.
    let origin = entry
        .sync_meta
        .origin_env
        .as_ref()
        .map(|env| format!("{} ({}/{})", env.device_label, env.os, env.arch))
        .filter(|summary| !summary.starts_with(" ("));
    let note = match origin {
        Some(origin) => format!("imported from synced device {author}; origin machine {origin}"),
        None => format!("imported from synced device {author}"),
    };
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
        Some(conflict_note.unwrap_or_else(|| {
            "imported from a synced device: review before promotion".to_string()
        }));
    candidate
}

/// Errors driving a sync run. Never carries memory content.
#[derive(Debug, Error)]
pub enum SyncEngineError {
    #[error("failed to access sync folder {path:?}: {source}")]
    Folder {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("sync cursor is malformed: {0}")]
    Cursor(String),
    #[error(transparent)]
    Signing(#[from] crate::SigningError),
    #[error(transparent)]
    SyncBundle(#[from] crate::SyncBundleError),
    #[error(transparent)]
    Bundle(#[from] crate::BundleError),
    #[error(transparent)]
    Review(#[from] crate::ReviewQueueError),
    #[error(transparent)]
    Persistence(#[from] crate::MemoryPersistenceError),
}
