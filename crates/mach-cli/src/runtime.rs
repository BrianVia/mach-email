//! Shared bootstrap for the binary's subcommands. Resolves the DB path,
//! opens the SQLite store, and builds the `Dispatcher` against it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use mach_core::ids::AccountId;
use mach_core::ids::AccountScope;
use mach_core::UserConfig;
use mach_store::SqliteStore;

pub fn db_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "via", "mach")
        .context("could not resolve OS application-data directory")?;
    let dir = dirs.data_dir().to_path_buf();
    Ok(dir.join("mach.db"))
}

pub fn account_scope(account: Option<&str>) -> AccountScope {
    account
        .map(|email| AccountScope::One(AccountId::new(email)))
        .unwrap_or_default()
}

pub fn user_config() -> Result<UserConfig> {
    let dirs = ProjectDirs::from("com", "via", "mach")
        .context("could not resolve OS configuration directory")?;
    UserConfig::load(&dirs.config_dir().join("config.toml")).context("loading user config")
}

pub async fn open_store() -> Result<Arc<SqliteStore>> {
    let path = db_path()?;
    let pool = mach_store::open(&path)
        .with_context(|| format!("opening SQLite database at {}", path.display()))?;
    let store = Arc::new(SqliteStore::new(pool));
    let accounts = mach_gmail::credentials::load_all()?;
    if accounts.len() == 1 {
        store
            .claim_legacy_account(&AccountId::new(accounts[0].email.clone()))
            .await?;
    }
    Ok(store)
}
