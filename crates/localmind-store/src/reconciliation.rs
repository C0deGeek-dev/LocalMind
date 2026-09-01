//! Orphan memory-file reconciliation sweep.
//!
//! A memory Markdown file is written before the SQLite transaction that
//! indexes it, so a crash in that window leaves a fully-written file with no
//! matching index row: invisible to search, and never repaired by anything
//! else, because a memory id is written once and never reused (an ordinary
//! retry writes a *new* id, not the orphaned one).
//!
//! The sweep set is defined by a query, not a cursor, mirroring
//! `backfill.rs`'s vector sweep: idempotent and resumable, correct without
//! any persisted progress state. Report-only (dry-run) by default; a caller
//! must ask for the apply path to actually reindex anything. Every candidate
//! is fully read, parsed, and validated (its front matter must name the
//! same id and scope its filename and directory imply) **during planning**,
//! not deferred to apply — a mismatched or unparseable file is reported for
//! manual review, never guessed at. An id that already carries a
//! `MemorySuperseded`/`MemoryDeleted` audit event is reported the same way
//! and never reindexed, even under apply — a legitimately retired memory
//! must never resurface into active search just because its file (the
//! durable, never-deleted source of truth) is still on disk. Because a scan
//! and a later apply are two separate moments, apply re-verifies every
//! precondition inside the same transaction as the write it is about to
//! make, and skips (rather than blindly writes) anything whose state moved
//! in between.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use time::OffsetDateTime;

use crate::markdown::MarkdownMemoryFormat;
use crate::memory_persistence::{MemoryPersistence, MemoryPersistenceError};
use localmind_core::{content_fingerprint, AuditEventKind, MemoryEntry, MemoryScope};

/// One `.md` file on disk with no matching `memory_index` row, confirmed
/// self-consistent (front matter id/scope match the filename/directory it
/// was found under) and free of any retirement audit event — safe to
/// reindex as far as planning can tell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrphanEntry {
    pub memory_id: String,
    pub path: PathBuf,
    pub scope: MemoryScope,
    /// A content fingerprint of the body planning read, so apply can detect
    /// a same-id/same-scope body replacement between the scan and the
    /// write — not just that the id/scope still match, but that the
    /// content a reviewer would have seen in the plan is the content that
    /// is about to be indexed.
    pub content_fingerprint: String,
}

/// Why a candidate orphan was routed to manual review instead of the
/// reindexable set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlagReason {
    /// A `MemorySuperseded`/`MemoryDeleted` audit event already exists for
    /// this id.
    Retired,
    /// The file could not be read from disk, or its front matter could not
    /// be parsed. Carries a short description of the failure.
    Unreadable(String),
    /// The file's front matter names a different id than its filename.
    IdMismatch { parsed_id: String },
    /// The file's front matter names a different scope than the directory
    /// it was found in.
    ScopeMismatch { parsed_scope: MemoryScope },
    /// The directory entry is not a regular file (a symlink, most
    /// plausibly) — never opened or followed, since it could point outside
    /// the memory root.
    NotRegularFile,
    /// Apply-time only: another writer indexed this id in the gap between
    /// planning and this write.
    AlreadyIndexed,
    /// Apply-time only: the file's content changed in the gap between
    /// planning and this write — the id/scope still match, but the body
    /// about to be indexed is not the body a reviewer of the plan would
    /// have seen.
    ContentChanged,
}

/// A candidate that was **not** classified as safely reindexable, with why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlaggedOrphan {
    pub entry: OrphanEntry,
    pub reason: FlagReason,
}

/// The sweep set, split into the two decisions planning makes: safe to
/// reindex automatically, versus needing a human.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrphanReport {
    pub reindexable: Vec<OrphanEntry>,
    pub flagged_for_review: Vec<FlaggedOrphan>,
}

impl OrphanReport {
    #[must_use]
    pub fn total(&self) -> usize {
        self.reindexable.len() + self.flagged_for_review.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    fn merge(&mut self, other: OrphanReport) {
        self.reindexable.extend(other.reindexable);
        self.flagged_for_review.extend(other.flagged_for_review);
    }
}

/// What an apply run actually did, alongside what it found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReconciliationReport {
    pub found: OrphanReport,
    /// How many of `found.reindexable` were successfully repaired. Always
    /// `0` for a dry-run plan.
    pub reindexed: usize,
    /// Entries from `found.reindexable` whose state had changed by the time
    /// apply reached them (another process indexed or retired the id, or
    /// the file no longer matches what planning saw), with why — skipped
    /// rather than written. Distinct from `found.flagged_for_review`, which
    /// planning already knew about before any write was attempted.
    pub stale: Vec<FlaggedOrphan>,
}

