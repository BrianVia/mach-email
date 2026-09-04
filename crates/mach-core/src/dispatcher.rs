use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use tracing::{debug, instrument};

use crate::action::{Action, ActionOutcome, DraftPatch, InviteResponse, OpId};
use crate::calendar::{build_reply_ics, PartStat};
use crate::compose::{forward_prefill, reply_prefill, Prefill};
use crate::error::{CoreError, CoreResult};
use crate::event::StateEvent;
use crate::ids::{AccountId, AccountScope, DraftId, LabelId, ThreadId};
use crate::state::{AppState, View};
use crate::store::{Draft, MailStore, OutboxOpKind};
use crate::unsubscribe::unsubscribe_targets;
use crate::user_config::UserConfig;

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
    default_account: Option<AccountId>,
    user_config: UserConfig,
    state: Mutex<AppState>,
    events: broadcast::Sender<StateEvent>,
    /// Serializes actions and owns the complete undo/redo invariant. Keeping
    /// both stacks under one lock prevents a concurrent action from landing
    /// between a mutation and its history record.
    history: Mutex<ActionHistory>,
}

#[derive(Debug)]
struct ActionHistory {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
}

#[derive(Debug)]
struct HistoryEntry {
    forward: Action,
    inverse: Vec<Action>,
}

impl ActionHistory {
    fn new() -> Self {
        Self {
            undo: VecDeque::with_capacity(UNDO_DEPTH),
            redo: VecDeque::with_capacity(UNDO_DEPTH),
        }
    }

    fn record_new(&mut self, entry: HistoryEntry) {
        push_bounded(&mut self.undo, entry);
        self.redo.clear();
    }

    fn record_undo(&mut self, entry: HistoryEntry) {
        push_bounded(&mut self.undo, entry);
    }

    fn record_redo(&mut self, entry: HistoryEntry) {
        push_bounded(&mut self.redo, entry);
    }
}

