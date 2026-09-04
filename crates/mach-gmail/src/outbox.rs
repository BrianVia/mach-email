//! Drains the local outbox to Gmail.
//!
//! The dispatcher writes optimistic local mutations + an outbox entry. This
//! worker reads pending entries in FIFO order and rams them through the
//! corresponding Gmail endpoint, marking each `done` on success. The store
//! owns retries for every operation kind: failures back off for 1m, 5m, 30m,
//! and 2h, then dead-letter on the fifth failed attempt.
//!
//! `drain_once` makes one pass over operations currently due; later sync ticks
//! retry them after the store's durable `next_attempt_at` deadline.
//!
//! Echoes of recently completed label mutations are suppressed by
//! [`crate::sync::incremental_sync`] to avoid redundant refetches and UI flicker.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mach_core::ids::{AccountId, AccountScope};
use mach_core::store::{MailStore, Message, OutboxOp, OutboxOpKind};
use mach_store::SqliteStore;
use tracing::{debug, info, warn};

use crate::client::GmailClient;

pub struct OutboxWorker {
    account: AccountId,
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DrainStats {
    pub processed: usize,
    pub failed: usize,
}

impl OutboxWorker {
    pub fn new(account: AccountId, client: Arc<GmailClient>, store: Arc<SqliteStore>) -> Self {
        Self {
            account,
            client,
            store,
        }
    }