/// Every scope `MemoryPathResolver::write_memory_file` will ever write
/// under the *project* memory root — every scope except `GlobalUser`,
/// which is rooted at the separate global store instead (handled on its
/// own in [`MemoryPersistence::orphan_sweep_plan`]). Not every project
/// enables all of these (`ProjectConfig::allows_scope` gates each one), but
/// the sweep must not silently skip a scope the *config* allows just
/// because it defaults to unused — `write_memory_file` writes it the same
/// way whichever scope is configured.
const PROJECT_ROOTED_SCOPES: [MemoryScope; 4] = [
    MemoryScope::Project,
    MemoryScope::Session,
    MemoryScope::Skill,
    MemoryScope::Research,
];

impl MemoryPersistence {
    /// Report-only: scans every scope directory this project's config
    /// allows under the project memory root, plus the global store when one
    /// is open, for orphaned memory files. No side effects.
    pub fn orphan_sweep_plan(&self) -> Result<OrphanReport, MemoryPersistenceError> {
        let mut report = OrphanReport::default();
        let memory_root = self.config().memory_root();
        for scope in PROJECT_ROOTED_SCOPES {
            if !self.config().allows_scope(&scope) {
                continue;
            }
            report.merge(scan_scope(self.connection(), &memory_root, scope)?);
        }
        if let (Some(global_connection), Some(global_root)) =
            (self.global_connection(), self.config().global_memory_root())
        {
            report.merge(scan_scope(
                global_connection,
                &global_root,
                MemoryScope::GlobalUser,
            )?);
        }
        Ok(report)
    }

    /// Scans exactly like [`Self::orphan_sweep_plan`], then reindexes every
    /// `reindexable` entry found — reusing the same `index_memory_with` the
    /// promote/persist paths use, so indexing logic never forks in two — and
    /// records an `OrphanReconciled` audit row per repaired file, tagged
    /// with a shared identifier for this apply run. Entries in
    /// `flagged_for_review` are never touched. Every write re-verifies its
    /// preconditions inside its own transaction immediately before writing,
    /// so a state change between the scan above and this call is skipped
    /// (reported in `stale`), never blindly applied.
    pub fn orphan_sweep_apply(&self) -> Result<ReconciliationReport, MemoryPersistenceError> {
        let found = self.orphan_sweep_plan()?;
        let sweep_run = OffsetDateTime::now_utc().to_string();
        let mut reindexed = 0usize;
        let mut stale = Vec::new();
        for entry in &found.reindexable {
            let connection = match entry.scope {
                MemoryScope::GlobalUser => self
                    .global_connection()
                    .ok_or(MemoryPersistenceError::GlobalStoreUnavailable)?,
                _ => self.connection(),
            };
            match reindex_one(connection, entry, &sweep_run)? {
                ReindexOutcome::Reindexed => reindexed += 1,
                ReindexOutcome::Stale(reason) => stale.push(FlaggedOrphan {
                    entry: entry.clone(),
                    reason,
                }),
            }
        }
        Ok(ReconciliationReport {
            found,
            reindexed,
            stale,
        })
    }
}

