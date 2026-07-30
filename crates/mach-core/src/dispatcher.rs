use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use tracing::{debug, instrument};

use crate::action::{Action, ActionOutcome, OpId};
use crate::error::{CoreError, CoreResult};
use crate::event::StateEvent;
use crate::ids::{AccountScope, LabelId, ThreadId};
use crate::state::{AppState, View};
use crate::store::{MailStore, OutboxOpKind};

const UNDO_DEPTH: usize = 20;

/// The single point of durable mutation in the system. Presentation adapters
/// may handle navigation actions locally, but all shared mail mutations flow
/// through here.
///
/// Order of operations on a mutating action:
///   1. validate against AppState
///   2. atomically write the optimistic change and durable outbox record
///   3. push the inverse onto the bounded history
///   4. broadcast StateEvent
///
/// The sync engine drains the outbox in the background. UI surfaces never
/// wait on the network — that's the whole point.
pub struct Dispatcher {
    store: Arc<dyn MailStore>,
    scope: AccountScope,
    state: Mutex<AppState>,
    events: broadcast::Sender<StateEvent>,
    /// Serializes actions and owns the complete undo/redo invariant. Keeping
    /// both stacks under one lock prevents a concurrent action from landing
    /// between a mutation and its history record.
    history: Mutex<ActionHistory>,
}

#[derive(Debug)]
struct ActionHistory {
    undo: VecDeque<Action>,
    redo: VecDeque<Action>,
}

impl ActionHistory {
    fn new() -> Self {
        Self {
            undo: VecDeque::with_capacity(UNDO_DEPTH),
            redo: VecDeque::with_capacity(UNDO_DEPTH),
        }
    }

    fn record_new(&mut self, inverse: Action) {
        push_bounded(&mut self.undo, inverse);
        self.redo.clear();
    }

    fn record_undo(&mut self, inverse: Action) {
        push_bounded(&mut self.undo, inverse);
    }

    fn record_redo(&mut self, action: Action) {
        push_bounded(&mut self.redo, action);
    }
}

fn push_bounded(stack: &mut VecDeque<Action>, action: Action) {
    if stack.len() == UNDO_DEPTH {
        stack.pop_front();
    }
    stack.push_back(action);
}

impl Dispatcher {
    pub fn new(store: Arc<dyn MailStore>) -> Self {
        Self::with_scope(store, AccountScope::All)
    }

