//! Lazy full-body fetcher.
//!
//! Bootstrap fetches `format=metadata` — fast, no bodies. The first time a
//! thread is opened, this service detects the gap and pulls `format=full`,
//! decodes the MIME tree, persists `body_plain`/`body_html`, flips the
//! `fetched_full` flag. Idempotent: subsequent calls are no-ops.

use std::sync::Arc;

use anyhow::{Context, Result};
use mach_core::{ids::ThreadId, store::MailStore};
use mach_store::{MessageBodyUpdate, SqliteStore};
use tracing::{debug, info};

use crate::body::{self, ParsedBody};
use crate::client::{GmailClient, RemoteMessage};

pub struct BodyFetcher {
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Nothing to do — all messages already have full bodies.
    AlreadyCached,
    /// Fetched and persisted N messages.
    Fetched(usize),
}

impl BodyFetcher {
    pub fn new(client: Arc<GmailClient>, store: Arc<SqliteStore>) -> Self {
        Self { client, store }
    }

    pub fn client(&self) -> &Arc<GmailClient> {
        &self.client
    }

    /// Check the local cache; if any message in the thread lacks a full body,
    /// fetch `format=full`, walk every message's MIME tree, and persist.
    pub async fn fetch_if_needed(&self, thread_id: &ThreadId) -> Result<FetchOutcome> {
        let messages = self
            .store
            .list_messages_in_thread(thread_id)
            .await
            .context("listing messages from cache")?;

        if messages.is_empty() {
            return Ok(FetchOutcome::AlreadyCached);
        }
        if messages.iter().all(|m| m.fetched_full) {
            debug!(thread = thread_id.as_str(), "all messages cached");
            return Ok(FetchOutcome::AlreadyCached);
        }

        info!(thread = thread_id.as_str(), "fetching full bodies");
        let full = self
            .client
            .get_thread_full(thread_id.as_str())
            .await
            .with_context(|| format!("get_thread_full({thread_id})"))?;

        let updates: Vec<MessageBodyUpdate> = full
            .messages
            .iter()
            .map(parse_message_into_update)
            .collect();

        let touched = self
            .store
            .update_message_bodies(updates)
            .await
            .context("persisting full bodies")?;
        info!(touched, "body backfill complete");
        Ok(FetchOutcome::Fetched(touched))
    }
}

fn parse_message_into_update(m: &RemoteMessage) -> MessageBodyUpdate {
    let parsed: ParsedBody = m.payload.as_ref().map(body::extract).unwrap_or_default();

    // Prefer raw plain. Fall back to html→text via html2text inside
    // ParsedBody::effective_plain. We persist both: body_plain feeds the
    // FTS5 index + TUI; body_html sticks around for the desktop renderer.
    let body_plain = parsed.effective_plain();
    let body_html = parsed.html.clone();
    let inline_images_json = if parsed.inline_images.is_empty() {
        None
    } else {
        serde_json::to_string(&parsed.inline_images).ok()
    };

    MessageBodyUpdate {
        id: m.id.clone(),
        body_plain,
        body_html,
        inline_images_json,
    }
}
