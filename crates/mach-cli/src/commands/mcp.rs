use std::sync::Arc;

use anyhow::Result;
use mach_mcp::Server;
use tracing::warn;

use crate::runtime;

pub async fn run() -> Result<()> {
    let store = runtime::open_store()?;
    // Try to spin up a Gmail client; if env creds aren't set the MCP
    // server still serves cached reads + queues mutations to the outbox
    // for the next sync.
    let body_fetcher = match build_body_fetcher(store.clone()).await {
        Ok(b) => Some(Arc::new(b)),
        Err(e) => {
            warn!(error = %e, "mcp: no gmail client; cached-only mode");
            None
        }
    };
    Server::new(store, body_fetcher).run().await
}

async fn build_body_fetcher(
    store: Arc<mach_store::SqliteStore>,
) -> Result<mach_gmail::BodyFetcher> {
    let config = mach_gmail::config::OAuthConfig::from_env()?;
    let client = mach_gmail::GmailClient::from_stored_credentials(config)?;
    Ok(mach_gmail::BodyFetcher::new(client, store))
}