    /// Drain up to `max` pending ops. Returns stats. Idempotent: marking
    /// an op `done` is a SQL UPDATE so duplicate calls are harmless.
    pub async fn drain_once(&self, max: u32) -> Result<DrainStats> {
        let pending = self
            .store
            .drain_pending_outbox(&self.account, max)
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
                    if !matches!(op.kind, OutboxOpKind::SendDraft { .. }) {
                        self.store
                            .mark_outbox_done(op.id)
                            .await
                            .context("marking op done")?;
                    }
                    debug!(op_id = %op.op_id, "outbox op done");
                }
                Err(e) => {
                    stats.failed += 1;
                    // `{e:#}` keeps the whole context chain — the root cause
                    // (e.g. the HTTP status + body) lives at the bottom.
                    let chain = format!("{e:#}");
                    warn!(op_id = %op.op_id, error = %chain, "outbox op failed");
                    if let Err(e2) = self.store.mark_outbox_failed(op.id, &chain).await {
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
                    .get_draft(&self.account, draft_id)
                    .await?
                    .ok_or_else(|| anyhow!("draft {draft_id} not found"))?;
                let source = match &draft.in_reply_to_message_id {
                    Some(message_id) => {
                        let source = self
                            .store
                            .get_message(&AccountScope::One(self.account.clone()), message_id)
                            .await?;
                        if source.is_none() {
                            warn!(
                                draft_id = %draft_id,
                                source_message_id = %message_id,
                                "source message for draft is missing; sending without RFC threading headers"
                            );
                        }
                        source
                    }
                    None => None,
                };
                let raw = build_mime_raw(&draft, source.as_ref())?;
                let _sent_id = self
                    .client
                    .send_raw(&raw, draft.thread_id.as_ref().map(|t| t.as_str()))
                    .await
                    .context("send_raw")?;
                self.store
                    .complete_send(op.id, &self.account, draft_id)
                    .await?;
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

fn split_recipient(value: &str) -> (Option<&str>, &str) {
    let trimmed = value.trim();
    let Some((raw_name, raw_email)) = trimmed.split_once('<') else {
        return (None, trimmed);
    };
    let Some(raw_email) = raw_email.strip_suffix('>') else {
        return (None, trimmed);
    };
    if raw_name.contains('<') || raw_email.is_empty() || raw_email.contains('>') {
        return (None, trimmed);
    }

    let raw_name = raw_name.trim();
    let name = if raw_name.starts_with('"') || raw_name.ends_with('"') {
        let Some(name) = raw_name
            .strip_prefix('"')
            .and_then(|name| name.strip_suffix('"'))
        else {
            return (None, trimmed);
        };
        name
    } else {
        raw_name
    }
    .trim();
    if name.contains('"') {
        return (None, trimmed);
    }

    (
        if name.is_empty() { None } else { Some(name) },
        raw_email.trim(),
    )
}

/// Build an RFC 2822 message + URL-safe base64url-encode it for the
/// `messages.send` endpoint. We use `mail-builder` for headers + body.
fn build_mime_raw(d: &mach_core::store::Draft, source: Option<&Message>) -> Result<String> {
    use base64::{
        alphabet,
        engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig},
        Engine as _,
    };
    use mail_builder::{headers::address::Address, MessageBuilder};

    let mut b = MessageBuilder::new().from(d.account_id.as_str());
    if !d.to.is_empty() {
        b = b.to(d
            .to
            .iter()
            .map(|recipient| {
                let (name, email) = split_recipient(recipient);
                Address::new_address(name, email)
            })
            .collect::<Vec<_>>());
    }
    if !d.cc.is_empty() {
        b = b.cc(d
            .cc
            .iter()
            .map(|recipient| {
                let (name, email) = split_recipient(recipient);
                Address::new_address(name, email)
            })
            .collect::<Vec<_>>());
    }
    if !d.bcc.is_empty() {
        b = b.bcc(
            d.bcc
                .iter()
                .map(|recipient| {
                    let (name, email) = split_recipient(recipient);
                    Address::new_address(name, email)
                })
                .collect::<Vec<_>>(),
        );
    }
    b = b.subject(&d.subject).text_body(&d.body_md);
    if let Some(ics) = &d.calendar_reply_ics {
        b = b.attachment(
            "text/calendar; method=REPLY; charset=utf-8",
            "invite.ics",
            ics.as_bytes(),
        );
    }
    if let Some(headers) = source.and_then(|message| message.headers.as_ref()) {
        if let Some(message_id) = headers.message_id.as_deref() {
            let references = format!(
                "{} {}",
                headers.references.as_deref().unwrap_or_default(),
                message_id
            );
            b = b
                .header(
                    "In-Reply-To",
                    mail_builder::headers::raw::Raw::new(message_id),
                )
                .header(
                    "References",
                    mail_builder::headers::raw::Raw::new(references.trim().to_string()),
                );
        }
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

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::URL_SAFE, Engine as _};
    use chrono::Utc;
    use mach_core::ids::{DraftId, LabelId, MessageId, ThreadId};
    use mach_core::store::{Draft, MessageHeaders};

    use super::*;

    fn draft() -> Draft {
        Draft {
            account_id: AccountId::new("me@example.com"),
            id: DraftId::new("draft"),
            gmail_draft_id: None,
            thread_id: Some(ThreadId::new("gmail-thread")),
            in_reply_to_message_id: Some(MessageId::new("gmail-message")),
            to: vec!["recipient@example.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Reply".into(),
            body_md: "Hello".into(),
            calendar_reply_ics: None,
            updated_at: Utc::now(),
        }
    }

    fn source(headers: Option<MessageHeaders>) -> Message {
        Message {
            account_id: AccountId::new("me@example.com"),
            id: MessageId::new("gmail-message"),
            thread_id: ThreadId::new("gmail-thread"),
            from: "sender@example.com".into(),
            to: vec!["me@example.com".into()],
            cc: vec![],
            subject: "Original".into(),
            snippet: "Original body".into(),
            internal_date: Utc::now(),
            body_plain: Some("Original body".into()),
            body_html: None,
            calendar: None,
            headers,
            label_ids: vec![LabelId::new("INBOX")],
            fetched_full: true,
            inline_images: vec![],
        }
    }

    fn decoded_mime(draft: &Draft, source: Option<&Message>) -> String {
        let raw = build_mime_raw(draft, source).unwrap();
        String::from_utf8(URL_SAFE.decode(raw).unwrap()).unwrap()
    }

    #[test]
    fn mime_threads_from_stored_rfc_headers() {
        let source = source(Some(MessageHeaders {
            message_id: Some("<source@example.com>".into()),
            references: Some("<root@example.com> <parent@example.com>".into()),
            ..MessageHeaders::default()
        }));
        let mime = decoded_mime(&draft(), Some(&source));

        assert!(mime.contains("From: <me@example.com>"));
        assert!(mime.contains("In-Reply-To: <source@example.com>"));
        assert!(mime
            .contains("References: <root@example.com> <parent@example.com> <source@example.com>"));
        assert!(!mime.contains("<gmail-message>"));
    }

    #[test]
    fn mime_omits_threading_headers_when_stored_headers_are_absent() {
        let source = source(None);
        let mime = decoded_mime(&draft(), Some(&source));

        assert!(!mime.contains("In-Reply-To:"));
        assert!(!mime.contains("References:"));
    }

    #[test]
    fn mime_includes_calendar_reply_part() {
        let mut draft = draft();
        draft.calendar_reply_ics =
            Some("BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into());
        let mime = decoded_mime(&draft, None);
        assert!(mime.contains("Content-Type: text/calendar;"));
        assert!(mime.contains("method=REPLY"));
    }

    #[test]
    fn mime_does_not_nest_quoted_email_display_name() {
        assert_eq!(
            split_recipient("\"jane@x.com\" <jane@x.com>"),
            (Some("jane@x.com"), "jane@x.com")
        );

        let mut draft = draft();
        draft.to = vec!["\"jane@x.com\" <jane@x.com>".into()];

        let mime = decoded_mime(&draft, None);

        assert!(!mime.contains("<\""), "malformed To header: {mime}");
        assert_eq!(mime.matches("<jane@x.com>").count(), 1);
    }

    #[test]
    fn mime_preserves_recipient_display_name() {
        let mut draft = draft();
        draft.to = vec!["Jane Doe <jane@x.com>".into()];

        let mime = decoded_mime(&draft, None);

        assert!(
            mime.contains("To: Jane Doe <jane@x.com>")
                || mime.contains("To: \"Jane Doe\" <jane@x.com>"),
            "unexpected To header: {mime}"
        );
        assert_eq!(mime.matches("<jane@x.com>").count(), 1);
    }

    #[test]
    fn mime_preserves_bare_recipient() {
        let mut draft = draft();
        draft.to = vec!["jane@x.com".into()];

        let mime = decoded_mime(&draft, None);

        assert!(mime.contains("To: <jane@x.com>"));
    }
}
