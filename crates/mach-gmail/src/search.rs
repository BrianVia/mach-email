//! Remote Gmail search used as the completeness backstop for local FTS.
//!
//! Gmail owns the full-mailbox index. Matching thread metadata is cached in
//! SQLite so opening a remote-only hit uses the same local projection and body
//! hydration path as every other thread.

use std::sync::Arc;

use anyhow::{Context, Result};
use mach_core::{
    ids::{AccountId, AccountScope, ThreadId},
    store::{MailStore, ThreadSummary},
};
use mach_store::SqliteStore;

use crate::{client::GmailClient, sync::fetch_and_upsert_threads};

pub(crate) async fn search_account(
    account: &AccountId,
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
    query: &str,
    limit: u32,
) -> Result<Vec<ThreadSummary>> {
    let page = client
        .list_threads_page(query, None)
        .await
        .with_context(|| format!("searching Gmail account {account}"))?;
    let ids: Vec<String> = page
        .threads
        .unwrap_or_default()
        .into_iter()
        .map(|thread| thread.id)
        .take(limit as usize)
        .collect();

    let remote_threads = fetch_and_upsert_threads(client, store.clone(), account, ids)
        .await?
        .threads;

    let ids: Vec<ThreadId> = remote_threads
        .iter()
        .map(|thread| ThreadId::new(thread.id.clone()))
        .collect();
    let scope = AccountScope::One(account.clone());
    let mut summaries = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(summary) = store.get_thread(&scope, &id).await? {
            summaries.push(summary);
        }
    }
    Ok(summaries)
}
