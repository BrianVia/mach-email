//! SQLite + FTS5 storage layer. Implements `mach_core::MailStore`.
//!
//! Migrations live in `crates/mach-store/migrations/` and are embedded at
//! compile time via `refinery`. Apply them with [`open`] or [`open_in_memory`].

use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use thiserror::Error;

mod store;

pub use store::{
    DueSend, DueSnooze, LabelUpsert, MessageBodyUpdate, MessageUpsert, SqliteStore, ThreadUpsert,
};

mod embedded {
    refinery::embed_migrations!("migrations");
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("pool: {0}")]
    Pool(#[from] r2d2::Error),
    #[error("migration: {0}")]
    Migration(#[from] refinery::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type DbPool = Pool<SqliteConnectionManager>;

/// Open the SQLite database at `path`, creating directories as needed,
/// applying pragmas to every pooled connection, and running migrations.
pub fn open(path: impl AsRef<Path>) -> Result<DbPool, StoreError> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let manager = SqliteConnectionManager::file(&path).with_init(apply_pragmas);
    let pool = Pool::builder().max_size(8).build(manager)?;
    run_migrations(&pool)?;
    Ok(pool)
}

/// In-memory database — used by the test suite. Limited to one connection
/// because each `:memory:` connection gets its own database.
pub fn open_in_memory() -> Result<DbPool, StoreError> {
    let manager = SqliteConnectionManager::memory().with_init(apply_pragmas_memory);
    let pool = Pool::builder().max_size(1).build(manager)?;
    run_migrations(&pool)?;
    Ok(pool)
}

fn apply_pragmas(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 268_435_456i64)?;
    conn.pragma_update(None, "cache_size", -65_536i64)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn apply_pragmas_memory(conn: &mut Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn run_migrations(pool: &DbPool) -> Result<(), StoreError> {
    let mut conn = pool.get()?;
    embedded::migrations::runner().run(&mut *conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_expected_tables() {
        let pool = open_in_memory().unwrap();
        let conn = pool.get().unwrap();
        let table_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'refinery_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // threads, messages, labels, attachments, drafts, send_later, outbox,
        // sync_state, meta + the 5 internal messages_fts_* tables.
        assert!(table_count >= 9, "expected 9+ tables, got {table_count}");

        let has_fts: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='messages_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_fts, 1, "messages_fts virtual table missing");
    }
}