/// Scans one store's `<scope>/` memory directory for orphans, fully
/// validating each candidate (not merely noting its filename).
fn scan_scope(
    connection: &Connection,
    memory_root: &Path,
    scope: MemoryScope,
) -> Result<OrphanReport, MemoryPersistenceError> {
    let scope_dir = memory_root.join(crate::paths::scope_dir(&scope));

    let entries = match fs::read_dir(&scope_dir) {
        Ok(entries) => entries,
        // No directory yet means no memory has ever been written there —
        // an empty sweep result, not an error.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OrphanReport::default());
        }
        Err(source) => {
            return Err(MemoryPersistenceError::ScanMemoryRoot {
                path: scope_dir,
                source,
            });
        }
    };

    let mut report = OrphanReport::default();
    for entry in entries {
        let entry = entry.map_err(|source| MemoryPersistenceError::ScanMemoryRoot {
            path: scope_dir.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue; // a stray non-.md file (e.g. a leftover atomic_write .tmp) is not memory
        }
        let Some(memory_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };

        // Never open or follow anything but a plain regular file: a
        // symlinked `.md` name inside a scope directory could point
        // anywhere on disk, and this sweep must never read (let alone
        // index) content from outside the memory root it was asked to
        // scan.
        let file_type =
            entry
                .file_type()
                .map_err(|source| MemoryPersistenceError::ScanMemoryRoot {
                    path: path.clone(),
                    source,
                })?;
        if !file_type.is_file() {
            report.flagged_for_review.push(FlaggedOrphan {
                entry: placeholder(memory_id, &path, &scope),
                reason: FlagReason::NotRegularFile,
            });
            continue;
        }

        if is_indexed(connection, memory_id)? {
            continue; // has a memory_index row already — not an orphan, whatever its status
        }

        if has_retirement_event(connection, memory_id)? {
            report.flagged_for_review.push(FlaggedOrphan {
                entry: placeholder(memory_id, &path, &scope),
                reason: FlagReason::Retired,
            });
            continue;
        }

        match read_and_validate(memory_id, &path, &scope) {
            Ok((_, fingerprint)) => report.reindexable.push(OrphanEntry {
                memory_id: memory_id.to_string(),
                path: path.clone(),
                scope: scope.clone(),
                content_fingerprint: fingerprint,
            }),
            Err(reason) => report.flagged_for_review.push(FlaggedOrphan {
                entry: placeholder(memory_id, &path, &scope),
                reason,
            }),
        }
    }
    Ok(report)
}

/// An `OrphanEntry` for a candidate that is being flagged rather than
/// reindexed — its `content_fingerprint` is meaningless (flagged entries
/// are never applied) and left empty rather than computed.
fn placeholder(memory_id: &str, path: &Path, scope: &MemoryScope) -> OrphanEntry {
    OrphanEntry {
        memory_id: memory_id.to_string(),
        path: path.to_path_buf(),
        scope: scope.clone(),
        content_fingerprint: String::new(),
    }
}

/// Reads and parses `path`, then confirms its front matter names the exact
/// `expected_id`/`expected_scope` the filename/directory already imply,
/// returning the parsed entry and its content fingerprint on success. A
/// file whose content disagrees with its own name is never guessed at — it
/// is the caller's job to route it to manual review.
fn read_and_validate(
    expected_id: &str,
    path: &Path,
    expected_scope: &MemoryScope,
) -> Result<(MemoryEntry, String), FlagReason> {
    let text =
        fs::read_to_string(path).map_err(|source| FlagReason::Unreadable(source.to_string()))?;
    let entry = MarkdownMemoryFormat::parse(&text)
        .map_err(|source| FlagReason::Unreadable(source.to_string()))?;
    if entry.id.as_str() != expected_id {
        return Err(FlagReason::IdMismatch {
            parsed_id: entry.id.as_str().to_string(),
        });
    }
    if &entry.scope != expected_scope {
        return Err(FlagReason::ScopeMismatch {
            parsed_scope: entry.scope,
        });
    }
    let fingerprint = content_fingerprint(&entry.body);
    Ok((entry, fingerprint))
}

