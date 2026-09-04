//! In-memory `MailStore` implementation. Production code uses `mach-store`'s
//! SQLite-backed impl; this exists so `mach-core` can test its dispatcher
//! logic without dragging SQLite into the unit tests.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::action::OpId;
use crate::error::CoreResult;
use crate::ids::{AccountId, AccountScope, DraftId, LabelId, MessageId, ThreadId};
use crate::store::{
    ActivityEntry, Draft, Label, MailStore, Message, OutboxOp, OutboxOpKind, OutboxSummary,
    ThreadSummary,
};

#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    threads: HashMap<(AccountId, ThreadId), ThreadSummary>,
    messages_by_thread: HashMap<(AccountId, ThreadId), Vec<Message>>,
    labels: Vec<Label>,
    drafts: HashMap<(AccountId, DraftId), Draft>,
    scheduled_sends: HashMap<(AccountId, DraftId), DateTime<Utc>>,
    outbox: Vec<OutboxOp>,
    outbox_next_attempt_at: HashMap<i64, i64>,
    outbox_undone_by: HashMap<i64, i64>,
    next_outbox_id: i64,
    history_cursors: HashMap<AccountId, u64>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper — seed the store with a thread.
    pub fn insert_thread(&self, summary: ThreadSummary, messages: Vec<Message>) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .messages_by_thread
            .insert((summary.account_id.clone(), summary.id.clone()), messages);
        inner
            .threads
            .insert((summary.account_id.clone(), summary.id.clone()), summary);
    }

    pub fn outbox_snapshot(&self) -> Vec<OutboxOp> {
        self.inner.lock().unwrap().outbox.clone()
    }
}

#[async_trait]
impl MailStore for InMemoryStore {
    async fn get_thread(
        &self,
        scope: &AccountScope,
        id: &ThreadId,
    ) -> CoreResult<Option<ThreadSummary>> {
        let inner = self.inner.lock().unwrap();
        let mut matches = inner
            .threads
            .values()
            .filter(|thread| thread.id == *id && scope_matches(scope, &thread.account_id));
        let result = matches.next().cloned();
        if matches.next().is_some() {
            return Err(crate::error::CoreError::InvalidAction(format!(
                "thread {id} exists in multiple accounts; choose one account"
            )));
        }
        Ok(result)
    }

