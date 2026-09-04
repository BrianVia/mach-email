use std::sync::Arc;

use anyhow::{Context, Result};
use mach_core::{ids::AccountId, MailStore};
use mach_store::SqliteStore;
use tracing::warn;

use crate::{BootstrapStats, GmailClient, OAuthConfig, StoredCredentials};

pub async fn add_account(store: Arc<SqliteStore>) -> Result<StoredCredentials> {
    let config = OAuthConfig::load().context("OAuth client credentials not configured")?;
    let credentials = crate::oauth::login(&config).await?;
    crate::credentials::save(&credentials)?;
    config.persist()?;
    if crate::credentials::load_all()?.len() == 1 {
        store
            .claim_legacy_account(&AccountId::new(credentials.email.clone()))
            .await?;
    }
    let client = GmailClient::from_credentials(config, credentials.clone())?;
    if let Err(error) = bootstrap_account(false, &credentials.email, client, store).await {
        warn!(account = credentials.email, error = %error, "login succeeded but mailbox bootstrap failed");
    }
    Ok(credentials)
}

pub async fn bootstrap_account(
    force: bool,
    email: &str,
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
) -> Result<Option<BootstrapStats>> {
    let account = AccountId::new(email);
    if !force && store.get_history_cursor(&account).await?.is_some() {
        return Ok(None);
    }
    crate::bootstrap(client, store).await.map(Some)
}
