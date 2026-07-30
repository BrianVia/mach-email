use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use tracing::{debug, instrument};

use crate::action::{Action, ActionOutcome, OpId};
use crate::error::{CoreError, CoreResult};
use crate::event::StateEvent;
use crate::ids::{LabelId, ThreadId};
use crate::state::{AppState, View};
use crate::store::{MailStore, OutboxOpKind};

const UNDO_DEPTH: usize = 20;

/// The single point of mutation in the system. Every Action — whether it came
/// from a TUI keypress, `mach do`, or an MCP tool call — flows through here.
///
/// Order of operations on a mutating action:
///   1. validate against AppState
///   2. write optimistic local change via MailStore
///   3. enqueue outbox op (durable)
///   4. push inverse onto undo stack
///   5. broadcast StateEvent
///
/// The sync engine drains the outbox in the background. UI surfaces never
/// wait on the network — that's the whole point.
pub struct Dispatcher {
    store: Arc<dyn MailStore>,
    state: Mutex<AppState>,
    events: broadcast::Sender<StateEvent>,
    /// Reverse-action stack — pop to undo. Bounded at `UNDO_DEPTH` so we
    /// never grow without limit on long-running sessions.
    undo_stack: Mutex<VecDeque<Action>>,
    /// Forward-action stack — pop to redo after an undo. Cleared on any
    /// new (non-undo/redo) mutation.
    redo_stack: Mutex<VecDeque<Action>>,
}

impl Dispatcher {
    pub fn new(store: Arc<dyn MailStore>) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            store,
            state: Mutex::new(AppState::default()),
            events,
            undo_stack: Mutex::new(VecDeque::with_capacity(UNDO_DEPTH)),
            redo_stack: Mutex::new(VecDeque::with_capacity(UNDO_DEPTH)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StateEvent> {
        self.events.subscribe()
    }

    pub fn store(&self) -> Arc<dyn MailStore> {
        Arc::clone(&self.store)
    }

    #[instrument(skip(self), fields(action = action.name()))]
    pub async fn execute(&self, action: Action) -> CoreResult<ActionOutcome> {
        debug!("dispatching");
        // External execute: track for undo, clear redo (because a new
        // mutation invalidates the redo chain).
        self.execute_inner(action, true).await
    }

    /// Internal dispatch. `track_for_undo=false` is used when replaying
    /// an undo/redo so we don't infinite-loop our own history.
    async fn execute_inner(
        &self,
        action: Action,
        track_for_undo: bool,
    ) -> CoreResult<ActionOutcome> {
        // Compute the inverse BEFORE mutating so we don't lose info that
        // a later mutation might wipe (e.g. star toggle has the bool baked in).
        let inverse = if track_for_undo {
            compute_inverse(&action)
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
            Action::Star { thread_ids, starred } => {
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
            Action::Undo => return self.do_undo().await,
            Action::Redo => return self.do_redo().await,
            other => Err(CoreError::InvalidAction(format!(
                "{} is not implemented yet",
                other.name()
            ))),
        };

        // On success, push inverse to undo stack and (when this is a
        // first-class user action, not a replay) clear redo.
        if track_for_undo {
            if let (Ok(_), Some(inv)) = (&outcome, inverse) {
                let mut undo = self.undo_stack.lock().await;
                if undo.len() == UNDO_DEPTH {
                    undo.pop_front();
                }
                undo.push_back(inv);
                self.redo_stack.lock().await.clear();
            }
        }
        outcome
    }

    async fn do_undo(&self) -> CoreResult<ActionOutcome> {
        let mut undo = self.undo_stack.lock().await;
        let Some(inverse) = undo.pop_back() else {
            return Ok(ActionOutcome {
                action_name: "undo".into(),
                op_id: None,
                changed_threads: Vec::new(),
                changed_drafts: Vec::new(),
                data: None,
                message: "nothing to undo".into(),
            });
        };
        drop(undo);
        let redo_action = compute_inverse(&inverse).unwrap_or_else(|| inverse.clone());
        let outcome = Box::pin(self.execute_inner(inverse, false)).await?;
        let mut redo = self.redo_stack.lock().await;
        if redo.len() == UNDO_DEPTH {
            redo.pop_front();
        }
        redo.push_back(redo_action);
        Ok(outcome)
    }

    async fn do_redo(&self) -> CoreResult<ActionOutcome> {
        let mut redo = self.redo_stack.lock().await;
        let Some(action) = redo.pop_back() else {
            return Ok(ActionOutcome {
                action_name: "redo".into(),
                op_id: None,
                changed_threads: Vec::new(),
                changed_drafts: Vec::new(),
                data: None,
                message: "nothing to redo".into(),
            });
        };
        drop(redo);
        let undo_inverse = compute_inverse(&action);
        let outcome = Box::pin(self.execute_inner(action, false)).await?;
        if let Some(inv) = undo_inverse {
            let mut undo = self.undo_stack.lock().await;
            if undo.len() == UNDO_DEPTH {
                undo.pop_front();
            }
            undo.push_back(inv);
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

        self.store
            .modify_labels_local(thread_ids, add, remove)
            .await?;

        self.store
            .enqueue_outbox(
                &op_id,
                &OutboxOpKind::ModifyLabels {
                    thread_ids: thread_ids.to_vec(),
                    add: add.to_vec(),
                    remove: remove.to_vec(),
                },
            )
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

    async fn trash(
        &self,
        thread_ids: &[crate::ids::ThreadId],
    ) -> CoreResult<ActionOutcome> {
        if thread_ids.is_empty() {
            return Err(CoreError::InvalidAction(
                "trash called with empty thread_ids".into(),
            ));
        }
        let op_id = OpId::new();

        // Local: drop INBOX, add TRASH so the optimistic UI matches Gmail.
        self.store
            .modify_labels_local(thread_ids, &[LabelId::new("TRASH")], &[LabelId::new("INBOX")])
            .await?;

        self.store
            .enqueue_outbox(
                &op_id,
                &OutboxOpKind::Trash {
                    thread_ids: thread_ids.to_vec(),
                },
            )
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
        let threads = self.store.list_threads_in_label(&label, 200).await?;
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
            action_name: if delta > 0 { "select_next" } else { "select_prev" }.into(),
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
            .get_thread(&id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("thread {id}")))?;
        let messages = self.store.list_messages_in_thread(&id).await?;

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
        let results = self.store.search_threads(query, limit).await?;
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
}

/// Compute the inverse Action for the user-facing mutations we support
/// undo for. Returns `None` for actions without a clean inverse (snooze,
/// drafts, sends, trash) — those just don't show up in the undo chain
/// for v1.
fn compute_inverse(action: &Action) -> Option<Action> {
    match action {
        Action::Archive { thread_ids } => Some(Action::AddLabel {
            thread_ids: thread_ids.clone(),
            label_id: LabelId::new("INBOX"),
        }),
        Action::MarkRead { thread_ids, read } => Some(Action::MarkRead {
            thread_ids: thread_ids.clone(),
            read: !read,
        }),
        Action::Star { thread_ids, starred } => Some(Action::Star {
            thread_ids: thread_ids.clone(),
            starred: !starred,
        }),
        Action::AddLabel {
            thread_ids,
            label_id,
        } => Some(Action::RemoveLabel {
            thread_ids: thread_ids.clone(),
            label_id: label_id.clone(),
        }),
        Action::RemoveLabel {
            thread_ids,
            label_id,
        } => Some(Action::AddLabel {
            thread_ids: thread_ids.clone(),
            label_id: label_id.clone(),
        }),
        _ => None,
    }
}
