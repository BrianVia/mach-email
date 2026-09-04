//! Shared bootstrap for the binary's subcommands. Resolves the DB path,
//! opens the SQLite store, and builds the `Dispatcher` against it.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
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

pub fn resolve_account(
    input: Option<&str>,
    config: &UserConfig,
    known: &[AccountId],
) -> Result<Option<AccountId>> {
    let Some(input) = input else { return Ok(None) };
    if let Some(account) = config.resolve_account(input, known) {
        return Ok(Some(account));
    }
    let choices = known
        .iter()
        .map(|account| {
            let email = account.as_str();
            let nickname = config.account_label(email);
            if nickname == email {
                email.to_string()
            } else {
                format!("{email} ({nickname})")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    bail!("unknown account {input}; known accounts: {choices}")
}

pub fn selected_account(input: Option<&str>) -> Result<Option<AccountId>> {
    let config = user_config()?;
    let known = known_accounts()?;
    resolve_account(input, &config, &known)
}

pub fn known_accounts() -> Result<Vec<AccountId>> {
    Ok(mach_gmail::credentials::load_all()?
        .into_iter()
        .map(|credentials| AccountId::new(credentials.email))
        .collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_cli_account_by_email_or_nickname() {
        let config = UserConfig {
            accounts: [
                ("work@example.com".into(), "Work".into()),
                ("home@example.com".into(), "Home".into()),
            ]
            .into(),
            ..UserConfig::default()
        };
        let known = [
            AccountId::new("work@example.com"),
            AccountId::new("home@example.com"),
        ];

        assert_eq!(
            resolve_account(Some("work"), &config, &known).unwrap(),
            Some(known[0].clone())
        );
        assert_eq!(
            resolve_account(Some("home@example.com"), &config, &known).unwrap(),
            Some(known[1].clone())
        );
        assert!(resolve_account(Some("missing"), &config, &known)
            .unwrap_err()
            .to_string()
            .contains("work@example.com (Work)"));
    }
}
