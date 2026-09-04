use anyhow::Result;
use mach_mcp::Server;
use tracing::warn;

use crate::runtime;

pub async fn run(account: Option<&str>) -> Result<()> {
    let store = runtime::open_store().await?;
    let accounts = mach_gmail::credentials::load_all()?;
    // Try to spin up a Gmail client; if env creds aren't set the MCP
    // server still serves cached reads + queues mutations to the outbox
    // for the next sync.
    let body_fetchers = match mach_gmail::GmailAccountPool::from_stored_credentials(store.clone()) {
        Ok(pool) => std::sync::Arc::new(pool),
        Err(e) => {
            warn!(error = %e, "mcp: no gmail clients; cached-only mode");
            std::sync::Arc::new(mach_gmail::GmailAccountPool::default())
        }
    };
    let scope = runtime::account_scope(account);
    Server::new(store, body_fetchers, scope)
        .with_user_config(runtime::user_config()?)
        .with_accounts(accounts)
        .run()
        .await
}
