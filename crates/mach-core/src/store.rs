use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::action::OpId;
use crate::error::CoreResult;
use crate::ids::{DraftId, LabelId, MessageId, ThreadId};

/// A row in the inbox list. Denormalized for render speed — the projection
/// the TUI actually paints comes straight from this struct, no joins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
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
    pub id: LabelId,
    pub name: String,
    pub system: bool,
    pub color: Option<String>,
    pub unread_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
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
    pub op_id: OpId,
    pub kind: OutboxOpKind,
    pub created_at: DateTime<Utc>,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// Local persistence. The dispatcher writes here optimistically; the sync
/// engine reads pending outbox ops and pushes them upstream.
#[async_trait]
pub trait MailStore: Send + Sync {
    async fn get_thread(&self, id: &ThreadId) -> CoreResult<Option<ThreadSummary>>;
    async fn list_threads_in_label(&self, label: &LabelId, limit: u32) -> CoreResult<Vec<ThreadSummary>>;
    async fn list_messages_in_thread(&self, id: &ThreadId) -> CoreResult<Vec<Message>>;
    async fn search_threads(&self, query: &str, limit: u32) -> CoreResult<Vec<ThreadSummary>>;

    async fn modify_labels_local(
        &self,
        thread_ids: &[ThreadId],
        add: &[LabelId],
        remove: &[LabelId],
    ) -> CoreResult<()>;

    async fn list_labels(&self) -> CoreResult<Vec<Label>>;

    async fn get_draft(&self, id: &DraftId) -> CoreResult<Option<Draft>>;
    async fn save_draft_local(&self, draft: &Draft) -> CoreResult<()>;
    async fn delete_draft_local(&self, id: &DraftId) -> CoreResult<()>;

    async fn enqueue_outbox(&self, op_id: &OpId, kind: &OutboxOpKind) -> CoreResult<i64>;
    async fn drain_pending_outbox(&self, max: u32) -> CoreResult<Vec<OutboxOp>>;
    async fn mark_outbox_done(&self, id: i64) -> CoreResult<()>;
    async fn mark_outbox_failed(&self, id: i64, error: &str) -> CoreResult<()>;

    /// Currently-stored sync cursor for the active account, if any.
    async fn get_history_cursor(&self) -> CoreResult<Option<u64>>;
    async fn set_history_cursor(&self, cursor: u64) -> CoreResult<()>;
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
