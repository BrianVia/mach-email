use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{DraftId, LabelId, MessageId, ThreadId};

pub const DISPATCHER_ACTION_NAMES: &[&str] = &[
    "select_next",
    "select_prev",
    "open_thread",
    "back_to_list",
    "archive",
    "trash",
    "mark_read",
    "star",
    "add_label",
    "remove_label",
    "snooze",
    "search",
    "undo",
    "redo",
    "refresh",
    "cancel_send_later",
];

/// Deterministic identifier for a single mutation. Stamped on every outbox op
/// and used to suppress history events that echo our own writes back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OpId(pub String);

impl OpId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

impl std::fmt::Display for OpId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Default for OpId {
    fn default() -> Self {
        Self::new()
    }
}

/// The single set of user-invokable commands. Every TUI keybinding, CLI verb,
/// and MCP tool resolves to one of these.
///
/// Shared action vocabulary for interactive and programmatic surfaces.
/// Adapters may own presentation-only actions; durable mutations belong in
/// `Dispatcher`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    // --- Navigation (TUI-only state, no remote effect) ---
    SelectNext,
    SelectPrev,
    OpenThread {
        id: ThreadId,
    },
    BackToList,

    // --- Inbox mutations (optimistic local + outbox) ---
    Archive {
        thread_ids: Vec<ThreadId>,
    },
    Trash {
        thread_ids: Vec<ThreadId>,
    },
    MarkRead {
        thread_ids: Vec<ThreadId>,
        read: bool,
    },
    Star {
        thread_ids: Vec<ThreadId>,
        starred: bool,
    },
    AddLabel {
        thread_ids: Vec<ThreadId>,
        label_id: LabelId,
    },
    RemoveLabel {
        thread_ids: Vec<ThreadId>,
        label_id: LabelId,
    },

    /// Snooze applies the `MACH/Snoozed/<rfc3339>` label and removes INBOX.
    /// A background sweep removes the label and re-applies INBOX when due.
    Snooze {
        thread_ids: Vec<ThreadId>,
        until: DateTime<Utc>,
    },

    // --- Compose ---
    ComposeNew,
    Reply {
        message_id: MessageId,
        all: bool,
    },
    Forward {
        message_id: MessageId,
    },
    SaveDraft {
        draft_id: DraftId,
        patch: DraftPatch,
    },
    SendDraft {
        draft_id: DraftId,
    },
    SendLater {
        draft_id: DraftId,
        at: DateTime<Utc>,
    },
    CancelSendLater {
        send_later_id: String,
    },

    // --- Discovery ---
    Search {
        query: String,
        limit: u32,
    },
    OpenLabel {
        label_id: LabelId,
    },

    // --- Meta ---
    Undo,
    Redo,
    Refresh,
    Quit,
}

impl Action {
    /// Stable name used as the MCP tool name and for telemetry. Mirrors the
    /// serde tag, so `Archive` → `"archive"`.
    pub fn name(&self) -> &'static str {
        match self {
            Action::SelectNext => "select_next",
            Action::SelectPrev => "select_prev",
            Action::OpenThread { .. } => "open_thread",
            Action::BackToList => "back_to_list",
            Action::Archive { .. } => "archive",
            Action::Trash { .. } => "trash",
            Action::MarkRead { .. } => "mark_read",
            Action::Star { .. } => "star",
            Action::AddLabel { .. } => "add_label",
            Action::RemoveLabel { .. } => "remove_label",
            Action::Snooze { .. } => "snooze",
            Action::ComposeNew => "compose_new",
            Action::Reply { .. } => "reply",
            Action::Forward { .. } => "forward",
            Action::SaveDraft { .. } => "save_draft",
            Action::SendDraft { .. } => "send_draft",
            Action::SendLater { .. } => "send_later",
            Action::CancelSendLater { .. } => "cancel_send_later",
            Action::Search { .. } => "search",
            Action::OpenLabel { .. } => "open_label",
            Action::Undo => "undo",
            Action::Redo => "redo",
            Action::Refresh => "refresh",
            Action::Quit => "quit",
        }
    }

    /// `true` if executing this action queues a remote-bound write.
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Action::Archive { .. }
                | Action::Trash { .. }
                | Action::MarkRead { .. }
                | Action::Star { .. }
                | Action::AddLabel { .. }
                | Action::RemoveLabel { .. }
                | Action::Snooze { .. }
                | Action::SaveDraft { .. }
                | Action::SendDraft { .. }
                | Action::SendLater { .. }
                | Action::CancelSendLater { .. }
        )
    }

    /// Whether the shared dispatcher can execute this action without
    /// presentation-specific state. MCP uses this to avoid advertising
    /// reserved composition actions as working tools.
    pub fn is_dispatcher_supported(&self) -> bool {
        DISPATCHER_ACTION_NAMES.contains(&self.name())
    }
}

/// Patch applied to a draft when saving. All fields optional; `None` leaves
/// the existing value alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct DraftPatch {
    pub to: Option<Vec<String>>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    pub subject: Option<String>,
    pub body_md: Option<String>,
    pub in_reply_to_message_id: Option<MessageId>,
    pub thread_id: Option<ThreadId>,
}

/// What the dispatcher gives back. Surfaces decide how to render it: TUI
/// repaints, CLI prints JSON, MCP returns it as the tool result.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActionOutcome {
    pub action_name: String,
    pub op_id: Option<OpId>,
    /// Threads whose state changed locally as a result of this action.
    pub changed_threads: Vec<ThreadId>,
    /// Drafts whose state changed locally.
    pub changed_drafts: Vec<DraftId>,
    /// Free-form result payload; for `Search` this is the result list.
    pub data: Option<serde_json::Value>,
    /// Human-readable status. Empty string when nothing to say.
    pub message: String,
}

impl ActionOutcome {
    pub fn empty(name: &'static str) -> Self {
        Self {
            action_name: name.to_string(),
            op_id: None,
            changed_threads: Vec::new(),
            changed_drafts: Vec::new(),
            data: None,
            message: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let action = Action::Archive {
            thread_ids: vec![ThreadId::new("t1"), ThreadId::new("t2")],
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: Action = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, Action::Archive { ref thread_ids } if thread_ids.len() == 2));
    }

    #[test]
    fn search_action_parses_from_external_json() {
        let raw = r#"{"kind":"search","query":"is:unread","limit":10}"#;
        let parsed: Action = serde_json::from_str(raw).unwrap();
        match parsed {
            Action::Search { query, limit } => {
                assert_eq!(query, "is:unread");
                assert_eq!(limit, 10);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn name_matches_serde_tag() {
        let action = Action::AddLabel {
            thread_ids: vec![ThreadId::new("t1")],
            label_id: LabelId::new("L1"),
        };
        assert_eq!(action.name(), "add_label");
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["kind"], "add_label");
    }

    #[test]
    fn navigation_actions_are_not_mutations() {
        assert!(!Action::SelectNext.is_mutation());
        assert!(!Action::Refresh.is_mutation());
        assert!(Action::Archive {
            thread_ids: vec![ThreadId::new("t")]
        }
        .is_mutation());
    }
}