    async fn list_threads_in_label(
        &self,
        scope: &AccountScope,
        label: &LabelId,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>> {
        let inner = self.inner.lock().unwrap();
        let mut out: Vec<_> = inner
            .threads
            .values()
            .filter(|t| scope_matches(scope, &t.account_id) && t.label_ids.contains(label))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));
        out.truncate(limit as usize);
        Ok(out)
    }

    async fn list_messages_in_thread(
        &self,
        scope: &AccountScope,
        id: &ThreadId,
    ) -> CoreResult<Vec<Message>> {
        let thread = self.get_thread(scope, id).await?;
        let Some(thread) = thread else {
            return Ok(Vec::new());
        };
        Ok(self
            .inner
            .lock()
            .unwrap()
            .messages_by_thread
            .get(&(thread.account_id, id.clone()))
            .cloned()
            .unwrap_or_default())
    }

    async fn get_message(
        &self,
        scope: &AccountScope,
        id: &MessageId,
    ) -> CoreResult<Option<Message>> {
        let inner = self.inner.lock().unwrap();
        let mut matches = inner
            .messages_by_thread
            .values()
            .flatten()
            .filter(|message| message.id == *id && scope_matches(scope, &message.account_id));
        let result = matches.next().cloned();
        if matches.next().is_some() {
            return Err(crate::error::CoreError::InvalidAction(format!(
                "message {id} exists in multiple accounts; choose one account"
            )));
        }
        Ok(result)
    }

    async fn search_threads(
        &self,
        scope: &AccountScope,
        query: &str,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>> {
        let inner = self.inner.lock().unwrap();
        let q = query.to_lowercase();
        let mut out: Vec<_> = inner
            .threads
            .values()
            .filter(|t| {
                scope_matches(scope, &t.account_id)
                    && (t.subject.to_lowercase().contains(&q)
                        || t.snippet.to_lowercase().contains(&q))
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));
        out.truncate(limit as usize);
        Ok(out)
    }

    async fn apply_thread_mutation(
        &self,
        scope: &AccountScope,
        op_id: &OpId,
        kind: &OutboxOpKind,
    ) -> CoreResult<i64> {
        let (thread_ids, add, remove) = match kind {
            OutboxOpKind::ModifyLabels {
                thread_ids,
                add,
                remove,
            } => (thread_ids.as_slice(), add.as_slice(), remove.as_slice()),
            OutboxOpKind::Trash { thread_ids } => (
                thread_ids.as_slice(),
                &[LabelId::new("TRASH")][..],
                &[LabelId::new("INBOX")][..],
            ),
            _ => {
                return Err(crate::error::CoreError::InvalidAction(
                    "apply_thread_mutation requires a thread operation".into(),
                ))
            }
        };

        let account = resolve_mutation_account(&self.inner, scope, thread_ids)?;
        let mut inner = self.inner.lock().unwrap();
        for tid in thread_ids {
            if let Some(t) = inner.threads.get_mut(&(account.clone(), tid.clone())) {
                for r in remove {
                    t.label_ids.retain(|l| l != r);
                }
                for a in add {
                    if !t.label_ids.contains(a) {
                        t.label_ids.push(a.clone());
                    }
                }
                t.unread = t.label_ids.iter().any(|l| l.as_str() == "UNREAD");
                t.starred = t.label_ids.iter().any(|l| l.as_str() == "STARRED");
            }
        }
        inner.next_outbox_id += 1;
        let id = inner.next_outbox_id;
        inner.outbox.push(OutboxOp {
            id,
            account_id: account,
            op_id: op_id.clone(),
            kind: kind.clone(),
            created_at: Utc::now(),
            attempts: 0,
            last_error: None,
        });
        Ok(id)
    }

    async fn list_labels(&self, scope: &AccountScope) -> CoreResult<Vec<Label>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .labels
            .iter()
            .filter(|label| scope_matches(scope, &label.account_id))
            .cloned()
            .collect())
    }

    async fn get_draft(&self, account: &AccountId, id: &DraftId) -> CoreResult<Option<Draft>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .drafts
            .get(&(account.clone(), id.clone()))
            .cloned())
    }

    async fn find_draft(&self, scope: &AccountScope, id: &DraftId) -> CoreResult<Option<Draft>> {
        let inner = self.inner.lock().unwrap();
        let mut matches = inner
            .drafts
            .values()
            .filter(|draft| draft.id == *id && scope_matches(scope, &draft.account_id));
        let result = matches.next().cloned();
        if matches.next().is_some() {
            return Err(crate::error::CoreError::InvalidAction(format!(
                "draft {id} exists in multiple accounts; choose one account"
            )));
        }
        Ok(result)
    }

    async fn save_draft_local(&self, draft: &Draft) -> CoreResult<()> {
        self.inner
            .lock()
            .unwrap()
            .drafts
            .insert((draft.account_id.clone(), draft.id.clone()), draft.clone());
        Ok(())
    }

    async fn delete_draft_local(&self, account: &AccountId, id: &DraftId) -> CoreResult<()> {
        self.inner
            .lock()
            .unwrap()
            .drafts
            .remove(&(account.clone(), id.clone()));
        Ok(())
    }

    async fn queue_draft_send(
        &self,
        account: &AccountId,
        draft_id: &DraftId,
        op_id: &OpId,
    ) -> CoreResult<i64> {
        let mut inner = self.inner.lock().unwrap();
        if !inner
            .drafts
            .contains_key(&(account.clone(), draft_id.clone()))
        {
            return Err(crate::error::CoreError::NotFound("draft not found".into()));
        }
        inner.next_outbox_id += 1;
        let id = inner.next_outbox_id;
        inner.outbox.push(OutboxOp {
            id,
            account_id: account.clone(),
            op_id: op_id.clone(),
            kind: OutboxOpKind::SendDraft {
                draft_id: draft_id.clone(),
            },
            created_at: Utc::now(),
            attempts: 0,
            last_error: None,
        });
        Ok(id)
    }

    async fn schedule_send(
        &self,
        account: &AccountId,
        draft_id: &DraftId,
        send_at: DateTime<Utc>,
    ) -> CoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner
            .drafts
            .contains_key(&(account.clone(), draft_id.clone()))
        {
            return Err(crate::error::CoreError::NotFound("draft not found".into()));
        }
        inner
            .scheduled_sends
            .insert((account.clone(), draft_id.clone()), send_at);
        Ok(())
    }

    async fn complete_send(
        &self,
        outbox_row_id: i64,
        account: &AccountId,
        draft_id: &DraftId,
    ) -> CoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.outbox.retain(|op| op.id != outbox_row_id);
        inner.drafts.remove(&(account.clone(), draft_id.clone()));
        inner
            .scheduled_sends
            .remove(&(account.clone(), draft_id.clone()));
        Ok(())
    }

    async fn enqueue_outbox(
        &self,
        account: &AccountId,
        op_id: &OpId,
        kind: &OutboxOpKind,
    ) -> CoreResult<i64> {
        let mut inner = self.inner.lock().unwrap();
        inner.next_outbox_id += 1;
        let id = inner.next_outbox_id;
        inner.outbox.push(OutboxOp {
            id,
            account_id: account.clone(),
            op_id: op_id.clone(),
            kind: kind.clone(),
            created_at: Utc::now(),
            attempts: 0,
            last_error: None,
        });
        Ok(id)
    }

    async fn drain_pending_outbox(
        &self,
        account: &AccountId,
        max: u32,
    ) -> CoreResult<Vec<OutboxOp>> {
        let inner = self.inner.lock().unwrap();
        let now = Utc::now().timestamp_millis();
        Ok(inner
            .outbox
            .iter()
            .filter(|op| {
                &op.account_id == account
                    && op.attempts < 5
                    && inner
                        .outbox_next_attempt_at
                        .get(&op.id)
                        .map_or(true, |next| *next <= now)
            })
            .take(max as usize)
            .cloned()
            .collect())
    }

    async fn mark_outbox_done(&self, id: i64) -> CoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.outbox.retain(|o| o.id != id);
        inner.outbox_next_attempt_at.remove(&id);
        Ok(())
    }

    async fn mark_outbox_failed(&self, id: i64, error: &str) -> CoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(op) = inner.outbox.iter_mut().find(|o| o.id == id) {
            op.attempts += 1;
            op.last_error = Some(error.to_string());
            if op.attempts < 5 {
                const BACKOFF_MS: [i64; 5] = [60_000, 300_000, 1_800_000, 7_200_000, 43_200_000];
                let next = Utc::now().timestamp_millis() + BACKOFF_MS[op.attempts as usize - 1];
                inner.outbox_next_attempt_at.insert(id, next);
            }
        }
        Ok(())
    }

    async fn outbox_summary(&self, scope: &AccountScope) -> CoreResult<OutboxSummary> {
        let inner = self.inner.lock().unwrap();
        let relevant: Vec<_> = inner
            .outbox
            .iter()
            .filter(|op| scope_matches(scope, &op.account_id))
            .collect();
        Ok(OutboxSummary {
            pending: relevant.iter().filter(|op| op.attempts < 5).count() as u32,
            failed: relevant.iter().filter(|op| op.attempts >= 5).count() as u32,
            last_error: relevant.iter().rev().find_map(|op| op.last_error.clone()),
        })
    }

    async fn retry_failed_outbox(&self, account: &AccountId) -> CoreResult<u32> {
        let mut inner = self.inner.lock().unwrap();
        let ids: Vec<_> = inner
            .outbox
            .iter_mut()
            .filter(|op| &op.account_id == account && op.attempts >= 5)
            .map(|op| {
                op.attempts = 0;
                op.id
            })
            .collect();
        for id in &ids {
            inner.outbox_next_attempt_at.remove(id);
        }
        Ok(ids.len() as u32)
    }

    async fn list_activity(
        &self,
        scope: &AccountScope,
        since_ms: i64,
        limit: u32,
    ) -> CoreResult<Vec<ActivityEntry>> {
        let inner = self.inner.lock().unwrap();
        let mut entries = inner
            .outbox
            .iter()
            .filter(|op| {
                scope_matches(scope, &op.account_id) && op.created_at.timestamp_millis() >= since_ms
            })
            .map(|op| {
                ActivityEntry::from_outbox(
                    op,
                    if op.attempts >= 5 {
                        "failed"
                    } else {
                        "pending"
                    }
                    .into(),
                    inner.outbox_undone_by.contains_key(&op.id),
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| std::cmp::Reverse((entry.at, entry.id)));
        entries.truncate(limit as usize);
        Ok(entries)
    }

    async fn get_outbox_op(&self, scope: &AccountScope, id: i64) -> CoreResult<Option<OutboxOp>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .outbox
            .iter()
            .find(|op| op.id == id && scope_matches(scope, &op.account_id))
            .cloned())
    }

    async fn mark_outbox_undone(&self, id: i64, undone_by: i64) -> CoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.outbox.iter().any(|op| op.id == id)
            || inner.outbox_undone_by.insert(id, undone_by).is_some()
        {
            return Err(crate::error::CoreError::InvalidAction(format!(
                "activity {id} was not found or is already undone"
            )));
        }
        Ok(())
    }

    async fn get_history_cursor(&self, account: &AccountId) -> CoreResult<Option<u64>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .history_cursors
            .get(account)
            .copied())
    }

    async fn set_history_cursor(&self, account: &AccountId, cursor: u64) -> CoreResult<()> {
        self.inner
            .lock()
            .unwrap()
            .history_cursors
            .insert(account.clone(), cursor);
        Ok(())
    }
}

