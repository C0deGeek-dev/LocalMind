//! Versioned schema migrations for the shared project database
//! (`.localmind/localmind.sqlite`).
//!
//! Every component that opens the database runs [`migrate`] first. The
//! stepper reads `PRAGMA user_version`, serializes stale openers with an
//! immediate transaction, applies each missing step, and stamps the new
//! version on commit. Version 1 is the
//! consolidated baseline of everything that previously ran as ad-hoc
//! `CREATE TABLE IF NOT EXISTS` batches, so upgrading a pre-versioned
//! database is a verified no-op: the guards keep existing tables and rows
//! untouched.
//!
//! The graph store's payload format is versioned separately through
//! `graph_meta` (`GRAPH_FORMAT_VERSION`); this module owns the *database*
//! schema lifecycle.

use rusqlite::{Connection, Transaction, TransactionBehavior};
use thiserror::Error;

/// Highest schema version this build understands.
pub(crate) const DB_SCHEMA_VERSION: i32 = 12;

/// How long a connection waits on a locked database before failing.
///
/// The host (e.g. a LocalPilot session) and the CLI legitimately open the
/// same `.localmind/localmind.sqlite` concurrently; without a timeout any
/// overlap surfaces as an immediate `SQLITE_BUSY`.
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Open the shared database the way every production component must: WAL
/// journal (readers don't block the writer across processes), a busy
/// timeout, and `synchronous=NORMAL` (the standard WAL pairing — durable at
/// checkpoint, not per-write fsync). WAL is a persistent database property
/// but the busy timeout is per-connection, so this helper is the single
/// sanctioned way to open the file.
///
/// WAL adds `-wal`/`-shm` sidecar files beside the database; the on-disk
/// contract documents them.
pub(crate) fn open_database(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // `journal_mode` answers with the resulting mode, so it needs the checked
    // variant; accept whatever SQLite settled on (the mode itself is pinned
    // by the contention test, not here).
    connection.pragma_update_and_check(None, "journal_mode", "WAL", |_row| Ok(()))?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(connection)
}

