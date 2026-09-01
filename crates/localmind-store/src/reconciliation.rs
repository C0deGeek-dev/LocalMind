//! Orphan memory-file reconciliation sweep.
//!
//! Subject 01 made the Markdown file write itself crash-atomic (temp file
//! plus rename), but a process can still crash in the window *between* that
//! fully-written file and the SQLite transaction that indexes it
//! (`memory_persistence.rs`'s promote/persist paths). That leaves a
//! fully-written, permanently un-indexed file: invisible to search, and
//! never repaired by anything else, because a memory id is written once and
//! never reused (an ordinary retry writes a *new* id, not the orphaned one).
//!
//! Mirrors `backfill.rs`'s "the sweep set is defined by a query, never a
//! cursor" idiom: idempotent and resumable, correct without any persisted
//! progress state. Report-only (dry-run) by default; a caller must ask for
//! the apply path to actually reindex anything. An orphan whose id already
//! carries a `MemorySuperseded`/`MemoryDeleted` audit event is **never**
//! reindexed, even under apply — it is reported separately, for manual
//! review, because a legitimately retired memory must never resurface into
//! active search results just because its Markdown file (the durable,
//! never-deleted source of truth) is still on disk.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::markdown::MarkdownMemoryFormat;
use crate::memory_persistence::{MemoryPersistence, MemoryPersistenceError};
use localmind_core::{AuditEventKind, MemoryScope};

/// One `.md` file on disk with no matching `memory_index` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrphanEntry {
    pub memory_id: String,
    pub path: PathBuf,
    pub scope: MemoryScope,
}

/// The sweep set, split into the two decisions 02.2 asks for: safe to
/// reindex automatically, versus needing a human because the file's id has
/// a retirement event on record that contradicts it still being present.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrphanReport {
    pub reindexable: Vec<OrphanEntry>,
    pub flagged_for_review: Vec<OrphanEntry>,
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
    /// `0` for a dry-run plan. `found.flagged_for_review` is never touched —
    /// its length is how many were skipped for manual review.
    pub reindexed: usize,
}

impl MemoryPersistence {
    /// Report-only: scans the project store (and the global store, when one
    /// is open) for orphaned memory files. No side effects.
    pub fn orphan_sweep_plan(&self) -> Result<OrphanReport, MemoryPersistenceError> {
        let mut report = scan_scope(
            self.connection(),
            &self.config().memory_root(),
            MemoryScope::Project,
        )?;
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
    /// records an `OrphanReconciled` audit row per repaired file. Entries in
    /// `flagged_for_review` are never touched.
    pub fn orphan_sweep_apply(&self) -> Result<ReconciliationReport, MemoryPersistenceError> {
        let found = self.orphan_sweep_plan()?;
        let mut reindexed = 0usize;
        for entry in &found.reindexable {
            let connection = match entry.scope {
                MemoryScope::GlobalUser => self
                    .global_connection()
                    .ok_or(MemoryPersistenceError::GlobalStoreUnavailable)?,
                _ => self.connection(),
            };
            reindex_one(connection, entry)?;
            reindexed += 1;
        }
        Ok(ReconciliationReport { found, reindexed })
    }
}

/// Scans one store's `<scope>/` memory directory for orphans.
fn scan_scope(
    connection: &Connection,
    memory_root: &Path,
    scope: MemoryScope,
) -> Result<OrphanReport, MemoryPersistenceError> {
    let scope_dir = memory_root.join(match scope {
        MemoryScope::GlobalUser => "global",
        _ => "project",
    });

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
        if is_indexed(connection, memory_id)? {
            continue; // has a memory_index row already — not an orphan, whatever its status
        }

        let orphan = OrphanEntry {
            memory_id: memory_id.to_string(),
            path: path.clone(),
            scope: scope.clone(),
        };
        if has_retirement_event(connection, memory_id)? {
            report.flagged_for_review.push(orphan);
        } else {
            report.reindexable.push(orphan);
        }
    }
    Ok(report)
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
/// than a genuine crash orphan (see the module doc; defensive belt-and-
/// suspenders per the plan's own risk table, even though no code path today
/// actually removes a `memory_index` row on supersede or leaves the file
/// behind on delete).
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

/// Reads and parses one orphan's Markdown file, then indexes it and records
/// the recovery — in one transaction, exactly like every other indexing
/// call site in this crate.
fn reindex_one(
    connection: &Connection,
    orphan: &OrphanEntry,
) -> Result<(), MemoryPersistenceError> {
    let text = fs::read_to_string(&orphan.path).map_err(|source| {
        MemoryPersistenceError::ReadOrphanFile {
            path: orphan.path.clone(),
            source,
        }
    })?;
    let entry = MarkdownMemoryFormat::parse(&text).map_err(|source| {
        MemoryPersistenceError::ParseOrphanFile {
            path: orphan.path.clone(),
            source,
        }
    })?;

    let tx = connection
        .unchecked_transaction()
        .map_err(MemoryPersistenceError::Sqlite)?;
    // The orphan's own file predates this sweep and carries no session
    // context to infer a language from beyond its body — the same
    // body-wins-only branch `persist_memory_entry` uses for a
    // directly-persisted entry.
    MemoryPersistence::index_memory_with(&tx, &entry, &orphan.path, None)?;
    MemoryPersistence::write_audit_with(
        &tx,
        AuditEventKind::OrphanReconciled,
        "localmind",
        orphan.memory_id.as_str(),
        &serde_json::json!({ "path": orphan.path.to_string_lossy() }),
    )?;
    tx.commit().map_err(MemoryPersistenceError::Sqlite)?;
    Ok(())
}
