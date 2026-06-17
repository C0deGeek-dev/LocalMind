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
pub(crate) const DB_SCHEMA_VERSION: i32 = 3;

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
    use super::{migrate, SchemaError, DB_SCHEMA_VERSION};
    use rusqlite::Connection;

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
