//! Shared bootstrap for the binary's subcommands. Resolves the DB path,
//! opens the SQLite store, and builds the `Dispatcher` against it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use mach_store::SqliteStore;

pub fn db_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "via", "mach")
        .context("could not resolve OS application-data directory")?;
    let dir = dirs.data_dir().to_path_buf();
    Ok(dir.join("mach.db"))
}

pub fn open_store() -> Result<Arc<SqliteStore>> {
    let path = db_path()?;
    let pool = mach_store::open(&path)
        .with_context(|| format!("opening SQLite database at {}", path.display()))?;
    Ok(Arc::new(SqliteStore::new(pool)))
}