fn push_bounded(stack: &mut VecDeque<HistoryEntry>, entry: HistoryEntry) {
    if stack.len() == UNDO_DEPTH {
        stack.pop_front();
    }
    stack.push_back(entry);
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
            default_account: None,
            user_config: UserConfig::default(),
            state: Mutex::new(AppState::default()),
            events,
            history: Mutex::new(ActionHistory::new()),
        }
    }

    pub fn with_default_account(mut self, account: AccountId) -> Self {
        self.default_account = Some(account);
        self
    }

    pub fn with_user_config(mut self, config: UserConfig) -> Self {
        self.user_config = config;
        self
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
        let forward = action.clone();
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
            Action::Mute { thread_ids } => {
                self.modify_labels(
                    &thread_ids,
                    &[LabelId::new("MACH/Muted")],
                    &[LabelId::new("INBOX")],
                    "mute",
                )
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
            Action::Unsubscribe { message_id } => self.unsubscribe(&message_id).await,
            Action::RespondToInvite {
                message_id,
                response,
            } => self.respond_to_invite(&message_id, response).await,
            Action::ComposeNew => self.compose_new().await,
            Action::Reply { message_id, all } => self.reply(&message_id, all).await,
            Action::Forward { message_id } => self.forward(&message_id).await,
            Action::SaveDraft { draft_id, patch } => self.save_draft(&draft_id, patch).await,
            Action::SendDraft { draft_id } => self.send_draft(&draft_id).await,
            Action::SendLater { draft_id, at } => self.send_later(&draft_id, at).await,
            Action::CancelSendLater { send_later_id } => {
                self.cancel_send_later(&send_later_id).await
            }
            Action::Search { query, limit } => self.search(&query, limit).await,
            Action::Refresh => Ok(ActionOutcome::empty("refresh")),
            Action::Undo => return self.do_undo(history).await,
            Action::UndoActivity { outbox_id } => {
                return self.do_undo_activity(outbox_id, history).await
            }
            Action::Redo => return self.do_redo(history).await,
            other => Err(CoreError::InvalidAction(format!(
                "{} is not implemented yet",
                other.name()
            ))),
        };

        // On success, push inverse to undo stack and (when this is a
        // first-class user action, not a replay) clear redo.
        if track_for_undo {
            if let (Ok(_), Some(inverse)) = (&outcome, inverse) {
                history.record_new(HistoryEntry { forward, inverse });
            }
        }
        outcome
    }

    fn draft_account(&self) -> CoreResult<AccountId> {
        self.scope
            .account()
            .cloned()
            .or_else(|| self.default_account.clone())
            .ok_or_else(|| {
                CoreError::InvalidAction(
                    "no default account is configured for compose actions".into(),
                )
            })
    }

    async fn compose_new(&self) -> CoreResult<ActionOutcome> {
        let account_id = self.draft_account()?;
        let draft = Draft {
            body_md: self.body_with_signature(&account_id, ""),
            calendar_reply_ics: None,
            account_id,
            id: new_draft_id(),
            gmail_draft_id: None,
            thread_id: None,
            in_reply_to_message_id: None,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::new(),
            attachments: Vec::new(),
            updated_at: chrono::Utc::now(),
        };
        self.store.save_draft_local(&draft).await?;
        self.draft_outcome("compose_new", draft, None, String::new())
    }

    async fn reply(
        &self,
        message_id: &crate::ids::MessageId,
        all: bool,
    ) -> CoreResult<ActionOutcome> {
        let source = self
            .store
            .get_message(&self.scope, message_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
        let prefill = reply_prefill(&source, source.account_id.as_str(), all);
        self.create_prefilled_draft("reply", source.account_id, prefill)
            .await
    }

    async fn unsubscribe(&self, message_id: &crate::ids::MessageId) -> CoreResult<ActionOutcome> {
        let message = self
            .store
            .get_message(&self.scope, message_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
        let targets = message
            .headers
            .as_ref()
            .map(unsubscribe_targets)
            .unwrap_or_default();
        let mut outcome = ActionOutcome::empty("unsubscribe");
        outcome.data = Some(serde_json::json!({ "targets": targets }));
        outcome.message = format!("{} unsubscribe target(s)", targets.len());
        Ok(outcome)
    }

    async fn respond_to_invite(
        &self,
        message_id: &crate::ids::MessageId,
        response: InviteResponse,
    ) -> CoreResult<ActionOutcome> {
        let source = self
            .store
            .get_message(&self.scope, message_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
        let invite = source
            .calendar
            .as_ref()
            .ok_or_else(|| CoreError::InvalidAction("message has no calendar invite".into()))?;
        let (word, status) = match response {
            InviteResponse::Accepted => ("Accepted", PartStat::Accepted),
            InviteResponse::Declined => ("Declined", PartStat::Declined),
            InviteResponse::Tentative => ("Tentative", PartStat::Tentative),
        };
        let draft = Draft {
            account_id: source.account_id.clone(),
            id: new_draft_id(),
            gmail_draft_id: None,
            thread_id: Some(source.thread_id.clone()),
            in_reply_to_message_id: Some(source.id.clone()),
            to: vec![invite.organizer.clone()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{word}: {}", invite.summary),
            body_md: format!("{word}: {}", invite.summary),
            attachments: Vec::new(),
            calendar_reply_ics: Some(build_reply_ics(invite, source.account_id.as_str(), status)),
            updated_at: chrono::Utc::now(),
        };
        self.store.save_draft_local(&draft).await?;
        let mut outcome = self.send_draft(&draft.id).await?;
        outcome.action_name = "respond_to_invite".into();
        outcome.message = "Reply sent".into();
        Ok(outcome)
    }

    async fn forward(&self, message_id: &crate::ids::MessageId) -> CoreResult<ActionOutcome> {
        let source = self
            .store
            .get_message(&self.scope, message_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
        let prefill = forward_prefill(&source);
        self.create_prefilled_draft("forward", source.account_id, prefill)
            .await
    }

    async fn create_prefilled_draft(
        &self,
        action_name: &'static str,
        account_id: AccountId,
        prefill: Prefill,
    ) -> CoreResult<ActionOutcome> {
        let body_md = self.body_with_signature(&account_id, &prefill.body_md);
        let draft = Draft {
            account_id,
            id: new_draft_id(),
            gmail_draft_id: None,
            thread_id: prefill.thread_id,
            in_reply_to_message_id: prefill.in_reply_to_message_id,
            to: prefill.to,
            cc: prefill.cc,
            bcc: Vec::new(),
            subject: prefill.subject,
            body_md,
            attachments: Vec::new(),
            calendar_reply_ics: None,
            updated_at: chrono::Utc::now(),
        };
        self.store.save_draft_local(&draft).await?;
        self.draft_outcome(action_name, draft, None, String::new())
    }

    fn body_with_signature(&self, account_id: &AccountId, prefill: &str) -> String {
        let Some(signature) = self.user_config.signature_for(account_id.as_str()) else {
            return prefill.to_string();
        };
        if prefill.is_empty() {
            format!("\n\n{signature}")
        } else {
            format!("\n\n{signature}\n\n{}", prefill.trim_start_matches('\n'))
        }
    }

    async fn save_draft(&self, draft_id: &DraftId, patch: DraftPatch) -> CoreResult<ActionOutcome> {
        let mut draft = self
            .store
            .find_draft(&self.scope, draft_id)
            .await?
            .ok_or_else(|| CoreError::NotFound("draft not found".into()))?;
        if let Some(to) = patch.to {
            draft.to = to;
        }
        if let Some(cc) = patch.cc {
            draft.cc = cc;
        }
        if let Some(bcc) = patch.bcc {
            draft.bcc = bcc;
        }
        if let Some(subject) = patch.subject {
            draft.subject = subject;
        }
        if let Some(body_md) = patch.body_md {
            draft.body_md = body_md;
        }
        if let Some(attachments) = patch.attachments {
            draft.attachments = attachments;
        }
        if let Some(message_id) = patch.in_reply_to_message_id {
            draft.in_reply_to_message_id = Some(message_id);
        }
        if let Some(thread_id) = patch.thread_id {
            draft.thread_id = Some(thread_id);
        }
        draft.updated_at = chrono::Utc::now();
        self.store.save_draft_local(&draft).await?;
        self.draft_outcome("save_draft", draft, None, String::new())
    }

    async fn send_draft(&self, draft_id: &DraftId) -> CoreResult<ActionOutcome> {
        let draft = self
            .store
            .find_draft(&self.scope, draft_id)
            .await?
            .ok_or_else(|| CoreError::NotFound("draft not found".into()))?;
        validate_recipients(&draft)?;
        let op_id = OpId::new();
        self.store
            .queue_draft_send(&draft.account_id, draft_id, &op_id)
            .await?;
        self.draft_changed_outcome("send_draft", draft.id, Some(op_id), "queued to send".into())
    }

    async fn send_later(
        &self,
        draft_id: &DraftId,
        at: chrono::DateTime<chrono::Utc>,
    ) -> CoreResult<ActionOutcome> {
        let draft = self
            .store
            .find_draft(&self.scope, draft_id)
            .await?
            .ok_or_else(|| CoreError::NotFound("draft not found".into()))?;
        validate_recipients(&draft)?;
        self.store
            .schedule_send(&draft.account_id, draft_id, at)
            .await?;
        self.draft_changed_outcome("send_later", draft.id, None, String::new())
    }

    async fn cancel_send_later(&self, send_later_id: &str) -> CoreResult<ActionOutcome> {
        let scheduled = self
            .store
            .list_scheduled(&self.scope)
            .await?
            .into_iter()
            .find(|send| send.send_later_id == send_later_id)
            .ok_or_else(|| CoreError::NotFound("scheduled send not found".into()))?;
        self.store
            .cancel_scheduled(&scheduled.account_id, send_later_id)
            .await?;
        self.draft_changed_outcome(
            "cancel_send_later",
            scheduled.draft_id,
            None,
            "scheduled send cancelled".into(),
        )
    }

    fn draft_outcome(
        &self,
        action_name: &'static str,
        draft: Draft,
        op_id: Option<OpId>,
        message: String,
    ) -> CoreResult<ActionOutcome> {
        let id = draft.id.clone();
        let data = serde_json::json!({ "draft": draft });
        let _ = self.events.send(StateEvent::DraftChanged(id.clone()));
        Ok(ActionOutcome {
            action_name: action_name.into(),
            op_id,
            changed_threads: Vec::new(),
            changed_drafts: vec![id],
            data: Some(data),
            message,
        })
    }

    fn draft_changed_outcome(
        &self,
        action_name: &'static str,
        id: DraftId,
        op_id: Option<OpId>,
        message: String,
    ) -> CoreResult<ActionOutcome> {
        let _ = self.events.send(StateEvent::DraftChanged(id.clone()));
        Ok(ActionOutcome {
            action_name: action_name.into(),
            op_id,
            changed_threads: Vec::new(),
            changed_drafts: vec![id],
            data: None,
            message,
        })
    }
    async fn do_undo(&self, history: &mut ActionHistory) -> CoreResult<ActionOutcome> {
        let Some(entry) = history.undo.pop_back() else {
            return Ok(ActionOutcome {
                action_name: "undo".into(),
                op_id: None,
                changed_threads: Vec::new(),
                changed_drafts: Vec::new(),
                data: None,
                message: "nothing to undo".into(),
            });
        };
        let mut outcome = ActionOutcome::empty("undo");
        for inverse in entry.inverse.iter().cloned() {
            outcome = Box::pin(self.execute_inner(inverse, false, history)).await?;
        }
        history.record_redo(entry);
        Ok(outcome)
    }

    async fn do_redo(&self, history: &mut ActionHistory) -> CoreResult<ActionOutcome> {
        let Some(entry) = history.redo.pop_back() else {
            return Ok(ActionOutcome {
                action_name: "redo".into(),
                op_id: None,
                changed_threads: Vec::new(),
                changed_drafts: Vec::new(),
                data: None,
                message: "nothing to redo".into(),
            });
        };
        let outcome = Box::pin(self.execute_inner(entry.forward.clone(), false, history)).await?;
        history.record_undo(entry);
        Ok(outcome)
    }

    async fn do_undo_activity(
        &self,
        outbox_id: i64,
        history: &mut ActionHistory,
    ) -> CoreResult<ActionOutcome> {
        let activity = self
            .store
            .list_activity(&self.scope, 0, u32::MAX)
            .await?
            .into_iter()
            .find(|entry| entry.id == outbox_id)
            .ok_or_else(|| CoreError::NotFound(format!("activity {outbox_id}")))?;
        if activity.undone {
            return Err(CoreError::InvalidAction(format!(
                "activity {outbox_id} is already undone"
            )));
        }
        let op = self
            .store
            .get_outbox_op(&self.scope, outbox_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("activity {outbox_id}")))?;
        let inverse = self.inverse_of_op(&op.kind, true).await?;
        if inverse.is_empty() {
            return Err(CoreError::InvalidAction(format!(
                "activity {outbox_id} is not undoable"
            )));
        }

        let mut changed_threads = Vec::new();
        let mut last_op_id = None;
        let mut inverse_outbox_id = None;
        for action in inverse {
            let outcome = Box::pin(self.execute_inner(action, false, history)).await?;
            changed_threads.extend(outcome.changed_threads);
            last_op_id = outcome.op_id;
            inverse_outbox_id = outcome
                .data
                .and_then(|data| data.get("outbox_id").and_then(|id| id.as_i64()));
        }
        let inverse_outbox_id = inverse_outbox_id.ok_or_else(|| {
            CoreError::Storage("undo did not create an inverse outbox row".into())
        })?;
        self.store
            .mark_outbox_undone(outbox_id, inverse_outbox_id)
            .await?;
        let mut unique_threads = Vec::new();
        for id in changed_threads {
            if !unique_threads.contains(&id) {
                unique_threads.push(id);
            }
        }
        Ok(ActionOutcome {
            action_name: "undo_activity".into(),
            op_id: last_op_id,
            changed_threads: unique_threads,
            changed_drafts: Vec::new(),
            data: None,
            message: format!("undid activity {outbox_id}"),
        })
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
        let outbox_id = self
            .store
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
            data: Some(serde_json::json!({ "outbox_id": outbox_id })),
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
        let outbox_id = self
            .store
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
            data: Some(serde_json::json!({ "outbox_id": outbox_id })),
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
    async fn compute_inverse(&self, action: &Action) -> CoreResult<Option<Vec<Action>>> {
        let kind = match action {
            Action::Archive { thread_ids } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: Vec::new(),
                remove: vec![LabelId::new("INBOX")],
            },
            Action::Mute { thread_ids } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: vec![LabelId::new("MACH/Muted")],
                remove: vec![LabelId::new("INBOX")],
            },
            Action::Trash { thread_ids } => OutboxOpKind::Trash {
                thread_ids: thread_ids.clone(),
            },
            Action::MarkRead { thread_ids, read } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: (!read)
                    .then(|| LabelId::new("UNREAD"))
                    .into_iter()
                    .collect(),
                remove: read.then(|| LabelId::new("UNREAD")).into_iter().collect(),
            },
            Action::Star {
                thread_ids,
                starred,
            } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: starred
                    .then(|| LabelId::new("STARRED"))
                    .into_iter()
                    .collect(),
                remove: (!starred)
                    .then(|| LabelId::new("STARRED"))
                    .into_iter()
                    .collect(),
            },
            Action::AddLabel {
                thread_ids,
                label_id,
            } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: vec![label_id.clone()],
                remove: Vec::new(),
            },
            Action::RemoveLabel {
                thread_ids,
                label_id,
            } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: Vec::new(),
                remove: vec![label_id.clone()],
            },
            Action::Snooze { thread_ids, until } => OutboxOpKind::ModifyLabels {
                thread_ids: thread_ids.clone(),
                add: vec![LabelId::new(format!("MACH/Snoozed/{}", until.to_rfc3339()))],
                remove: vec![LabelId::new("INBOX")],
            },
            _ => return Ok(None),
        };
        let inverse = self.inverse_of_op(&kind, false).await?;
        Ok((!inverse.is_empty()).then_some(inverse))
    }

    async fn inverse_of_op(
        &self,
        kind: &OutboxOpKind,
        observed_after: bool,
    ) -> CoreResult<Vec<Action>> {
        let (thread_ids, add, remove) = match kind {
            OutboxOpKind::ModifyLabels {
                thread_ids,
                add,
                remove,
            } => (thread_ids, add.clone(), remove.clone()),
            OutboxOpKind::Trash { thread_ids } => (
                thread_ids,
                vec![LabelId::new("TRASH")],
                vec![LabelId::new("INBOX")],
            ),
            _ => return Ok(Vec::new()),
        };
        let mut inverse = Vec::new();
        for label_id in add {
            let changed = self
                .with_label_state(thread_ids, label_id.as_str(), observed_after)
                .await?;
            if !changed.is_empty() {
                inverse.push(Action::RemoveLabel {
                    thread_ids: changed,
                    label_id,
                });
            }
        }
        for label_id in remove {
            let changed = self
                .with_label_state(thread_ids, label_id.as_str(), !observed_after)
                .await?;
            if !changed.is_empty() {
                inverse.push(Action::AddLabel {
                    thread_ids: changed,
                    label_id,
                });
            }
        }
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

fn new_draft_id() -> DraftId {
    DraftId::new(uuid::Uuid::new_v4().simple().to_string())
}

fn validate_recipients(draft: &Draft) -> CoreResult<()> {
    if draft
        .to
        .iter()
        .chain(&draft.cc)
        .chain(&draft.bcc)
        .all(|address| address.trim().is_empty())
    {
        return Err(CoreError::InvalidAction("draft has no recipients".into()));
    }
    Ok(())
}
