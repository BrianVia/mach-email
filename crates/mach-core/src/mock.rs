//! In-memory `MailStore` implementation. Production code uses `mach-store`'s
//! SQLite-backed impl; this exists so `mach-core` can test its dispatcher
//! logic without dragging SQLite into the unit tests.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;

use crate::action::OpId;
use crate::error::CoreResult;
use crate::ids::{DraftId, LabelId, ThreadId};
use crate::store::{Draft, Label, MailStore, Message, OutboxOp, OutboxOpKind, ThreadSummary};

#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    threads: HashMap<ThreadId, ThreadSummary>,
    messages_by_thread: HashMap<ThreadId, Vec<Message>>,
    labels: Vec<Label>,
    drafts: HashMap<DraftId, Draft>,
    outbox: Vec<OutboxOp>,
    next_outbox_id: i64,
    history_cursor: Option<u64>,
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
            .insert(summary.id.clone(), messages);
        inner.threads.insert(summary.id.clone(), summary);
    }

    pub fn outbox_snapshot(&self) -> Vec<OutboxOp> {
        self.inner.lock().unwrap().outbox.clone()
    }
}

#[async_trait]
impl MailStore for InMemoryStore {
    async fn get_thread(&self, id: &ThreadId) -> CoreResult<Option<ThreadSummary>> {
        Ok(self.inner.lock().unwrap().threads.get(id).cloned())
    }

    async fn list_threads_in_label(
        &self,
        label: &LabelId,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>> {
        let inner = self.inner.lock().unwrap();
        let mut out: Vec<_> = inner
            .threads
            .values()
            .filter(|t| t.label_ids.contains(label))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));
        out.truncate(limit as usize);
        Ok(out)
    }

    async fn list_messages_in_thread(&self, id: &ThreadId) -> CoreResult<Vec<Message>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .messages_by_thread
            .get(id)
            .cloned()
            .unwrap_or_default())
    }

    async fn search_threads(&self, query: &str, limit: u32) -> CoreResult<Vec<ThreadSummary>> {
        let inner = self.inner.lock().unwrap();
        let q = query.to_lowercase();
        let mut out: Vec<_> = inner
            .threads
            .values()
            .filter(|t| {
                t.subject.to_lowercase().contains(&q) || t.snippet.to_lowercase().contains(&q)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| b.last_message_at.cmp(&a.last_message_at));
        out.truncate(limit as usize);
        Ok(out)
    }

    async fn apply_thread_mutation(&self, op_id: &OpId, kind: &OutboxOpKind) -> CoreResult<i64> {
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

        let mut inner = self.inner.lock().unwrap();
        for tid in thread_ids {
            if let Some(t) = inner.threads.get_mut(tid) {
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
            op_id: op_id.clone(),
            kind: kind.clone(),
            created_at: Utc::now(),
            attempts: 0,
            last_error: None,
        });
        Ok(id)
    }

    async fn list_labels(&self) -> CoreResult<Vec<Label>> {
        Ok(self.inner.lock().unwrap().labels.clone())
    }

    async fn get_draft(&self, id: &DraftId) -> CoreResult<Option<Draft>> {
        Ok(self.inner.lock().unwrap().drafts.get(id).cloned())
    }

    async fn save_draft_local(&self, draft: &Draft) -> CoreResult<()> {
        self.inner
            .lock()
            .unwrap()
            .drafts
            .insert(draft.id.clone(), draft.clone());
        Ok(())
    }

    async fn delete_draft_local(&self, id: &DraftId) -> CoreResult<()> {
        self.inner.lock().unwrap().drafts.remove(id);
        Ok(())
    }

    async fn enqueue_outbox(&self, op_id: &OpId, kind: &OutboxOpKind) -> CoreResult<i64> {
        let mut inner = self.inner.lock().unwrap();
        inner.next_outbox_id += 1;
        let id = inner.next_outbox_id;
        inner.outbox.push(OutboxOp {
            id,
            op_id: op_id.clone(),
            kind: kind.clone(),
            created_at: Utc::now(),
            attempts: 0,
            last_error: None,
        });
        Ok(id)
    }

    async fn drain_pending_outbox(&self, max: u32) -> CoreResult<Vec<OutboxOp>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.outbox.iter().take(max as usize).cloned().collect())
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

    async fn get_history_cursor(&self) -> CoreResult<Option<u64>> {
        Ok(self.inner.lock().unwrap().history_cursor)
    }

    async fn set_history_cursor(&self, cursor: u64) -> CoreResult<()> {
        self.inner.lock().unwrap().history_cursor = Some(cursor);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Action;
    use crate::dispatcher::Dispatcher;
    use std::sync::Arc;

    fn seed_thread(store: &InMemoryStore, id: &str, labels: &[&str]) {
        let summary = ThreadSummary {
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
            .get_thread(&ThreadId::new("t1"))
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
            .get_thread(&ThreadId::new("t1"))
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
            .get_thread(&ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!t.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        // Undo → INBOX back.
        dispatcher.execute(Action::Undo).await.unwrap();
        let t = store
            .get_thread(&ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(t.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        // Redo → INBOX gone again.
        dispatcher.execute(Action::Redo).await.unwrap();
        let t = store
            .get_thread(&ThreadId::new("t1"))
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
            let thread = store.get_thread(&ThreadId::new(id)).await.unwrap().unwrap();
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
            .get_thread(&ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread.starred);
    }
}
