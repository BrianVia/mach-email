//! Drains the local outbox to Gmail.
//!
//! The dispatcher writes optimistic local mutations + an outbox entry. This
//! worker reads pending entries in FIFO order and rams them through the
//! corresponding Gmail endpoint, marking each `done` on success or
//! `failed` with the error string on persistent failure.
//!
//! Retry policy is lazy: we don't loop with backoff inside the worker.
//! `drain_once` makes a single pass; currently `mach sync` is the caller.
//!
//! ECHO SUPPRESSION (planned, not yet implemented): when the sync engine
//! receives `historyAdded`/`labelRemoved` events whose net effect matches
//! one of our recent (<60s) own `op_id`s, drop the event to prevent a
//! UI flicker. That logic lives in the incremental sync path, not here.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mach_core::store::{MailStore, OutboxOp, OutboxOpKind};
use mach_store::SqliteStore;
use tracing::{debug, info, warn};

use crate::client::GmailClient;

pub struct OutboxWorker {
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DrainStats {
    pub processed: usize,
    pub failed: usize,
}

impl OutboxWorker {
    pub fn new(client: Arc<GmailClient>, store: Arc<SqliteStore>) -> Self {
        Self { client, store }
    }

    /// Drain up to `max` pending ops. Returns stats. Idempotent: marking
    /// an op `done` is a SQL UPDATE so duplicate calls are harmless.
    pub async fn drain_once(&self, max: u32) -> Result<DrainStats> {
        let pending = self
            .store
            .drain_pending_outbox(max)
            .await
            .context("draining pending outbox")?;
        if pending.is_empty() {
            return Ok(DrainStats::default());
        }
        info!(count = pending.len(), "draining outbox");
        let mut stats = DrainStats::default();
        for op in pending {
            match self.execute_op(&op).await {
                Ok(()) => {
                    stats.processed += 1;
                    self.store
                        .mark_outbox_done(op.id)
                        .await
                        .context("marking op done")?;
                    debug!(op_id = %op.op_id, "outbox op done");
                }
                Err(e) => {
                    stats.failed += 1;
                    warn!(op_id = %op.op_id, error = %e, "outbox op failed");
                    if let Err(e2) = self.store.mark_outbox_failed(op.id, &e.to_string()).await {
                        warn!(error = %e2, "marking op failed also failed");
                    }
                }
            }
        }
        Ok(stats)
    }

    async fn execute_op(&self, op: &OutboxOp) -> Result<()> {
        match &op.kind {
            OutboxOpKind::ModifyLabels {
                thread_ids,
                add,
                remove,
            } => {
                let add: Vec<String> = add.iter().map(|l| l.as_str().to_string()).collect();
                let remove: Vec<String> = remove.iter().map(|l| l.as_str().to_string()).collect();
                for tid in thread_ids {
                    self.client
                        .modify_thread(tid.as_str(), &add, &remove)
                        .await
                        .with_context(|| format!("modify_thread {tid}"))?;
                }
                Ok(())
            }
            OutboxOpKind::Trash { thread_ids } => {
                for tid in thread_ids {
                    self.client
                        .trash_thread(tid.as_str())
                        .await
                        .with_context(|| format!("trash_thread {tid}"))?;
                }
                Ok(())
            }
            OutboxOpKind::SendDraft { draft_id } => {
                // Look up the draft locally, build the MIME, send.
                let draft = self
                    .store
                    .get_draft(draft_id)
                    .await?
                    .ok_or_else(|| anyhow!("draft {draft_id} not found"))?;
                let raw = build_mime_raw(&draft)?;
                let _sent_id = self
                    .client
                    .send_raw(&raw, draft.thread_id.as_ref().map(|t| t.as_str()))
                    .await
                    .context("send_raw")?;
                // Drop the local draft once Gmail accepted it.
                self.store.delete_draft_local(draft_id).await?;
                Ok(())
            }
            OutboxOpKind::SaveDraft { .. } | OutboxOpKind::DeleteDraft { .. } => {
                // v1: drafts only live locally. Remote draft sync is a v1.5
                // item. Treat as no-op so the outbox doesn't pile up.
                Ok(())
            }
        }
    }
}

/// Build an RFC 2822 message + URL-safe base64url-encode it for the
/// `messages.send` endpoint. We use `mail-builder` for headers + body.
fn build_mime_raw(d: &mach_core::store::Draft) -> Result<String> {
    use base64::{
        alphabet,
        engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig},
        Engine as _,
    };
    use mail_builder::MessageBuilder;

    let mut b = MessageBuilder::new();
    if !d.to.is_empty() {
        b = b.to(d.to.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    if !d.cc.is_empty() {
        b = b.cc(d.cc.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    if !d.bcc.is_empty() {
        b = b.bcc(d.bcc.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    b = b.subject(&d.subject).text_body(&d.body_md);
    if let Some(reply_to) = &d.in_reply_to_message_id {
        b = b.header(
            "In-Reply-To",
            mail_builder::headers::raw::Raw::new(format!("<{}>", reply_to.as_str())),
        );
    }

    let mime = b.write_to_string().context("building MIME")?;

    // Same encoding the rest of the codebase uses for Gmail bodies.
    const B64URL: GeneralPurpose = GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    Ok(B64URL.encode(mime.as_bytes()))
}
