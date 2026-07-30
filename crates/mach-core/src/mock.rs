//! In-memory `MailStore` implementation. Production code uses `mach-store`'s
//! SQLite-backed impl; this exists so `mach-core` can test its dispatcher
//! logic without dragging SQLite into the unit tests.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

use crate::action::OpId;
use crate::error::CoreResult;
use crate::ids::{AccountId, AccountScope, DraftId, LabelId, ThreadId};
use crate::store::{Draft, Label, MailStore, Message, OutboxOp, OutboxOpKind, ThreadSummary};

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
    outbox: Vec<OutboxOp>,
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
        Ok(inner
            .outbox
            .iter()
            .filter(|op| &op.account_id == account)
            .take(max as usize)
            .cloned()
            .collect())
    }

    async fn mark_outbox_done(&self, id: i64) -> CoreResult<()> {
        self.inner.lock().unwrap().outbox.retain(|o| o.id != id);
        Ok(())
    }

    async fn mark_outbox_failed(&self, id: i64, error: &str) -> CoreResult<()> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(op) = inner.outbox.iter_mut().find(|o| o.id == id) {
            op.attempts += 1;
            op.last_error = Some(error.to_string());
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
    use crate::action::Action;
    use crate::dispatcher::Dispatcher;
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
