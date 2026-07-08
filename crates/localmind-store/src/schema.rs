//! Versioned schema migrations for the shared project database
//! (`.localmind/localmind.sqlite`).
//!
//! Every component that opens the database runs [`migrate`] first. The
//! stepper reads `PRAGMA user_version`, applies each missing step inside one
//! transaction, and stamps the new version on commit. Version 1 is the
//! consolidated baseline of everything that previously ran as ad-hoc
//! `CREATE TABLE IF NOT EXISTS` batches, so upgrading a pre-versioned
//! database is a verified no-op: the guards keep existing tables and rows
//! untouched.
//!
//! The graph store's payload format is versioned separately through
//! `graph_meta` (`GRAPH_FORMAT_VERSION`); this module owns the *database*
//! schema lifecycle.

use rusqlite::Connection;
use thiserror::Error;

/// Highest schema version this build understands.
pub(crate) const DB_SCHEMA_VERSION: i32 = 9;

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

    if current > DB_SCHEMA_VERSION {
        return Err(SchemaError::TooNew {
            found: current,
            supported: DB_SCHEMA_VERSION,
        });
    }
    if current == DB_SCHEMA_VERSION {
        return Ok(());
    }

    let tx = connection
        .unchecked_transaction()
        .map_err(SchemaError::Sqlite)?;
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
    tx.execute_batch(&format!("PRAGMA user_version = {DB_SCHEMA_VERSION}"))
        .map_err(SchemaError::Sqlite)?;
    tx.commit().map_err(SchemaError::Sqlite)?;
    Ok(())
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
    use super::{migrate, open_database, SchemaError, DB_SCHEMA_VERSION};
    use rusqlite::Connection;

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
