use serde::{Deserialize, Serialize};

use crate::ids::{DraftId, ThreadId};

/// Broadcast to UI/agent surfaces whenever the dispatcher mutates state. The
/// TUI uses this to know when to repaint without polling the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StateEvent {
    /// Threads whose row in the inbox changed (label, read status, etc.).
    ThreadsChanged(Vec<ThreadId>),
    /// A draft was created, updated, or sent.
    DraftChanged(DraftId),
    /// Sync engine status change. Surfaces render this in the status bar.
    SyncStatus(SyncStatus),
    /// User-visible toast (e.g. "Archived 3 threads — u to undo").
    Toast(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SyncStatus {
    Idle,
    Bootstrapping,
    Incremental,
    GapRecovering,
    Error(String),
}
