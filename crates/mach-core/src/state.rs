use std::collections::VecDeque;

use crate::action::Action;
use crate::ids::{LabelId, ThreadId};

/// In-process UI state owned by the dispatcher actor. The store is the
/// source of truth for mail data; this is only the *projection* the user
/// is currently looking at plus undo history.
#[derive(Debug, Default)]
pub struct AppState {
    pub view: View,
    pub selection: Selection,
    pub current_label: Option<LabelId>,
    pub undo: VecDeque<Action>,
    pub redo: VecDeque<Action>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum View {
    #[default]
    Inbox,
    Thread(ThreadId),
    Compose,
    Search,
}

#[derive(Debug, Default, Clone)]
pub struct Selection {
    /// Thread IDs the user has multi-selected. Single-select is len() == 1.
    pub thread_ids: Vec<ThreadId>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.thread_ids.is_empty()
    }
}

const UNDO_DEPTH: usize = 20;

impl AppState {
    pub fn push_undo(&mut self, action: Action) {
        if self.undo.len() == UNDO_DEPTH {
            self.undo.pop_front();
        }
        self.undo.push_back(action);
        self.redo.clear();
    }
}