    pub fn with_scope(store: Arc<dyn MailStore>, scope: AccountScope) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            store,
            scope,
            state: Mutex::new(AppState::default()),
            events,
            history: Mutex::new(ActionHistory::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    pub fn store(&self) -> Arc<dyn MailStore> {
        Arc::clone(&self.store)
    }

    pub fn scope(&self) -> &AccountScope {
        &self.scope
    }

    #[instrument(skip(self), fields(action = action.name()))]
    pub async fn execute(&self, action: Action) -> CoreResult<ActionOutcome> {
        debug!("dispatching");
        let mut history = self.history.lock().await;
        self.execute_inner(action, true, &mut history).await
    }

    /// Internal dispatch. `track_for_undo=false` replays an undo/redo without
    /// recursively recording it as a new user action.
    async fn execute_inner(
        &self,
        action: Action,
        track_for_undo: bool,
        history: &mut ActionHistory,
    ) -> CoreResult<ActionOutcome> {
        // Compute the inverse BEFORE mutating so we don't lose info that
        // a later mutation might wipe (e.g. star toggle has the bool baked in).
        let inverse = if track_for_undo {
            self.compute_inverse(&action).await?
        } else {
            None
        };

        let outcome = match action {
            Action::SelectNext => self.move_selection(1).await,
            Action::SelectPrev => self.move_selection(-1).await,
            Action::OpenThread { id } => self.open_thread(id).await,
            Action::BackToList => self.back_to_list().await,
            Action::Archive { thread_ids } => {
                self.modify_labels(&thread_ids, &[], &[LabelId::new("INBOX")], "archive")
                    .await
            }
            Action::Trash { thread_ids } => self.trash(&thread_ids).await,
            Action::MarkRead { thread_ids, read } => {
                let unread = LabelId::new("UNREAD");
                if read {
                    self.modify_labels(&thread_ids, &[], &[unread], "mark_read")
                        .await
                } else {
                    self.modify_labels(&thread_ids, &[unread], &[], "mark_read")
                        .await
                }
            }
            Action::Star {
                thread_ids,
                starred,
            } => {
                let starred_lbl = LabelId::new("STARRED");
                if starred {
                    self.modify_labels(&thread_ids, &[starred_lbl], &[], "star")
                        .await
                } else {
                    self.modify_labels(&thread_ids, &[], &[starred_lbl], "star")
                        .await
                }
            }
            Action::AddLabel {
                thread_ids,
                label_id,
            } => {
                self.modify_labels(&thread_ids, &[label_id], &[], "add_label")
                    .await
            }
            Action::RemoveLabel {
                thread_ids,
                label_id,
            } => {
                self.modify_labels(&thread_ids, &[], &[label_id], "remove_label")
                    .await
            }
            Action::Snooze { thread_ids, until } => {
                let snoozed = LabelId::new(format!("MACH/Snoozed/{}", until.to_rfc3339()));
                let inbox = LabelId::new("INBOX");
                self.modify_labels(&thread_ids, &[snoozed], &[inbox], "snooze")
                    .await
            }
            Action::Search { query, limit } => self.search(&query, limit).await,
            Action::Refresh => Ok(ActionOutcome::empty("refresh")),
            Action::Undo => return self.do_undo(history).await,
            Action::Redo => return self.do_redo(history).await,
            other => Err(CoreError::InvalidAction(format!(
                "{} is not implemented yet",
                other.name()
            ))),
        };

        // On success, push inverse to undo stack and (when this is a
        // first-class user action, not a replay) clear redo.
        if track_for_undo {
            if let (Ok(_), Some(inv)) = (&outcome, inverse) {
                history.record_new(inv);
            }
        }
        outcome
    }

    async fn do_undo(&self, history: &mut ActionHistory) -> CoreResult<ActionOutcome> {
        let Some(inverse) = history.undo.pop_back() else {
            return Ok(ActionOutcome {
                action_name: "undo".into(),
                op_id: None,
                changed_threads: Vec::new(),
                changed_drafts: Vec::new(),
                data: None,
                message: "nothing to undo".into(),
            });
        };
        let redo_action = self
            .compute_inverse(&inverse)
            .await?
            .unwrap_or_else(|| inverse.clone());
        let outcome = Box::pin(self.execute_inner(inverse, false, history)).await?;
        history.record_redo(redo_action);
        Ok(outcome)
    }

    async fn do_redo(&self, history: &mut ActionHistory) -> CoreResult<ActionOutcome> {
        let Some(action) = history.redo.pop_back() else {
            return Ok(ActionOutcome {
                action_name: "redo".into(),
                op_id: None,
                changed_threads: Vec::new(),
                changed_drafts: Vec::new(),
                data: None,
                message: "nothing to redo".into(),
            });
        };
        let undo_inverse = self.compute_inverse(&action).await?;
        let outcome = Box::pin(self.execute_inner(action, false, history)).await?;
        if let Some(inv) = undo_inverse {
            history.record_undo(inv);
        }
        Ok(outcome)
    }

    async fn modify_labels(
        &self,
        thread_ids: &[crate::ids::ThreadId],
        add: &[LabelId],
        remove: &[LabelId],
        action_name: &'static str,
    ) -> CoreResult<ActionOutcome> {
        if thread_ids.is_empty() {
            return Err(CoreError::InvalidAction(format!(
                "{action_name} called with empty thread_ids"
            )));
        }
        let op_id = OpId::new();
        let kind = OutboxOpKind::ModifyLabels {
            thread_ids: thread_ids.to_vec(),
            add: add.to_vec(),
            remove: remove.to_vec(),
        };
        self.store
            .apply_thread_mutation(&self.scope, &op_id, &kind)
            .await?;

        let _ = self
            .events
            .send(StateEvent::ThreadsChanged(thread_ids.to_vec()));

        Ok(ActionOutcome {
            action_name: action_name.to_string(),
            op_id: Some(op_id),
            changed_threads: thread_ids.to_vec(),
            changed_drafts: Vec::new(),
            data: None,
            message: format!("{action_name}: {} thread(s)", thread_ids.len()),
        })
    }

    async fn trash(&self, thread_ids: &[crate::ids::ThreadId]) -> CoreResult<ActionOutcome> {
        if thread_ids.is_empty() {
            return Err(CoreError::InvalidAction(
                "trash called with empty thread_ids".into(),
            ));
        }
        let op_id = OpId::new();
        let kind = OutboxOpKind::Trash {
            thread_ids: thread_ids.to_vec(),
        };
        self.store
            .apply_thread_mutation(&self.scope, &op_id, &kind)
            .await?;

        let _ = self
            .events
            .send(StateEvent::ThreadsChanged(thread_ids.to_vec()));

        Ok(ActionOutcome {
            action_name: "trash".into(),
            op_id: Some(op_id),
            changed_threads: thread_ids.to_vec(),
            changed_drafts: Vec::new(),
            data: None,
            message: format!("trashed {} thread(s)", thread_ids.len()),
        })
    }

    async fn move_selection(&self, delta: i32) -> CoreResult<ActionOutcome> {
        let mut state = self.state.lock().await;
        // Selection lives over the inbox list. We don't store the list itself
        // here (it's projected from the store on demand) — we just shift the
        // selected thread index. The TUI redraws based on the broadcast event.
        let label = state
            .current_label
            .clone()
            .unwrap_or_else(|| LabelId::new("INBOX"));
        let threads = self
            .store
            .list_threads_in_label(&self.scope, &label, 200)
            .await?;
        if threads.is_empty() {
            return Ok(ActionOutcome::empty(if delta > 0 {
                "select_next"
            } else {
                "select_prev"
            }));
        }
        let current = state
            .selection
            .thread_ids
            .first()
            .and_then(|tid| threads.iter().position(|t| &t.id == tid))
            .unwrap_or(0);
        let next = ((current as i32 + delta).rem_euclid(threads.len() as i32)) as usize;
        let new_id = threads[next].id.clone();
        state.selection.thread_ids = vec![new_id.clone()];
        let _ = self
            .events
            .send(StateEvent::ThreadsChanged(vec![new_id.clone()]));
        Ok(ActionOutcome {
            action_name: if delta > 0 {
                "select_next"
            } else {
                "select_prev"
            }
            .into(),
            op_id: None,
            changed_threads: vec![new_id],
            changed_drafts: Vec::new(),
            data: None,
            message: String::new(),
        })
    }

    async fn open_thread(&self, id: ThreadId) -> CoreResult<ActionOutcome> {
        let summary = self
            .store
            .get_thread(&self.scope, &id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("thread {id}")))?;
        let messages = self.store.list_messages_in_thread(&self.scope, &id).await?;

        let mut state = self.state.lock().await;
        state.view = View::Thread(id.clone());
        state.selection.thread_ids = vec![id.clone()];

        let message_count = messages.len();
        let data = serde_json::json!({ "thread": summary, "messages": messages });
        Ok(ActionOutcome {
            action_name: "open_thread".into(),
            op_id: None,
            changed_threads: vec![id],
            changed_drafts: Vec::new(),
            data: Some(data),
            message: format!("{message_count} message(s)"),
        })
    }