fn scope_matches(scope: &AccountScope, account: &AccountId) -> bool {
    scope.account().map_or(true, |selected| selected == account)
}

fn resolve_mutation_account(
    inner: &Mutex<Inner>,
    scope: &AccountScope,
    thread_ids: &[ThreadId],
) -> CoreResult<AccountId> {
    if let Some(account) = scope.account() {
        let inner = inner.lock().unwrap();
        if thread_ids.iter().any(|thread_id| {
            !inner
                .threads
                .contains_key(&(account.clone(), thread_id.clone()))
        }) {
            return Err(crate::error::CoreError::NotFound(format!(
                "mutation target in account {account}"
            )));
        }
        return Ok(account.clone());
    }
    let inner = inner.lock().unwrap();
    let mut accounts = inner
        .threads
        .values()
        .filter(|thread| thread_ids.contains(&thread.id))
        .map(|thread| thread.account_id.clone());
    let Some(account) = accounts.next() else {
        return Err(crate::error::CoreError::NotFound(
            "mutation target account".into(),
        ));
    };
    if accounts.any(|candidate| candidate != account) {
        return Err(crate::error::CoreError::InvalidAction(
            "mutation spans multiple accounts; choose one account".into(),
        ));
    }
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, DraftPatch};
    use crate::dispatcher::Dispatcher;
    use crate::store::MessageHeaders;
    use std::sync::Arc;

    fn seed_thread(store: &InMemoryStore, id: &str, labels: &[&str]) {
        let summary = ThreadSummary {
            account_id: AccountId::new("test@example.com"),
            id: ThreadId::new(id),
            subject: format!("subject {id}"),
            snippet: format!("snippet {id}"),
            participants: vec!["alice@example.com".into()],
            last_message_at: Utc::now(),
            message_count: 1,
            unread: labels.contains(&"UNREAD"),
            starred: labels.contains(&"STARRED"),
            label_ids: labels.iter().map(|l| LabelId::new(*l)).collect(),
        };
        store.insert_thread(summary, vec![]);
    }

    fn seed_message(store: &InMemoryStore) -> Message {
        let account_id = AccountId::new("test@example.com");
        let thread_id = ThreadId::new("reply-thread");
        let message = Message {
            account_id: account_id.clone(),
            id: MessageId::new("reply-message"),
            thread_id: thread_id.clone(),
            from: "Alice <alice@example.com>".into(),
            to: vec![account_id.as_str().into()],
            cc: vec![],
            subject: "Question".into(),
            snippet: "Can you help?".into(),
            internal_date: Utc::now(),
            body_plain: Some("Can you help?".into()),
            body_html: None,
            headers: Some(MessageHeaders {
                message_id: Some("<rfc-message@example.com>".into()),
                ..MessageHeaders::default()
            }),
            label_ids: vec![LabelId::new("INBOX")],
            fetched_full: true,
            inline_images: vec![],
        };
        store.insert_thread(
            ThreadSummary {
                account_id,
                id: thread_id,
                subject: message.subject.clone(),
                snippet: message.snippet.clone(),
                participants: vec![message.from.clone()],
                last_message_at: message.internal_date,
                message_count: 1,
                unread: false,
                starred: false,
                label_ids: message.label_ids.clone(),
            },
            vec![message.clone()],
        );
        message
    }

    #[tokio::test]
    async fn compose_save_send_happy_path() {
        let store = Arc::new(InMemoryStore::new());
        let account = AccountId::new("test@example.com");
        let dispatcher = Dispatcher::with_scope(store.clone(), AccountScope::One(account.clone()));

        let composed = dispatcher.execute(Action::ComposeNew).await.unwrap();
        let draft: Draft =
            serde_json::from_value(composed.data.unwrap().get("draft").unwrap().clone()).unwrap();
        assert_eq!(draft.account_id, account);

        let saved = dispatcher
            .execute(Action::SaveDraft {
                draft_id: draft.id.clone(),
                patch: DraftPatch {
                    to: Some(vec!["recipient@example.com".into()]),
                    subject: Some("Hello".into()),
                    body_md: Some("Body".into()),
                    ..DraftPatch::default()
                },
            })
            .await
            .unwrap();
        assert_eq!(saved.changed_drafts, [draft.id.clone()]);

        let sent = dispatcher
            .execute(Action::SendDraft {
                draft_id: draft.id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(sent.message, "queued to send");
        assert!(sent.data.is_none());
        assert!(matches!(
            store.outbox_snapshot().as_slice(),
            [OutboxOp {
                kind: OutboxOpKind::SendDraft { draft_id },
                ..
            }] if draft_id == &draft.id
        ));
    }

    #[tokio::test]
    async fn compose_new_uses_configured_default_for_all_accounts_scope() {
        let store = Arc::new(InMemoryStore::new());
        let missing_default = Dispatcher::new(store.clone())
            .execute(Action::ComposeNew)
            .await
            .unwrap_err();
        assert!(missing_default
            .to_string()
            .contains("no default account is configured"));

        let account = AccountId::new("default@example.com");
        let dispatcher = Dispatcher::new(store).with_default_account(account.clone());
        let outcome = dispatcher.execute(Action::ComposeNew).await.unwrap();
        let draft: Draft = serde_json::from_value(outcome.data.unwrap()["draft"].clone()).unwrap();
        assert_eq!(draft.account_id, account);
    }

    #[tokio::test]
    async fn compose_new_appends_configured_signature() {
        let store = Arc::new(InMemoryStore::new());
        let account = AccountId::new("test@example.com");
        let config = crate::UserConfig {
            signatures: [(account.to_string(), "Test Person".into())].into(),
        };
        let dispatcher =
            Dispatcher::with_scope(store, AccountScope::One(account)).with_user_config(config);

        let outcome = dispatcher.execute(Action::ComposeNew).await.unwrap();
        let draft: Draft = serde_json::from_value(outcome.data.unwrap()["draft"].clone()).unwrap();
        assert_eq!(draft.body_md, "\n\nTest Person");
    }

    #[tokio::test]
    async fn all_accounts_scope_saves_and_sends_draft_from_non_default_account() {
        let store = Arc::new(InMemoryStore::new());
        let default_account = AccountId::new("default@example.com");
        let draft_account = AccountId::new("other@example.com");
        let draft = Draft {
            account_id: draft_account.clone(),
            id: DraftId::new("other-account-draft"),
            gmail_draft_id: None,
            thread_id: None,
            in_reply_to_message_id: None,
            to: vec!["recipient@example.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Before save".into(),
            body_md: String::new(),
            updated_at: Utc::now(),
        };
        store.save_draft_local(&draft).await.unwrap();
        let dispatcher = Dispatcher::new(store.clone()).with_default_account(default_account);

        dispatcher
            .execute(Action::SaveDraft {
                draft_id: draft.id.clone(),
                patch: DraftPatch {
                    subject: Some("After save".into()),
                    ..DraftPatch::default()
                },
            })
            .await
            .unwrap();
        dispatcher
            .execute(Action::SendDraft {
                draft_id: draft.id.clone(),
            })
            .await
            .unwrap();

        assert!(matches!(
            store.outbox_snapshot().as_slice(),
            [OutboxOp {
                account_id,
                kind: OutboxOpKind::SendDraft { draft_id },
                ..
            }] if account_id == &draft_account && draft_id == &draft.id
        ));
    }

    #[tokio::test]
    async fn send_draft_without_recipients_errors() {
        let store = Arc::new(InMemoryStore::new());
        let dispatcher =
            Dispatcher::with_scope(store, AccountScope::One(AccountId::new("test@example.com")));
        let composed = dispatcher.execute(Action::ComposeNew).await.unwrap();
        let draft: Draft =
            serde_json::from_value(composed.data.unwrap().get("draft").unwrap().clone()).unwrap();

        let error = dispatcher
            .execute(Action::SendDraft { draft_id: draft.id })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("draft has no recipients"));
    }

    #[tokio::test]
    async fn reply_draft_keeps_source_thread_and_message_ids() {
        let store = Arc::new(InMemoryStore::new());
        let source = seed_message(&store);
        let dispatcher =
            Dispatcher::with_scope(store, AccountScope::One(source.account_id.clone()));

        let outcome = dispatcher
            .execute(Action::Reply {
                message_id: source.id.clone(),
                all: false,
            })
            .await
            .unwrap();
        let draft: Draft = serde_json::from_value(outcome.data.unwrap()["draft"].clone()).unwrap();
        assert_eq!(draft.thread_id, Some(source.thread_id));
        assert_eq!(draft.in_reply_to_message_id, Some(source.id));
    }

    #[tokio::test]
    async fn reply_places_signature_before_quote() {
        let store = Arc::new(InMemoryStore::new());
        let source = seed_message(&store);
        let config = crate::UserConfig {
            signatures: [("default".into(), "Test Person".into())].into(),
        };
        let dispatcher =
            Dispatcher::with_scope(store, AccountScope::One(source.account_id.clone()))
                .with_user_config(config);

        let outcome = dispatcher
            .execute(Action::Reply {
                message_id: source.id,
                all: false,
            })
            .await
            .unwrap();
        let draft: Draft = serde_json::from_value(outcome.data.unwrap()["draft"].clone()).unwrap();
        assert!(draft.body_md.starts_with("\n\nTest Person\n\nOn "));
        assert!(draft.body_md.ends_with("\n> Can you help?"));
    }

    #[tokio::test]
    async fn reply_draft_on_non_default_account_saves_and_sends_under_all_scope() {
        let store = Arc::new(InMemoryStore::new());
        let source = seed_message(&store); // lives on test@example.com
        let dispatcher = Dispatcher::with_scope(store.clone(), AccountScope::All)
            .with_default_account(AccountId::new("other@example.com"));

        let outcome = dispatcher
            .execute(Action::Reply {
                message_id: source.id.clone(),
                all: false,
            })
            .await
            .unwrap();
        let draft: Draft = serde_json::from_value(outcome.data.unwrap()["draft"].clone()).unwrap();
        assert_eq!(draft.account_id, source.account_id);

        dispatcher
            .execute(Action::SaveDraft {
                draft_id: draft.id.clone(),
                patch: DraftPatch {
                    body_md: Some("thanks!".into()),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        dispatcher
            .execute(Action::SendDraft {
                draft_id: draft.id.clone(),
            })
            .await
            .unwrap();
        let ops = store.outbox_snapshot();
        assert!(ops.iter().any(|op| op.account_id == source.account_id
            && matches!(&op.kind, OutboxOpKind::SendDraft { draft_id } if *draft_id == draft.id)));
    }

    #[tokio::test]
    async fn archive_drops_inbox_and_enqueues_outbox() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX", "UNREAD"]);
        let dispatcher = Dispatcher::new(store.clone());

        let outcome = dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();

        assert_eq!(outcome.action_name, "archive");
        assert_eq!(outcome.changed_threads.len(), 1);
        assert!(outcome.op_id.is_some());

        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!thread.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        let outbox = store.outbox_snapshot();
        assert_eq!(outbox.len(), 1);
        assert!(matches!(outbox[0].kind, OutboxOpKind::ModifyLabels { .. }));
    }

    #[tokio::test]
    async fn undo_activity_restores_archive_and_marks_original() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX"]);
        let dispatcher = Dispatcher::new(store.clone());
        dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();

        dispatcher
            .execute(Action::UndoActivity { outbox_id: 1 })
            .await
            .unwrap();

        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread.label_ids.contains(&LabelId::new("INBOX")));
        let activity = store
            .list_activity(&AccountScope::All, 0, 50)
            .await
            .unwrap();
        assert!(activity.iter().find(|entry| entry.id == 1).unwrap().undone);
    }

    #[tokio::test]
    async fn undo_activity_rejects_send_draft() {
        let store = Arc::new(InMemoryStore::new());
        let account = AccountId::new("test@example.com");
        let dispatcher = Dispatcher::with_scope(store.clone(), AccountScope::One(account));
        let composed = dispatcher.execute(Action::ComposeNew).await.unwrap();
        let draft: Draft = serde_json::from_value(composed.data.unwrap()["draft"].clone()).unwrap();
        dispatcher
            .execute(Action::SaveDraft {
                draft_id: draft.id.clone(),
                patch: DraftPatch {
                    to: Some(vec!["recipient@example.com".into()]),
                    ..Default::default()
                },
            })
            .await
            .unwrap();
        dispatcher
            .execute(Action::SendDraft { draft_id: draft.id })
            .await
            .unwrap();

        let error = dispatcher
            .execute(Action::UndoActivity { outbox_id: 1 })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not undoable"));
    }

    #[tokio::test]
    async fn mark_read_strips_unread() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX", "UNREAD"]);
        let dispatcher = Dispatcher::new(store.clone());

        dispatcher
            .execute(Action::MarkRead {
                thread_ids: vec![ThreadId::new("t1")],
                read: true,
            })
            .await
            .unwrap();

        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!thread.unread);
        assert!(!thread.label_ids.iter().any(|l| l.as_str() == "UNREAD"));
    }

    #[tokio::test]
    async fn empty_thread_ids_rejected() {
        let store = Arc::new(InMemoryStore::new());
        let dispatcher = Dispatcher::new(store);

        let result = dispatcher
            .execute(Action::Archive { thread_ids: vec![] })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn undo_archive_restores_inbox_label() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX", "UNREAD"]);
        let dispatcher = Dispatcher::new(store.clone());

        // Archive → INBOX gone.
        dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        let t = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!t.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        // Undo → INBOX back.
        dispatcher.execute(Action::Undo).await.unwrap();
        let t = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(t.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        // Redo → INBOX gone again.
        dispatcher.execute(Action::Redo).await.unwrap();
        let t = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!t.label_ids.iter().any(|l| l.as_str() == "INBOX"));
    }

    #[tokio::test]
    async fn undo_trash_restores_labels_and_redo_retrashes() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX", "UNREAD"]);
        let dispatcher = Dispatcher::new(store.clone());

        dispatcher
            .execute(Action::Trash {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        dispatcher.execute(Action::Undo).await.unwrap();

        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
        assert!(!thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "TRASH"));
        assert!(store.outbox_snapshot().iter().any(|op| matches!(
            &op.kind,
            OutboxOpKind::ModifyLabels { remove, .. }
                if remove.iter().any(|label| label.as_str() == "TRASH")
        )));

        dispatcher.execute(Action::Redo).await.unwrap();
        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "TRASH"));
    }

    #[tokio::test]
    async fn undo_snooze_restores_inbox_and_removes_snoozed_label() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX"]);
        let dispatcher = Dispatcher::new(store.clone());
        let until = Utc::now() + chrono::Duration::hours(1);
        let snoozed = format!("MACH/Snoozed/{}", until.to_rfc3339());

        dispatcher
            .execute(Action::Snooze {
                thread_ids: vec![ThreadId::new("t1")],
                until,
            })
            .await
            .unwrap();
        dispatcher.execute(Action::Undo).await.unwrap();

        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
        assert!(!thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == snoozed));

        dispatcher.execute(Action::Redo).await.unwrap();
        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == snoozed));
    }

    #[tokio::test]
    async fn mute_archives_and_undo_restores_changed_labels() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX"]);
        let dispatcher = Dispatcher::new(store.clone());

        dispatcher
            .execute(Action::Mute {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "MACH/Muted"));
        let outbox = store.outbox_snapshot();
        assert!(matches!(
            &outbox[0].kind,
            OutboxOpKind::ModifyLabels { add, remove, .. }
                if add == &[LabelId::new("MACH/Muted")] && remove == &[LabelId::new("INBOX")]
        ));

        dispatcher.execute(Action::Undo).await.unwrap();
        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
        assert!(!thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "MACH/Muted"));
    }

    #[tokio::test]
    async fn undo_trash_on_already_trashed_thread_is_a_no_op() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["TRASH"]);
        let dispatcher = Dispatcher::new(store.clone());

        dispatcher
            .execute(Action::Trash {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        let outcome = dispatcher.execute(Action::Undo).await.unwrap();

        assert!(outcome.message.contains("nothing"));
        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(thread.label_ids, vec![LabelId::new("TRASH")]);
        assert_eq!(store.outbox_snapshot().len(), 1);
    }

    #[tokio::test]
    async fn undo_with_empty_stack_returns_friendly_outcome() {
        let store = Arc::new(InMemoryStore::new());
        let dispatcher = Dispatcher::new(store);
        let outcome = dispatcher.execute(Action::Undo).await.unwrap();
        assert_eq!(outcome.action_name, "undo");
        assert!(outcome.message.contains("nothing"));
    }

    #[tokio::test]
    async fn new_mutation_clears_redo_stack() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX", "UNREAD"]);
        seed_thread(&store, "t2", &["INBOX", "UNREAD"]);
        let dispatcher = Dispatcher::new(store.clone());

        // Archive t1, undo it (so it's on redo), then archive t2.
        dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        dispatcher.execute(Action::Undo).await.unwrap();
        dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t2")],
            })
            .await
            .unwrap();

        // Redo should now be empty — the new archive invalidated it.
        let outcome = dispatcher.execute(Action::Redo).await.unwrap();
        assert!(outcome.message.contains("nothing"));
    }

    #[tokio::test]
    async fn concurrent_mutations_are_recorded_as_complete_history_entries() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX"]);
        seed_thread(&store, "t2", &["INBOX"]);
        let dispatcher = Arc::new(Dispatcher::new(store.clone()));

        let first = {
            let dispatcher = Arc::clone(&dispatcher);
            tokio::spawn(async move {
                dispatcher
                    .execute(Action::Archive {
                        thread_ids: vec![ThreadId::new("t1")],
                    })
                    .await
            })
        };
        let second = {
            let dispatcher = Arc::clone(&dispatcher);
            tokio::spawn(async move {
                dispatcher
                    .execute(Action::Archive {
                        thread_ids: vec![ThreadId::new("t2")],
                    })
                    .await
            })
        };

        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        dispatcher.execute(Action::Undo).await.unwrap();
        dispatcher.execute(Action::Undo).await.unwrap();

        for id in ["t1", "t2"] {
            let thread = store
                .get_thread(&AccountScope::All, &ThreadId::new(id))
                .await
                .unwrap()
                .unwrap();
            assert!(thread
                .label_ids
                .iter()
                .any(|label| label.as_str() == "INBOX"));
        }
    }

    #[tokio::test]
    async fn undo_does_not_reverse_a_preexisting_label() {
        let store = Arc::new(InMemoryStore::new());
        seed_thread(&store, "t1", &["INBOX", "STARRED"]);
        let dispatcher = Dispatcher::new(store.clone());

        dispatcher
            .execute(Action::Star {
                thread_ids: vec![ThreadId::new("t1")],
                starred: true,
            })
            .await
            .unwrap();
        let outcome = dispatcher.execute(Action::Undo).await.unwrap();
        assert!(outcome.message.contains("nothing"));

        let thread = store
            .get_thread(&AccountScope::All, &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread.starred);
    }
}
