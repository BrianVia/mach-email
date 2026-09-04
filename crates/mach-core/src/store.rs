use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::action::OpId;
use crate::error::CoreResult;
use crate::ids::{AccountId, AccountScope, DraftId, LabelId, MessageId, ThreadId};

/// A row in the inbox list. Denormalized for render speed — the projection
/// the TUI actually paints comes straight from this struct, no joins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub account_id: AccountId,
    pub id: ThreadId,
    pub subject: String,
    pub snippet: String,
    pub participants: Vec<String>,
    pub last_message_at: DateTime<Utc>,
    pub message_count: u32,
    pub unread: bool,
    pub starred: bool,
    pub label_ids: Vec<LabelId>,
}

/// Whether an inbox thread needs a response from its account owner.
pub fn is_awaiting_reply(thread: &ThreadSummary, last_message: Option<&Message>) -> bool {
    thread
        .label_ids
        .iter()
        .any(|label| label.as_str() == "INBOX")
        && (thread.unread || thread.starred)
        && last_message.is_some_and(|message| {
            crate::compose::normalized_addr_spec(&message.from)
                != crate::compose::normalized_addr_spec(thread.account_id.as_str())
        })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub account_id: AccountId,
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub internal_date: DateTime<Utc>,
    pub body_plain: Option<String>,
    pub body_html: Option<String>,
    #[serde(default)]
    pub headers: Option<MessageHeaders>,
    pub label_ids: Vec<LabelId>,
    /// `true` once `format=full` has been fetched and `body_plain`/`body_html`
    /// reflect the message contents. Bootstrap leaves this `false`.
    #[serde(default)]
    pub fetched_full: bool,
    /// Inline image references for `body_html`. Each is a tuple of
    /// `(content_id, attachment_id, mime_type, ...)`. The desktop renderer
    /// rewrites `<img src="cid:...">` against this map.
    #[serde(default)]
    pub inline_images: Vec<InlineImageRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageHeaders {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_unsubscribe: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_unsubscribe_post: Option<String>,
}

/// Mirrors `mach_gmail::body::InlineImageRef` but lives in `mach-core` so
/// store consumers don't have to depend on the Gmail crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineImageRow {
    pub content_id: String,
    pub attachment_id: String,
    pub mime_type: String,
    pub filename: Option<String>,
    pub size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub account_id: AccountId,
    pub id: LabelId,
    pub name: String,
    pub system: bool,
    pub color: Option<String>,
    pub unread_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub account_id: AccountId,
    pub id: DraftId,
    pub gmail_draft_id: Option<String>,
    pub thread_id: Option<ThreadId>,
    pub in_reply_to_message_id: Option<MessageId>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body_md: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledSend {
    pub send_later_id: String,
    pub draft_id: DraftId,
    pub send_at: DateTime<Utc>,
    pub subject: String,
    pub to: Vec<String>,
    pub account_id: AccountId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxOpKind {
    ModifyLabels {
        thread_ids: Vec<ThreadId>,
        add: Vec<LabelId>,
        remove: Vec<LabelId>,
    },
    Trash {
        thread_ids: Vec<ThreadId>,
    },
    SendDraft {
        draft_id: DraftId,
    },
    SaveDraft {
        draft_id: DraftId,
    },
    DeleteDraft {
        draft_id: DraftId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxOp {
    pub id: i64,
    pub account_id: AccountId,
    pub op_id: OpId,
    pub kind: OutboxOpKind,
    pub created_at: DateTime<Utc>,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutboxSummary {
    pub pending: u32,
    pub failed: u32,
    pub last_error: Option<String>,
}

/// Local persistence. The dispatcher writes here optimistically; the sync
/// engine reads pending outbox ops and pushes them upstream.
#[async_trait]
pub trait MailStore: Send + Sync {
    async fn get_thread(
        &self,
        scope: &AccountScope,
        id: &ThreadId,
    ) -> CoreResult<Option<ThreadSummary>>;
    async fn list_threads_in_label(
        &self,
        scope: &AccountScope,
        label: &LabelId,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>>;
    async fn list_messages_in_thread(
        &self,
        scope: &AccountScope,
        id: &ThreadId,
    ) -> CoreResult<Vec<Message>>;
    async fn get_message(
        &self,
        scope: &AccountScope,
        id: &MessageId,
    ) -> CoreResult<Option<Message>>;
    async fn search_threads(
        &self,
        scope: &AccountScope,
        query: &str,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>>;

    /// Apply a thread mutation to the local projection and durably enqueue
    /// the matching remote operation as one atomic store transaction.
    ///
    /// Implementations must reject non-thread operations. Returning success
    /// guarantees that the optimistic state and its recovery record either
    /// both exist or neither exists.
    async fn apply_thread_mutation(
        &self,
        scope: &AccountScope,
        op_id: &OpId,
        kind: &OutboxOpKind,
    ) -> CoreResult<i64>;

    async fn list_labels(&self, scope: &AccountScope) -> CoreResult<Vec<Label>>;

    async fn find_draft(&self, scope: &AccountScope, id: &DraftId) -> CoreResult<Option<Draft>>;
    async fn get_draft(&self, account: &AccountId, id: &DraftId) -> CoreResult<Option<Draft>>;
    async fn save_draft_local(&self, draft: &Draft) -> CoreResult<()>;
    async fn delete_draft_local(&self, account: &AccountId, id: &DraftId) -> CoreResult<()>;
    async fn queue_draft_send(
        &self,
        account: &AccountId,
        draft_id: &DraftId,
        op_id: &OpId,
    ) -> CoreResult<i64>;
    async fn schedule_send(
        &self,
        account: &AccountId,
        draft_id: &DraftId,
        send_at: DateTime<Utc>,
    ) -> CoreResult<()>;
    async fn list_scheduled(&self, scope: &AccountScope) -> CoreResult<Vec<ScheduledSend>>;
    async fn cancel_scheduled(&self, account: &AccountId, send_later_id: &str) -> CoreResult<()>;
    async fn complete_send(
        &self,
        outbox_row_id: i64,
        account: &AccountId,
        draft_id: &DraftId,
    ) -> CoreResult<()>;

    async fn enqueue_outbox(
        &self,
        account: &AccountId,
        op_id: &OpId,
        kind: &OutboxOpKind,
    ) -> CoreResult<i64>;
    async fn drain_pending_outbox(
        &self,
        account: &AccountId,
        max: u32,
    ) -> CoreResult<Vec<OutboxOp>>;
    async fn mark_outbox_done(&self, id: i64) -> CoreResult<()>;
    async fn mark_outbox_failed(&self, id: i64, error: &str) -> CoreResult<()>;
    async fn outbox_summary(&self, scope: &AccountScope) -> CoreResult<OutboxSummary>;
    async fn retry_failed_outbox(&self, account: &AccountId) -> CoreResult<u32>;

    /// Currently-stored sync cursor for the active account, if any.
    async fn get_history_cursor(&self, account: &AccountId) -> CoreResult<Option<u64>>;
    async fn set_history_cursor(&self, account: &AccountId, cursor: u64) -> CoreResult<()>;
}

/// Talks to Gmail. Kept as a trait so we can mock the network in tests
/// (especially for the sync state machine, where 404-on-stale-historyId is
/// the path most likely to break in production).
#[async_trait]
pub trait MailRemote: Send + Sync {
    async fn get_profile_history_id(&self) -> CoreResult<u64>;
    async fn list_threads(&self, query: &str, limit: u32) -> CoreResult<Vec<ThreadSummary>>;
    async fn get_thread(&self, id: &ThreadId) -> CoreResult<(ThreadSummary, Vec<Message>)>;

    async fn modify_labels(
        &self,
        thread_ids: &[ThreadId],
        add: &[LabelId],
        remove: &[LabelId],
        op_id: &OpId,
    ) -> CoreResult<()>;

    async fn trash(&self, thread_ids: &[ThreadId], op_id: &OpId) -> CoreResult<()>;

    async fn create_draft(&self, draft: &Draft) -> CoreResult<String>;
    async fn update_draft(&self, gmail_draft_id: &str, draft: &Draft) -> CoreResult<()>;
    async fn send_draft(&self, gmail_draft_id: &str) -> CoreResult<MessageId>;
    async fn delete_draft(&self, gmail_draft_id: &str) -> CoreResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread() -> ThreadSummary {
        ThreadSummary {
            account_id: AccountId::new("me@example.com"),
            id: ThreadId::new("thread"),
            subject: String::new(),
            snippet: String::new(),
            participants: Vec::new(),
            last_message_at: Utc::now(),
            message_count: 1,
            unread: true,
            starred: false,
            label_ids: vec![LabelId::new("INBOX")],
        }
    }

    fn message(from: &str) -> Message {
        Message {
            account_id: AccountId::new("me@example.com"),
            id: MessageId::new("message"),
            thread_id: ThreadId::new("thread"),
            from: from.into(),
            to: Vec::new(),
            cc: Vec::new(),
            subject: String::new(),
            snippet: String::new(),
            internal_date: Utc::now(),
            body_plain: None,
            body_html: None,
            headers: None,
            label_ids: Vec::new(),
            fetched_full: false,
            inline_images: Vec::new(),
        }
    }

    #[test]
    fn awaiting_reply_requires_inbox_attention_and_external_last_sender() {
        let mut summary = thread();
        assert!(is_awaiting_reply(
            &summary,
            Some(&message("Sender <them@example.com>"))
        ));
        assert!(!is_awaiting_reply(
            &summary,
            Some(&message("Me <ME@example.com>"))
        ));

        summary.unread = false;
        assert!(!is_awaiting_reply(
            &summary,
            Some(&message("them@example.com"))
        ));
        summary.starred = true;
        assert!(is_awaiting_reply(
            &summary,
            Some(&message("them@example.com"))
        ));
        summary.label_ids.clear();
        assert!(!is_awaiting_reply(
            &summary,
            Some(&message("them@example.com"))
        ));
    }
}