    async fn back_to_list(&self) -> CoreResult<ActionOutcome> {
        let mut state = self.state.lock().await;
        state.view = View::Inbox;
        Ok(ActionOutcome::empty("back_to_list"))
    }

    async fn search(&self, query: &str, limit: u32) -> CoreResult<ActionOutcome> {
        let results = self.store.search_threads(&self.scope, query, limit).await?;
        let data = serde_json::to_value(&results)?;
        Ok(ActionOutcome {
            action_name: "search".into(),
            op_id: None,
            changed_threads: Vec::new(),
            changed_drafts: Vec::new(),
            data: Some(data),
            message: format!("{} result(s)", results.len()),
        })
    }

    /// Build an inverse from the state that actually exists before mutation.
    /// This prevents undo from changing threads on which the original action
    /// was a no-op (for example, starring an already-starred thread).
    async fn compute_inverse(&self, action: &Action) -> CoreResult<Option<Action>> {
        let inverse = match action {
            Action::Archive { thread_ids } => {
                let changed = self.with_label_state(thread_ids, "INBOX", true).await?;
                (!changed.is_empty()).then(|| Action::AddLabel {
                    thread_ids: changed,
                    label_id: LabelId::new("INBOX"),
                })
            }
            Action::MarkRead { thread_ids, read } => {
                let changed = self.with_label_state(thread_ids, "UNREAD", *read).await?;
                (!changed.is_empty()).then(|| Action::MarkRead {
                    thread_ids: changed,
                    read: !read,
                })
            }
            Action::Star {
                thread_ids,
                starred,
            } => {
                let changed = self
                    .with_label_state(thread_ids, "STARRED", !starred)
                    .await?;
                (!changed.is_empty()).then(|| Action::Star {
                    thread_ids: changed,
                    starred: !starred,
                })
            }
            Action::AddLabel {
                thread_ids,
                label_id,
            } => {
                let changed = self
                    .with_label_state(thread_ids, label_id.as_str(), false)
                    .await?;
                (!changed.is_empty()).then(|| Action::RemoveLabel {
                    thread_ids: changed,
                    label_id: label_id.clone(),
                })
            }
            Action::RemoveLabel {
                thread_ids,
                label_id,
            } => {
                let changed = self
                    .with_label_state(thread_ids, label_id.as_str(), true)
                    .await?;
                (!changed.is_empty()).then(|| Action::AddLabel {
                    thread_ids: changed,
                    label_id: label_id.clone(),
                })
            }
            _ => None,
        };
        Ok(inverse)
    }

    async fn with_label_state(
        &self,
        thread_ids: &[ThreadId],
        label: &str,
        present: bool,
    ) -> CoreResult<Vec<ThreadId>> {
        let mut matching = Vec::new();
        for id in thread_ids {
            let Some(thread) = self.store.get_thread(&self.scope, id).await? else {
                continue;
            };
            let has_label = thread
                .label_ids
                .iter()
                .any(|candidate| candidate.as_str() == label);
            if has_label == present {
                matching.push(id.clone());
            }
        }
        Ok(matching)
    }
}