/// Whether `memory_id` already has a `memory_index` row, of any `status` —
/// a superseded row is still a row, so its file is not an orphan.
fn is_indexed(connection: &Connection, memory_id: &str) -> Result<bool, MemoryPersistenceError> {
    connection
        .query_row(
            "SELECT 1 FROM memory_index WHERE memory_id = ?1 LIMIT 1",
            params![memory_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(MemoryPersistenceError::Sqlite)
}

/// Whether `memory_id` has a prior `MemorySuperseded` or `MemoryDeleted`
/// audit event naming it as the subject — the signal that a file present on
/// disk with no index row might be a legitimately retired memory rather
/// than a genuine crash orphan. Defensive: no producer in this crate
/// removes a `memory_index` row on supersede or leaves the file behind on
/// delete today, but this check must hold even if that ever changes, since
/// the cost of getting it wrong is a retired lesson resurfacing.
fn has_retirement_event(
    connection: &Connection,
    memory_id: &str,
) -> Result<bool, MemoryPersistenceError> {
    connection
        .query_row(
            "SELECT 1 FROM audit_events \
             WHERE subject = ?1 AND kind IN ('MemorySuperseded', 'MemoryDeleted') LIMIT 1",
            params![memory_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(MemoryPersistenceError::Sqlite)
}

/// What [`reindex_one`] actually did.
#[derive(Debug)]
enum ReindexOutcome {
    Reindexed,
    /// A precondition no longer held when the write was about to happen —
    /// not an error: "someone else already resolved this orphan" (or its
    /// content moved) is not a fault, just something to report.
    Stale(FlagReason),
}

/// Re-verifies `orphan` from scratch — still unindexed, still no retirement
/// event, its file still readable/parseable/self-consistent, and its
/// content fingerprint still matches what planning saw — inside the same
/// transaction as the write, then indexes it and records the recovery.
fn reindex_one(
    connection: &Connection,
    orphan: &OrphanEntry,
    sweep_run: &str,
) -> Result<ReindexOutcome, MemoryPersistenceError> {
    let tx = connection
        .unchecked_transaction()
        .map_err(MemoryPersistenceError::Sqlite)?;

    if is_indexed(&tx, &orphan.memory_id)? {
        return Ok(ReindexOutcome::Stale(FlagReason::AlreadyIndexed));
    }
    if has_retirement_event(&tx, &orphan.memory_id)? {
        return Ok(ReindexOutcome::Stale(FlagReason::Retired));
    }
    let (entry, fingerprint) =
        match read_and_validate(&orphan.memory_id, &orphan.path, &orphan.scope) {
            Ok(pair) => pair,
            Err(reason) => return Ok(ReindexOutcome::Stale(reason)),
        };
    if fingerprint != orphan.content_fingerprint {
        return Ok(ReindexOutcome::Stale(FlagReason::ContentChanged));
    }

    // The orphan's own file predates this sweep and carries no session
    // context to infer a language from beyond its body — the same
    // body-wins-only branch a directly-persisted entry uses.
    MemoryPersistence::index_memory_with(&tx, &entry, &orphan.path, None)?;
    MemoryPersistence::write_audit_with(
        &tx,
        AuditEventKind::OrphanReconciled,
        "localmind",
        orphan.memory_id.as_str(),
        &serde_json::json!({
            "path": orphan.path.to_string_lossy(),
            "sweep_run": sweep_run,
        }),
    )?;
    tx.commit().map_err(MemoryPersistenceError::Sqlite)?;
    Ok(ReindexOutcome::Reindexed)
}

#[cfg(test)]
mod reindex_one_tests {
    #![allow(clippy::unwrap_used)]
    use super::{reindex_one, FlagReason, OrphanEntry, ReindexOutcome};
    use crate::memory_persistence::MemoryPersistence;
    use localmind_core::{
        content_fingerprint, Confidence, EvidenceKind, EvidenceRef, LessonCategory, MemoryEntry,
        MemoryEntryId, MemoryScope, MemoryStatus, SessionId, SyncMeta,
    };
    use rusqlite::Connection;

    fn schema_connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::schema::migrate(&connection).unwrap();
        connection
    }

    fn entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: MemoryEntryId::new(id),
            scope: MemoryScope::Project,
            body: "a raced orphan's body".to_string(),
            category: LessonCategory::ProjectConvention,
            confidence: Confidence::new(0.9).unwrap(),
            source_session: Some(SessionId::new("seed")),
            evidence: vec![EvidenceRef::new(EvidenceKind::Transcript, "redacted").redacted()],
            tags: vec!["accepted".to_string()],
            related_files: Vec::new(),
            related_entities: Vec::new(),
            created_at: None,
            updated_at: None,
            supersedes: Vec::new(),
            contradicts: Vec::new(),
            status: MemoryStatus::Active,
            sync_meta: SyncMeta::default(),
        }
    }

    fn write_and_orphan(dir: &std::path::Path, memory_entry: &MemoryEntry) -> OrphanEntry {
        let path = dir.join(format!("{}.md", memory_entry.id.as_str()));
        std::fs::write(
            &path,
            crate::markdown::MarkdownMemoryFormat::serialize(memory_entry),
        )
        .unwrap();
        OrphanEntry {
            memory_id: memory_entry.id.as_str().to_string(),
            path,
            scope: memory_entry.scope.clone(),
            content_fingerprint: content_fingerprint(&memory_entry.body),
        }
    }

    /// The race the navigator review caught: a plan is computed, then
    /// before this specific entry's own write runs, some other writer
    /// (another process on the same store, or an earlier entry in the same
    /// apply loop) has already indexed the id. `reindex_one` must re-check
    /// and skip, never blindly upsert over it.
    #[test]
    fn skips_when_another_writer_indexed_the_id_first() {
        let connection = schema_connection();
        let dir = tempfile::tempdir().unwrap();
        let memory_entry = entry("raced-indexed");
        let orphan = write_and_orphan(dir.path(), &memory_entry);

        let tx = connection.unchecked_transaction().unwrap();
        MemoryPersistence::index_memory_with(&tx, &memory_entry, &orphan.path, None).unwrap();
        tx.commit().unwrap();

        let outcome = reindex_one(&connection, &orphan, "sweep-1").unwrap();
        assert!(
            matches!(outcome, ReindexOutcome::Stale(FlagReason::AlreadyIndexed)),
            "an id that is no longer an orphan must be skipped, not re-upserted; got {outcome:?}"
        );
    }

    /// Same race, but the concurrent change is a retirement rather than an
    /// index write.
    #[test]
    fn skips_when_a_retirement_event_appears_first() {
        let connection = schema_connection();
        let dir = tempfile::tempdir().unwrap();
        let memory_entry = entry("raced-retired");
        let orphan = write_and_orphan(dir.path(), &memory_entry);

        connection
            .execute(
                "INSERT INTO audit_events(kind, actor, subject, metadata_json, happened_at) \
                 VALUES('MemorySuperseded', 'tester', 'raced-retired', '{}', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();

        let outcome = reindex_one(&connection, &orphan, "sweep-1").unwrap();
        assert!(matches!(
            outcome,
            ReindexOutcome::Stale(FlagReason::Retired)
        ));
        let indexed: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM memory_index WHERE memory_id = 'raced-retired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed, 0,
            "a retired id must never be indexed, even under apply"
        );
    }

    #[test]
    fn succeeds_and_records_the_sweep_run_when_state_is_unchanged() {
        let connection = schema_connection();
        let dir = tempfile::tempdir().unwrap();
        let memory_entry = entry("raced-clean");
        let orphan = write_and_orphan(dir.path(), &memory_entry);

        let outcome = reindex_one(&connection, &orphan, "sweep-xyz").unwrap();
        assert!(matches!(outcome, ReindexOutcome::Reindexed));

        let metadata: String = connection
            .query_row(
                "SELECT metadata_json FROM audit_events WHERE kind = 'OrphanReconciled' AND subject = 'raced-clean'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(metadata.contains("sweep-xyz"));
    }

    /// The race navigator review caught in a later round: the id/scope
    /// still match what planning saw, but the file's *content* changed in
    /// the gap between planning and this write — must be treated as stale,
    /// not silently indexed under a body the plan never showed anyone.
    #[test]
    fn skips_when_the_file_content_changed_since_planning() {
        let connection = schema_connection();
        let dir = tempfile::tempdir().unwrap();
        let memory_entry = entry("raced-content-changed");
        let orphan = write_and_orphan(dir.path(), &memory_entry);

        // The file is edited in place after planning captured its
        // fingerprint, keeping the same id/scope.
        let mut edited = memory_entry.clone();
        edited.body = "a completely different body than planning saw".to_string();
        std::fs::write(
            &orphan.path,
            crate::markdown::MarkdownMemoryFormat::serialize(&edited),
        )
        .unwrap();

        let outcome = reindex_one(&connection, &orphan, "sweep-1").unwrap();
        assert!(matches!(
            outcome,
            ReindexOutcome::Stale(FlagReason::ContentChanged)
        ));
        let indexed: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM memory_index WHERE memory_id = 'raced-content-changed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexed, 0,
            "changed content must never be silently indexed under the plan's fingerprint"
        );
    }
}