pub(crate) fn migrate(connection: &Connection) -> Result<(), SchemaError> {
    let current: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(SchemaError::Sqlite)?;

    // Keep the overwhelmingly common already-current open lock-free. A stale
    // read is only a hint: the authoritative version check happens after the
    // write lock is acquired below.
    if schema_is_current(current)? {
        return Ok(());
    }

    // BEGIN IMMEDIATE makes competing migrators wait at the busy handler
    // before either takes a schema snapshot or applies a non-idempotent step.
    let tx = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
        .map_err(SchemaError::Sqlite)?;
    let current: i32 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(SchemaError::Sqlite)?;
    if schema_is_current(current)? {
        tx.commit().map_err(SchemaError::Sqlite)?;
        return Ok(());
    }

    if current < 1 {
        apply_v1(&tx)?;
    }
    if current < 2 {
        apply_v2(&tx)?;
    }
    if current < 3 {
        apply_v3(&tx)?;
    }
    if current < 4 {
        apply_v4(&tx)?;
    }
    if current < 5 {
        apply_v5(&tx)?;
    }
    if current < 6 {
        apply_v6(&tx)?;
    }
    if current < 7 {
        apply_v7(&tx)?;
    }
    if current < 8 {
        apply_v8(&tx)?;
    }
    if current < 9 {
        apply_v9(&tx)?;
    }
    if current < 10 {
        apply_v10(&tx)?;
    }
    if current < 11 {
        apply_v11(&tx)?;
    }
    if current < 12 {
        apply_v12(&tx)?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {DB_SCHEMA_VERSION}"))
        .map_err(SchemaError::Sqlite)?;
    tx.commit().map_err(SchemaError::Sqlite)?;
    Ok(())
}

fn schema_is_current(current: i32) -> Result<bool, SchemaError> {
    if current > DB_SCHEMA_VERSION {
        return Err(SchemaError::TooNew {
            found: current,
            supported: DB_SCHEMA_VERSION,
        });
    }
    Ok(current == DB_SCHEMA_VERSION)
}

/// Baseline: the union of the memory-persistence and review-queue schemas as
/// they shipped before versioning existed. `IF NOT EXISTS` everywhere so a
/// database created by any earlier build steps to v1 without being touched.
fn apply_v1(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS review_items (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                candidate_json TEXT NOT NULL,
                state TEXT NOT NULL,
                reviewer_action TEXT,
                reviewer TEXT,
                note TEXT,
                replacement_summary TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_review_items_state
                ON review_items(state);
            CREATE INDEX IF NOT EXISTS idx_review_items_session
                ON review_items(session_id);

            CREATE TABLE IF NOT EXISTS audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                actor TEXT NOT NULL,
                subject TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                happened_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS memory_index (
                memory_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                scope TEXT NOT NULL,
                category TEXT NOT NULL,
                body TEXT NOT NULL,
                source_session TEXT,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts
                USING fts5(memory_id UNINDEXED, body);

            CREATE TABLE IF NOT EXISTS memory_relationships (
                memory_id TEXT NOT NULL,
                relation_kind TEXT NOT NULL,
                target TEXT NOT NULL,
                PRIMARY KEY(memory_id, relation_kind, target)
            );
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

fn apply_v2(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS vector_index (
                subject_kind TEXT NOT NULL,
                subject_id TEXT NOT NULL,
                source_fingerprint TEXT NOT NULL,
                model TEXT NOT NULL,
                dimensions INTEGER NOT NULL,
                vector_blob BLOB NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(subject_kind, subject_id)
            );

            CREATE INDEX IF NOT EXISTS idx_vector_index_kind
                ON vector_index(subject_kind);

            CREATE TABLE IF NOT EXISTS distilled_records (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                source_memory_ids_json TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT
            );

            CREATE TABLE IF NOT EXISTS skill_records (
                skill_id TEXT PRIMARY KEY,
                draft_json TEXT NOT NULL,
                status TEXT NOT NULL,
                source_memory_ids_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT
            );
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Review-queue dedup support: a normalized-canonical hash to collapse trivial
/// variants and a `seen_count` so a repeated proposal bumps the survivor instead
/// of stacking a new row. Both default so pre-existing rows upgrade cleanly.
fn apply_v3(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            ALTER TABLE review_items ADD COLUMN canonical_hash TEXT;
            ALTER TABLE review_items ADD COLUMN seen_count INTEGER NOT NULL DEFAULT 1;

            CREATE INDEX IF NOT EXISTS idx_review_items_canonical
                ON review_items(canonical_hash);
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Supersede support: the memory a `Supersede` decision retires, carried from the
/// decision to promotion. Nullable so every other decision and every pre-existing
/// row upgrades cleanly.
fn apply_v4(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch("ALTER TABLE review_items ADD COLUMN supersede_target TEXT;")
        .map_err(SchemaError::Sqlite)
}

/// Change-aware staleness support: a `stale_candidate` flag on accepted memory,
/// set when code the memory was anchored to changes. The memory stays active and
/// retrievable (just flagged for review), so retrieval can surface it rather than
/// silently drop it. Defaults to 0 so every pre-existing row upgrades cleanly.
fn apply_v5(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            "ALTER TABLE memory_index ADD COLUMN stale_candidate INTEGER NOT NULL DEFAULT 0;",
        )
        .map_err(SchemaError::Sqlite)
}

/// Epistemic-status + contradiction support on accepted memory: a deterministic
/// trust classification, a flag set when a memory is in a `contradicts`
/// relationship, and the entry's confidence (so provenance can answer "why do
/// you think that?" without re-reading the Markdown). All default so pre-existing
/// rows upgrade cleanly; a reindex repopulates the derived classification.
fn apply_v6(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            ALTER TABLE memory_index ADD COLUMN epistemic_status TEXT NOT NULL DEFAULT 'observation';
            ALTER TABLE memory_index ADD COLUMN contradicted INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE memory_index ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Language-relevance support: the single programming language an accepted
/// memory is about, so retrieval can filter off-language lessons inside the
/// query instead of dropping them after the fact. Nullable — a general or
/// cross-cutting lesson stays `NULL` and is eligible for every task — so every
/// pre-existing row upgrades cleanly (treated as language-agnostic until a
/// reindex re-detects it from the body). The index keeps the added filter cheap.
fn apply_v7(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            ALTER TABLE memory_index ADD COLUMN language TEXT;
            CREATE INDEX IF NOT EXISTS idx_memory_index_language
                ON memory_index(language);
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Proactive-lifecycle usage tracking: per-memory injection counters so the
/// freshness pass can surface never-retrieved dead weight and high-value
/// lessons. `hit_count` defaults to 0 and `last_used_at` is nullable, so every
/// pre-v8 row upgrades cleanly and reads as zero-usage. Unlike the other index
/// columns these are **runtime-accumulated**, not derived from the Markdown
/// source of truth — a reindex/rebuild resets them to zero-usage, which is the
/// same state as a fresh upgrade and is acceptable for a best-effort usage
/// signal.
fn apply_v8(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            ALTER TABLE memory_index ADD COLUMN hit_count INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE memory_index ADD COLUMN last_used_at TEXT;
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Document semantic-ingest support: chunked repository documentation stored as
/// retrievable text keyed by a stable chunk id. Each chunk's vector lives in the
/// shared `vector_index` under `subject_kind = 'doc'` (already generic since v2);
/// this table holds the passage text so a semantic hit can be shown and cited.
/// Idempotent create so re-running the migration is a no-op.
fn apply_v9(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS doc_chunk (
                chunk_id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                heading TEXT,
                body TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_doc_chunk_path
                ON doc_chunk(path);
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Cross-device origin: the label of the machine that wrote a *synced* memory,
/// so injection can down-weight (never drop) a lesson whose origin machine
/// differs from the one retrieving it. Derived from the Markdown source of truth
/// (the entry's origin environment); NULL for a memory with no origin stamp, so
/// every pre-v10 row upgrades cleanly.
fn apply_v10(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch("ALTER TABLE memory_index ADD COLUMN origin_device TEXT;")
        .map_err(SchemaError::Sqlite)
}

/// Retry-safe agent proposals. A proposal can merge into a pre-existing review
/// row, so its derived id is not necessarily the row id. This receipt preserves
/// that mapping and a content fingerprint without storing the caller's raw
/// idempotency key or proposal text.
fn apply_v11(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS proposal_receipts (
                proposal_id TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL,
                survivor_id TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_proposal_receipts_survivor
                ON proposal_receipts(survivor_id);
            "#,
        )
        .map_err(SchemaError::Sqlite)
}

/// Durable merge decisions. `ReviewAction::MergeInto` carries a review-item id,
/// but the action label alone cannot reconstruct that target after the decision.
/// Nullable so historic `reviewer_action = 'merge'` rows continue to load.
fn apply_v12(connection: &Connection) -> Result<(), SchemaError> {
    connection
        .execute_batch("ALTER TABLE review_items ADD COLUMN merge_target TEXT;")
        .map_err(SchemaError::Sqlite)
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error(
        "database schema version {found} is newer than this build supports ({supported}); \
         update LocalMind before opening this project"
    )]
    TooNew { found: i32, supported: i32 },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use super::{apply_v1, apply_v2, migrate, open_database, SchemaError, DB_SCHEMA_VERSION};
    use rusqlite::{Connection, Transaction, TransactionBehavior};
    use std::sync::{mpsc, Arc, Barrier};

    #[test]
    fn two_processes_worth_of_connections_share_the_database(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The host and the CLI open the same file concurrently. With WAL +
        // busy_timeout a second writer waits for the first instead of
        // failing SQLITE_BUSY — the exact cross-process overlap the
        // bare-`Connection::open` sites could not survive.
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("localmind.sqlite");
        let writer = open_database(&path)?;
        migrate(&writer)?;
        let mode: String = writer.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        let timeout: i64 = writer.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        assert!(timeout >= 5_000, "busy timeout must be set, got {timeout}");

        // Hold a write transaction on connection A…
        writer.execute_batch("BEGIN IMMEDIATE")?;
        writer.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)
             ON CONFLICT(version) DO UPDATE SET applied_at = excluded.applied_at",
            rusqlite::params![9_000, "held"],
        )?;

        // …and write through connection B on another thread while A commits
        // after a delay. Without the busy timeout this insert fails
        // immediately with `database is locked`.
        let path_b = path.clone();
        let second = std::thread::spawn(move || -> Result<(), rusqlite::Error> {
            let cli = open_database(&path_b)?;
            cli.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)
                 ON CONFLICT(version) DO UPDATE SET applied_at = excluded.applied_at",
                rusqlite::params![9_001, "second-writer"],
            )?;
            Ok(())
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        writer.execute_batch("COMMIT")?;
        // The second writer must outwait the lock, not fail SQLITE_BUSY.
        second
            .join()
            .map_err(|_| "second-writer thread panicked")??;

        let rows: i64 = writer.query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version IN (9000, 9001)",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(rows, 2, "both writers' rows must land");
        Ok(())
    }

    #[test]
    fn concurrent_migrators_wait_and_converge() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("localmind.sqlite");

        // Build a real v2 database so the first migrator has non-idempotent
        // ALTER TABLE steps to apply.
        let setup = open_database(&path)?;
        let setup_tx = Transaction::new_unchecked(&setup, TransactionBehavior::Immediate)?;
        apply_v1(&setup_tx)?;
        apply_v2(&setup_tx)?;
        setup_tx.execute_batch("PRAGMA user_version = 2")?;
        setup_tx.commit()?;
        drop(setup);

        // Open both contenders before taking the lock: open_database also
        // verifies WAL mode, which is intentionally outside migration.
        let barrier = Arc::new(Barrier::new(3));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut migrators = Vec::new();
        for _ in 0..2 {
            let contender_path = path.clone();
            let contender_barrier = Arc::clone(&barrier);
            let contender_ready = ready_tx.clone();
            let contender_done = done_tx.clone();
            migrators.push(std::thread::spawn(move || -> Result<(), SchemaError> {
                let connection = open_database(&contender_path)?;
                let _ = contender_ready.send(());
                contender_barrier.wait();
                let result = migrate(&connection);
                let _ = contender_done.send(());
                result
            }));
        }
        drop(ready_tx);
        drop(done_tx);
        for _ in 0..2 {
            ready_rx.recv_timeout(std::time::Duration::from_secs(5))?;
        }

        // Keep both migrators behind an existing writer long enough to prove
        // they wait. Once released, one applies the ladder and the other must
        // re-read v12 under the same write lock instead of replaying v3.
        let lock_holder = open_database(&path)?;
        let held = Transaction::new_unchecked(&lock_holder, TransactionBehavior::Immediate)?;
        barrier.wait();
        assert!(matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        held.commit()?;

        for migrator in migrators {
            migrator.join().map_err(|_| "migrator thread panicked")??;
        }

        let final_connection = open_database(&path)?;
        let version: i32 =
            final_connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, DB_SCHEMA_VERSION);
        let canonical_hash_columns: i64 = final_connection.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('review_items') WHERE name = 'canonical_hash'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(canonical_hash_columns, 1);
        Ok(())
    }

    #[test]
    fn fresh_database_steps_to_current_version() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;

        let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, DB_SCHEMA_VERSION);
        connection.execute(
            "INSERT INTO memory_index(memory_id, path, scope, category, body, status, created_at)
             VALUES('m', 'p', 's', 'c', 'b', 'active', 'now')",
            [],
        )?;
        Ok(())
    }

    #[test]
    fn v7_adds_a_nullable_language_column() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;
        // A tagged row round-trips, and a row that omits the column is NULL —
        // proving the column is nullable so pre-v7 rows upgrade cleanly.
        connection.execute(
            "INSERT INTO memory_index(memory_id, path, scope, category, body, status, created_at, language)
             VALUES('tagged', 'p', 's', 'c', 'b', 'active', 'now', 'rust')",
            [],
        )?;
        connection.execute(
            "INSERT INTO memory_index(memory_id, path, scope, category, body, status, created_at)
             VALUES('untagged', 'p', 's', 'c', 'b', 'active', 'now')",
            [],
        )?;
        let tagged: Option<String> = connection.query_row(
            "SELECT language FROM memory_index WHERE memory_id = 'tagged'",
            [],
            |row| row.get(0),
        )?;
        let untagged: Option<String> = connection.query_row(
            "SELECT language FROM memory_index WHERE memory_id = 'untagged'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tagged.as_deref(), Some("rust"));
        assert_eq!(untagged, None);
        Ok(())
    }

    #[test]
    fn v8_adds_defaulted_usage_columns() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;
        // A row that omits the usage columns reads as zero-usage (hit_count
        // defaulted to 0, last_used_at NULL), proving pre-v8 rows upgrade clean.
        connection.execute(
            "INSERT INTO memory_index(memory_id, path, scope, category, body, status, created_at)
             VALUES('unused', 'p', 's', 'c', 'b', 'active', 'now')",
            [],
        )?;
        let (hits, last): (i64, Option<String>) = connection.query_row(
            "SELECT hit_count, last_used_at FROM memory_index WHERE memory_id = 'unused'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(hits, 0);
        assert_eq!(last, None);
        // And an explicit usage value round-trips.
        connection.execute(
            "INSERT INTO memory_index(memory_id, path, scope, category, body, status, created_at, hit_count, last_used_at)
             VALUES('used', 'p', 's', 'c', 'b', 'active', 'now', 3, 'then')",
            [],
        )?;
        let (hits, last): (i64, Option<String>) = connection.query_row(
            "SELECT hit_count, last_used_at FROM memory_index WHERE memory_id = 'used'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(hits, 3);
        assert_eq!(last.as_deref(), Some("then"));
        Ok(())
    }

    #[test]
    fn v9_adds_the_doc_chunk_table() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;
        connection.execute(
            "INSERT INTO doc_chunk(chunk_id, path, ordinal, heading, body, updated_at)
             VALUES('p#0', 'p', 0, 'H', 'text', 'now')",
            [],
        )?;
        // A chunk with no heading (NULL) is allowed too.
        connection.execute(
            "INSERT INTO doc_chunk(chunk_id, path, ordinal, body, updated_at)
             VALUES('p#1', 'p', 1, 'more', 'now')",
            [],
        )?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM doc_chunk", [], |row| row.get(0))?;
        assert_eq!(count, 2);
        Ok(())
    }

    #[test]
    fn v11_adds_retry_safe_proposal_receipts() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;
        connection.execute(
            "INSERT INTO proposal_receipts(proposal_id, fingerprint, survivor_id, created_at)
             VALUES('proposal', 'fingerprint', 'candidate', 'now')",
            [],
        )?;
        let survivor: String = connection.query_row(
            "SELECT survivor_id FROM proposal_receipts WHERE proposal_id = 'proposal'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(survivor, "candidate");
        Ok(())
    }

    #[test]
    fn v12_adds_a_nullable_merge_target() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;
        connection.execute(
            "INSERT INTO review_items(
                id, session_id, candidate_json, state, created_at, merge_target
             ) VALUES('source', 'session', '{}', 'merged', 'now', 'target')",
            [],
        )?;
        connection.execute(
            "INSERT INTO review_items(id, session_id, candidate_json, state, created_at)
             VALUES('historic', 'session', '{}', 'merged', 'now')",
            [],
        )?;
        let target: Option<String> = connection.query_row(
            "SELECT merge_target FROM review_items WHERE id = 'source'",
            [],
            |row| row.get(0),
        )?;
        let historic: Option<String> = connection.query_row(
            "SELECT merge_target FROM review_items WHERE id = 'historic'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(target.as_deref(), Some("target"));
        assert_eq!(historic, None);
        Ok(())
    }

    #[test]
    fn pre_versioned_database_upgrades_as_a_no_op() -> Result<(), Box<dyn std::error::Error>> {
        // A database created by a build that predates user_version: tables
        // exist, data exists, version is 0.
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            r#"
            CREATE TABLE audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                actor TEXT NOT NULL,
                subject TEXT NOT NULL,
                metadata_json TEXT NOT NULL,
                happened_at TEXT NOT NULL
            );
            INSERT INTO audit_events(kind, actor, subject, metadata_json, happened_at)
            VALUES('K', 'a', 's', '{}', 'now');
            "#,
        )?;

        migrate(&connection)?;

        let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, DB_SCHEMA_VERSION);
        let preserved: i64 =
            connection.query_row("SELECT COUNT(*) FROM audit_events", [], |row| row.get(0))?;
        assert_eq!(preserved, 1);
        // And the rest of the baseline appeared around the existing table.
        let fts: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memory_fts'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(fts, 1);
        Ok(())
    }

    #[test]
    fn migrate_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        migrate(&connection)?;
        migrate(&connection)?;
        Ok(())
    }

    #[test]
    fn newer_database_is_refused_with_a_typed_error() -> Result<(), Box<dyn std::error::Error>> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("PRAGMA user_version = 99")?;

        let error = migrate(&connection);

        assert!(matches!(
            error,
            Err(SchemaError::TooNew {
                found: 99,
                supported: DB_SCHEMA_VERSION
            })
        ));
        Ok(())
    }
}
