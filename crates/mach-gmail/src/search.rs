//! Remote Gmail search used as the completeness backstop for local FTS.
//!
//! Gmail owns the full-mailbox index. Matching thread metadata is cached in
//! SQLite so opening a remote-only hit uses the same local projection and body
//! hydration path as every other thread.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use mach_core::{
    ids::{AccountId, AccountScope, ThreadId},
    store::{MailStore, ThreadSummary},
};
use mach_store::SqliteStore;
use tracing::warn;

use crate::{client::GmailClient, sync::remote_thread_to_upsert};

const SEARCH_FETCH_CONCURRENCY: usize = 10;

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

    let mut remote_threads = Vec::with_capacity(ids.len());
    let mut fetches = stream::iter(ids)
        .map(|id| {
            let client = client.clone();
            async move { client.get_thread_metadata(&id).await }
        })
        .buffer_unordered(SEARCH_FETCH_CONCURRENCY);
    while let Some(result) = fetches.next().await {
        match result {
            Ok(thread) => remote_threads.push(thread),
            Err(error) => warn!(account = %account, %error, "remote search hit fetch failed"),
        }
    }

    let ids: Vec<ThreadId> = remote_threads
        .iter()
        .map(|thread| ThreadId::new(thread.id.clone()))
        .collect();
    let upserts = remote_threads.iter().map(remote_thread_to_upsert).collect();
    store.upsert_threads(account, upserts).await?;

    let scope = AccountScope::One(account.clone());
    let mut summaries = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(summary) = store.get_thread(&scope, &id).await? {
            summaries.push(summary);
        }
    }
    Ok(summaries)
}
