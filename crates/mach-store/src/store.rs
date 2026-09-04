use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use mach_core::{
    action::OpId,
    error::{CoreError, CoreResult},
    ids::{AccountId, AccountScope, DraftId, LabelId, MessageId, ThreadId},
    search_query::SearchQuery,
    store::{
        Draft, Label, MailStore, Message, MessageHeaders, OutboxOp, OutboxOpKind, OutboxSummary,
        ThreadSummary,
    },
};
use rusqlite::{params, params_from_iter, types::Value, OptionalExtension, Row};
use tokio::task::spawn_blocking;

use crate::DbPool;

/// SQLite-backed [`MailStore`]. All trait methods wrap the (synchronous)
/// rusqlite calls in `spawn_blocking` so they can be safely awaited from a
/// tokio task without parking the runtime.
///
/// `prepare_cached` is used on every hot-path query — the prepared-statement
/// cache amortizes parsing across the inbox-render loop.
pub struct SqliteStore {
    pool: DbPool,
}

impl SqliteStore {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &DbPool {
        &self.pool
    }

    pub async fn recent_done_outbox(
        &self,
        account: &AccountId,
        since_ms: i64,
    ) -> CoreResult<Vec<OutboxOp>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> CoreResult<Vec<OutboxOp>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, account_id, op_id, op_kind, payload_json, created_at,
                            attempts, last_error
                     FROM outbox
                     WHERE account_id = ?1 AND state = 'done' AND completed_at >= ?2",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account.as_str(), since_ms], row_to_outbox)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }
}

/// Inputs the sync engine writes during bootstrap / incremental updates.
/// Lives outside the `MailStore` trait because the trait is consumer-facing
/// (mutations go through `Action`s); these are producer-facing.
#[derive(Debug, Clone)]
pub struct LabelUpsert {
    pub id: String,
    pub name: String,
    pub system: bool,
    pub color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ThreadUpsert {
    pub id: String,
    pub history_id: u64,
    pub subject: String,
    pub snippet: String,
    pub participants: Vec<String>,
    pub last_message_at_ms: i64,
    pub label_ids: Vec<String>,
    pub messages: Vec<MessageUpsert>,
}

#[derive(Debug, Clone)]
pub struct MessageUpsert {
    pub id: String,
    pub thread_id: String,
    pub history_id: u64,
    pub internal_date_ms: i64,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub snippet: String,
    pub label_ids: Vec<String>,
    pub body_plain: Option<String>,
    pub headers_json: Option<String>,
}

/// Inputs the body fetcher writes when format=full lands. We only touch the
/// body columns + `fetched_full`; the rest of the row was populated at
/// bootstrap and stays canonical.
#[derive(Debug, Clone)]
pub struct MessageBodyUpdate {
    pub id: String,
    pub body_plain: Option<String>,
    pub body_html: Option<String>,
    /// JSON-encoded `Vec<InlineImageRef>` if the message has inline images.
    pub inline_images_json: Option<String>,
}

impl SqliteStore {
    /// Assign rows imported from the pre-multi-account schema to the only
    /// credential that existed at upgrade time. Safe to call repeatedly.
    pub async fn claim_legacy_account(&self, account: &AccountId) -> CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            for table in ["threads", "labels", "drafts", "outbox", "sync_state"] {
                tx.execute(
                    &format!("UPDATE {table} SET account_id = ?1 WHERE account_id = '__legacy__'"),
                    params![account.as_str()],
                )
                .map_err(map_err)?;
            }
            tx.commit().map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    /// Replace the labels table contents wholesale. Cheaper than diffing on
    /// bootstrap; for incremental updates the sync engine should patch.
    pub async fn upsert_labels(
        &self,
        account: &AccountId,
        labels: Vec<LabelUpsert>,
    ) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            for l in labels {
                tx.execute(
                    "INSERT INTO labels (account_id, id, name, type, color)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(account_id, id) DO UPDATE SET
                       name  = excluded.name,
                       type  = excluded.type,
                       color = excluded.color",
                    params![
                        account.as_str(),
                        l.id,
                        l.name,
                        if l.system { "system" } else { "user" },
                        l.color,
                    ],
                )
                .map_err(map_err)?;
            }
            tx.commit().map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    /// Insert or replace threads + their messages. The thread row's
    /// `unread`/`starred` are derived from the union of its messages' labels;
    /// we compute that here so the inbox-list query stays lookup-free.
    pub async fn upsert_threads(
        &self,
        account: &AccountId,
        threads: Vec<ThreadUpsert>,
    ) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            for t in threads {
                let unread = t.label_ids.iter().any(|l| l == "UNREAD");
                let starred = t.label_ids.iter().any(|l| l == "STARRED");
                let participants_json = serde_json::to_string(&t.participants)?;
                let label_ids_json = serde_json::to_string(&t.label_ids)?;
                let now = Utc::now().timestamp_millis();
                tx.execute(
                    "INSERT INTO threads (account_id, id, history_id, snippet, subject, participants_json,
                                          last_message_at, message_count, unread, starred,
                                          label_ids_json, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT(account_id, id) DO UPDATE SET
                       history_id        = excluded.history_id,
                       snippet           = excluded.snippet,
                       subject           = excluded.subject,
                       participants_json = excluded.participants_json,
                       last_message_at   = excluded.last_message_at,
                       message_count     = excluded.message_count,
                       unread            = excluded.unread,
                       starred           = excluded.starred,
                       label_ids_json    = excluded.label_ids_json,
                       updated_at        = excluded.updated_at",
                    params![
                        account.as_str(),
                        t.id,
                        t.history_id as i64,
                        t.snippet,
                        t.subject,
                        participants_json,
                        t.last_message_at_ms,
                        t.messages.len() as i64,
                        unread as i64,
                        starred as i64,
                        label_ids_json,
                        now,
                    ],
                )
                .map_err(map_err)?;

                for m in &t.messages {
                    let to_json = serde_json::to_string(&m.to)?;
                    let cc_json = if m.cc.is_empty() {
                        None
                    } else {
                        Some(serde_json::to_string(&m.cc)?)
                    };
                    let label_json = serde_json::to_string(&m.label_ids)?;
                    tx.execute(
                        "INSERT INTO messages (account_id, id, thread_id, history_id, internal_date,
                                               from_addr, to_addrs, cc_addrs, subject,
                                               snippet, body_plain, headers_json, label_ids_json,
                                               fetched_full)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                         ON CONFLICT(account_id, id) DO UPDATE SET
                           history_id     = excluded.history_id,
                           internal_date  = excluded.internal_date,
                           from_addr      = excluded.from_addr,
                           to_addrs       = excluded.to_addrs,
                           cc_addrs       = excluded.cc_addrs,
                           subject        = excluded.subject,
                           snippet        = excluded.snippet,
                           body_plain     = COALESCE(excluded.body_plain, messages.body_plain),
                           headers_json   = excluded.headers_json,
                           label_ids_json = excluded.label_ids_json,
                           fetched_full   = MAX(excluded.fetched_full, messages.fetched_full)",
                        params![
                            account.as_str(),
                            m.id,
                            m.thread_id,
                            m.history_id as i64,
                            m.internal_date_ms,
                            m.from,
                            to_json,
                            cc_json,
                            m.subject,
                            m.snippet,
                            m.body_plain,
                            m.headers_json,
                            label_json,
                            m.body_plain.is_some() as i64,
                        ],
                    )
                    .map_err(map_err)?;
                }
            }
            tx.commit().map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    /// Remove messages explicitly reported as deleted by Gmail history.
    ///
    /// The caller subsequently refreshes every affected thread, so this owns
    /// only the durable deletion—not reconstruction of the thread summary.
    pub async fn delete_messages(
        &self,
        account: &AccountId,
        message_ids: Vec<String>,
    ) -> mach_core::CoreResult<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            for id in message_ids {
                tx.execute(
                    "DELETE FROM messages WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), id],
                )
                .map_err(map_err)?;
            }
            tx.commit().map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    /// Remove a thread that Gmail confirms no longer exists. Message,
    /// attachment, and FTS cleanup follows the schema's cascades/triggers.
    pub async fn delete_thread(
        &self,
        account: &AccountId,
        thread_id: String,
    ) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "DELETE FROM threads WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), thread_id],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }
}

/// One thread whose `MACH/Snoozed/<rfc3339>` label is past its time. The
/// scheduler reads these in `find_due_snoozes` and un-snoozes them.
#[derive(Debug, Clone)]
pub struct DueSnooze {
    pub account_id: AccountId,
    pub thread_id: String,
    pub snoozed_label: String,
}

/// One scheduled send. `send_later_due` returns these in `find_due_sends`.
#[derive(Debug, Clone)]
pub struct DueSend {
    pub account_id: AccountId,
    pub send_later_id: String,
    pub draft_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStateCount {
    pub state: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetter {
    pub id: i64,
    pub op_kind: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub account_id: AccountId,
    pub id: i64,
    pub kind: String,
    pub attempts: u32,
    pub state: String,
    pub last_error: Option<String>,
}

impl SqliteStore {
    pub async fn list_outbox(
        &self,
        scope: &AccountScope,
    ) -> mach_core::CoreResult<Vec<OutboxEntry>> {
        let pool = self.pool.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> mach_core::CoreResult<Vec<OutboxEntry>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, op_kind, attempts, state, last_error
                     FROM outbox WHERE ?1 IS NULL OR account_id = ?1
                     ORDER BY account_id, id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account], |row| {
                    Ok(OutboxEntry {
                        account_id: AccountId::new(row.get::<_, String>(0)?),
                        id: row.get(1)?,
                        kind: row.get(2)?,
                        attempts: row.get::<_, i64>(3)? as u32,
                        state: row.get(4)?,
                        last_error: row.get(5)?,
                    })
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    pub async fn draft_state_counts(
        &self,
        account: &AccountId,
    ) -> mach_core::CoreResult<Vec<DraftStateCount>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<Vec<DraftStateCount>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT state, COUNT(*) FROM drafts
                     WHERE account_id = ?1 GROUP BY state ORDER BY state",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account.as_str()], |row| {
                    Ok(DraftStateCount {
                        state: row.get(0)?,
                        count: row.get::<_, i64>(1)? as u32,
                    })
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    pub async fn dead_letters(
        &self,
        account: &AccountId,
    ) -> mach_core::CoreResult<Vec<DeadLetter>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<Vec<DeadLetter>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, op_kind, last_error FROM outbox
                     WHERE account_id = ?1 AND state = 'failed' ORDER BY id",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account.as_str()], |row| {
                    Ok(DeadLetter {
                        id: row.get(0)?,
                        op_kind: row.get(1)?,
                        last_error: row.get(2)?,
                    })
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    /// Return all snoozed threads whose `MACH/Snoozed/<rfc3339>` label is
    /// past `now`. Walks every thread row, parses the JSON label array,
    /// extracts the matching label. O(threads) — fine at personal scale.
    pub async fn find_due_snoozes(
        &self,
        account: &AccountId,
        now_ms: i64,
    ) -> mach_core::CoreResult<Vec<DueSnooze>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<Vec<DueSnooze>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached("SELECT id, label_ids_json FROM threads WHERE account_id = ?1")
                .map_err(map_err)?;
            let mut due = Vec::new();
            let rows = stmt
                .query_map(params![account.as_str()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            for row in rows {
                let (id, labels_json) = row.map_err(map_err)?;
                let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                for l in labels {
                    let Some(rest) = l.strip_prefix("MACH/Snoozed/") else {
                        continue;
                    };
                    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(rest) else {
                        continue;
                    };
                    if ts.timestamp_millis() <= now_ms {
                        due.push(DueSnooze {
                            account_id: account.clone(),
                            thread_id: id.clone(),
                            snoozed_label: l.clone(),
                        });
                    }
                }
            }
            Ok(due)
        })
        .await
        .map_err(map_err)?
    }

    /// Return scheduled sends whose `send_at` is past `now`.
    pub async fn find_due_sends(
        &self,
        account: &AccountId,
        now_ms: i64,
    ) -> mach_core::CoreResult<Vec<DueSend>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<Vec<DueSend>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, draft_id FROM send_later
                     WHERE account_id = ?1 AND state = 'scheduled' AND send_at <= ?2",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account.as_str(), now_ms], |r| {
                    Ok(DueSend {
                        account_id: account.clone(),
                        send_later_id: r.get::<_, String>(0)?,
                        draft_id: r.get::<_, String>(1)?,
                    })
                })
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    /// Mark a `send_later` row done (sent) or failed.
    pub async fn mark_send_later(
        &self,
        account: &AccountId,
        id: &str,
        state: &str,
    ) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        let id = id.to_string();
        let state = state.to_string();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "UPDATE send_later SET state = ?1 WHERE account_id = ?2 AND id = ?3",
                params![state, account.as_str(), id],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    /// Invalidate all message bodies for a thread. Used by force-refresh
    /// so the next `open_thread` re-fetches from Gmail with the latest
    /// parser and inline-image extraction.
    pub async fn invalidate_thread_bodies(
        &self,
        account: &AccountId,
        thread_id: &mach_core::ids::ThreadId,
    ) -> mach_core::CoreResult<usize> {
        let pool = self.pool.clone();
        let account = account.clone();
        let tid = thread_id.as_str().to_string();
        spawn_blocking(move || -> mach_core::CoreResult<usize> {
            let conn = pool.get().map_err(map_err)?;
            let n = conn
                .execute(
                    "UPDATE messages
                       SET body_plain         = NULL,
                           body_html          = NULL,
                           inline_images_json = NULL,
                           fetched_full       = 0
                     WHERE account_id = ?1 AND thread_id = ?2",
                    params![account.as_str(), tid],
                )
                .map_err(map_err)?;
            Ok(n)
        })
        .await
        .map_err(map_err)?
    }

    /// Persist full-body fetches. UPDATE-only — the message row already
    /// exists from bootstrap. Marks `fetched_full = 1` so the body fetcher
    /// short-circuits next time. FTS5 index is kept in sync via the `messages_au`
    /// trigger.
    pub async fn update_message_bodies(
        &self,
        account: &AccountId,
        updates: Vec<MessageBodyUpdate>,
    ) -> mach_core::CoreResult<usize> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> mach_core::CoreResult<usize> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            let mut touched = 0usize;
            for u in &updates {
                let n = tx
                    .execute(
                        "UPDATE messages
                         SET body_plain         = ?1,
                             body_html          = ?2,
                             inline_images_json = ?3,
                             fetched_full       = 1
                         WHERE account_id = ?4 AND id = ?5",
                        params![
                            u.body_plain,
                            u.body_html,
                            u.inline_images_json,
                            account.as_str(),
                            u.id
                        ],
                    )
                    .map_err(map_err)?;
                touched += n;
            }
            tx.commit().map_err(map_err)?;
            Ok(touched)
        })
        .await
        .map_err(map_err)?
    }
}

fn map_err<E: std::fmt::Display>(e: E) -> CoreError {
    CoreError::Storage(e.to_string())
}

fn dt_to_ms(dt: &DateTime<Utc>) -> i64 {
    dt.timestamp_millis()
}

fn ms_to_dt(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Utc::now)
}

fn outbox_kind_name(kind: &OutboxOpKind) -> &'static str {
    match kind {
        OutboxOpKind::ModifyLabels { .. } => "modify_labels",
        OutboxOpKind::Trash { .. } => "trash",
        OutboxOpKind::SendDraft { .. } => "send_draft",
        OutboxOpKind::SaveDraft { .. } => "save_draft",
        OutboxOpKind::DeleteDraft { .. } => "delete_draft",
    }
}

fn row_to_thread(row: &Row) -> rusqlite::Result<ThreadSummary> {
    let participants_json: String = row.get("participants_json")?;
    let label_ids_json: String = row.get("label_ids_json")?;
    Ok(ThreadSummary {
        account_id: AccountId::new(row.get::<_, String>("account_id")?),
        id: ThreadId::new(row.get::<_, String>("id")?),
        subject: row.get("subject")?,
        snippet: row.get("snippet")?,
        participants: serde_json::from_str(&participants_json).unwrap_or_default(),
        last_message_at: ms_to_dt(row.get("last_message_at")?),
        message_count: row.get::<_, i64>("message_count")? as u32,
        unread: row.get::<_, i64>("unread")? != 0,
        starred: row.get::<_, i64>("starred")? != 0,
        label_ids: serde_json::from_str(&label_ids_json).unwrap_or_default(),
    })
}

fn row_to_message(row: &Row) -> rusqlite::Result<Message> {
    use mach_core::store::InlineImageRow;
    let to_addrs: String = row.get("to_addrs")?;
    let cc_addrs: Option<String> = row.get("cc_addrs")?;
    let label_ids_json: String = row.get("label_ids_json")?;
    let inline_images_json: Option<String> = row.get("inline_images_json").ok();
    let inline_images: Vec<InlineImageRow> = inline_images_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let headers_json: Option<String> = row.get("headers_json")?;
    Ok(Message {
        account_id: AccountId::new(row.get::<_, String>("account_id")?),
        id: MessageId::new(row.get::<_, String>("id")?),
        thread_id: ThreadId::new(row.get::<_, String>("thread_id")?),
        from: row.get("from_addr")?,
        to: serde_json::from_str(&to_addrs).unwrap_or_default(),
        cc: cc_addrs
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default(),
        subject: row.get("subject")?,
        snippet: row.get("snippet")?,
        internal_date: ms_to_dt(row.get("internal_date")?),
        body_plain: row.get("body_plain")?,
        body_html: row.get("body_html")?,
        headers: headers_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<MessageHeaders>(value).ok()),
        label_ids: serde_json::from_str(&label_ids_json).unwrap_or_default(),
        fetched_full: row.get::<_, i64>("fetched_full").unwrap_or(0) != 0,
        inline_images,
    })
}

fn row_to_label(row: &Row) -> rusqlite::Result<Label> {
    let label_type: String = row.get("type")?;
    Ok(Label {
        account_id: AccountId::new(row.get::<_, String>("account_id")?),
        id: LabelId::new(row.get::<_, String>("id")?),
        name: row.get("name")?,
        system: label_type == "system",
        color: row.get("color")?,
        unread_count: row.get::<_, Option<i64>>("unread_count")?.map(|v| v as u32),
    })
}

fn row_to_draft(row: &Row) -> rusqlite::Result<Draft> {
    let to_addrs: String = row.get("to_addrs")?;
    let cc_addrs: String = row.get("cc_addrs")?;
    let bcc_addrs: String = row.get("bcc_addrs")?;
    Ok(Draft {
        account_id: AccountId::new(row.get::<_, String>("account_id")?),
        id: DraftId::new(row.get::<_, String>("id")?),
        gmail_draft_id: row.get("gmail_draft_id")?,
        thread_id: row
            .get::<_, Option<String>>("thread_id")?
            .map(ThreadId::new),
        in_reply_to_message_id: row
            .get::<_, Option<String>>("in_reply_to_message_id")?
            .map(MessageId::new),
        to: serde_json::from_str(&to_addrs).unwrap_or_default(),
        cc: serde_json::from_str(&cc_addrs).unwrap_or_default(),
        bcc: serde_json::from_str(&bcc_addrs).unwrap_or_default(),
        subject: row.get("subject")?,
        body_md: row.get("body_md")?,
        updated_at: ms_to_dt(row.get("updated_at")?),
    })
}

fn row_to_outbox(row: &Row) -> rusqlite::Result<OutboxOp> {
    let payload_json: String = row.get("payload_json")?;
    let kind: OutboxOpKind = serde_json::from_str(&payload_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(OutboxOp {
        id: row.get("id")?,
        account_id: AccountId::new(row.get::<_, String>("account_id")?),
        op_id: OpId(row.get::<_, String>("op_id")?),
        kind,
        created_at: ms_to_dt(row.get("created_at")?),
        attempts: row.get::<_, i64>("attempts")? as u32,
        last_error: row.get("last_error")?,
    })
}

#[async_trait]
impl MailStore for SqliteStore {
    async fn get_thread(
        &self,
        scope: &AccountScope,
        id: &ThreadId,
    ) -> CoreResult<Option<ThreadSummary>> {
        let pool = self.pool.clone();
        let id = id.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> CoreResult<Option<ThreadSummary>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, subject, snippet, participants_json, last_message_at,
                            message_count, unread, starred, label_ids_json
                     FROM threads
                     WHERE id = ?1 AND (?2 IS NULL OR account_id = ?2)
                     LIMIT 2",
                )
                .map_err(map_err)?;
            let mut rows = stmt
                .query_map(params![id.as_str(), account], row_to_thread)
                .map_err(map_err)?;
            let first = rows.next().transpose().map_err(map_err)?;
            if rows.next().transpose().map_err(map_err)?.is_some() {
                return Err(CoreError::InvalidAction(format!(
                    "thread {id} exists in multiple accounts; choose one account"
                )));
            }
            Ok(first)
        })
        .await
        .map_err(map_err)?
    }

    async fn list_threads_in_label(
        &self,
        scope: &AccountScope,
        label: &LabelId,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>> {
        let pool = self.pool.clone();
        let label = label.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> CoreResult<Vec<ThreadSummary>> {
            let conn = pool.get().map_err(map_err)?;
            // DONE is virtual: Gmail represents archived mail by removing INBOX,
            // rather than by adding an archive label.
            if label.as_str() == "DONE" {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT account_id, id, subject, snippet, participants_json, last_message_at,
                                message_count, unread, starred, label_ids_json
                         FROM threads
                         WHERE label_ids_json NOT LIKE '%\"INBOX\"%'
                           AND label_ids_json NOT LIKE '%\"TRASH\"%'
                           AND label_ids_json NOT LIKE '%\"SPAM\"%'
                           AND label_ids_json NOT LIKE '%\"DRAFT\"%'
                           AND (?2 IS NULL OR account_id = ?2)
                         ORDER BY last_message_at DESC
                         LIMIT ?3",
                    )
                    .map_err(map_err)?;
                let rows = stmt
                    .query_map(params![label.as_str(), account, limit as i64], row_to_thread)
                    .map_err(map_err)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(map_err)?;
                return Ok(rows);
            }
            // Filter via JSON containment. With label-id strings always quoted
            // in the JSON array, a LIKE check is safe — no false positives.
            let needle = format!("\"{}\"", label.as_str());
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, subject, snippet, participants_json, last_message_at,
                            message_count, unread, starred, label_ids_json
                     FROM threads
                     WHERE label_ids_json LIKE '%' || ?1 || '%'
                       AND (?2 IS NULL OR account_id = ?2)
                     ORDER BY last_message_at DESC
                     LIMIT ?3",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![needle, account, limit as i64], row_to_thread)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
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
        let pool = self.pool.clone();
        let id = id.clone();
        let account = thread.account_id;
        spawn_blocking(move || -> CoreResult<Vec<Message>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, thread_id, internal_date, from_addr, to_addrs, cc_addrs,
                            subject, snippet, body_plain, body_html, headers_json, label_ids_json,
                            fetched_full, inline_images_json
                     FROM messages
                     WHERE account_id = ?1 AND thread_id = ?2
                     ORDER BY internal_date",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account.as_str(), id.as_str()], row_to_message)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn get_message(
        &self,
        scope: &AccountScope,
        id: &MessageId,
    ) -> CoreResult<Option<Message>> {
        let pool = self.pool.clone();
        let id = id.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> CoreResult<Option<Message>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, thread_id, internal_date, from_addr, to_addrs, cc_addrs,
                            subject, snippet, body_plain, body_html, headers_json, label_ids_json,
                            fetched_full, inline_images_json
                     FROM messages
                     WHERE id = ?1 AND (?2 IS NULL OR account_id = ?2)
                     LIMIT 2",
                )
                .map_err(map_err)?;
            let mut rows = stmt
                .query_map(params![id.as_str(), account], row_to_message)
                .map_err(map_err)?;
            let first = rows.next().transpose().map_err(map_err)?;
            if rows.next().transpose().map_err(map_err)?.is_some() {
                return Err(CoreError::InvalidAction(format!(
                    "message {id} exists in multiple accounts; choose one account"
                )));
            }
            Ok(first)
        })
        .await
        .map_err(map_err)?
    }

    async fn search_threads(
        &self,
        scope: &AccountScope,
        query: &str,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>> {
        let pool = self.pool.clone();
        let account = scope.account().map(|value| value.as_str().to_owned());
        let query = SearchQuery::parse(query);
        spawn_blocking(move || -> CoreResult<Vec<ThreadSummary>> {
            let conn = pool.get().map_err(map_err)?;
            let mut sql = String::from(
                "SELECT t.account_id, t.id, t.subject, t.snippet, t.participants_json, t.last_message_at,
                        t.message_count, t.unread, t.starred, t.label_ids_json ",
            );
            let mut values = Vec::new();
            if let Some(fts) = query.to_fts5() {
                sql.push_str(
                    "FROM messages_fts f
                     JOIN messages m ON m.rowid = f.rowid
                     JOIN threads t ON t.account_id = m.account_id AND t.id = m.thread_id
                     WHERE messages_fts MATCH ? ",
                );
                values.push(Value::Text(fts));
            } else {
                sql.push_str("FROM threads t WHERE 1 = 1 ");
            }
            if let Some(account) = account {
                sql.push_str("AND t.account_id = ? ");
                values.push(Value::Text(account));
            }
            if let Some(unread) = query.is_unread {
                sql.push_str("AND t.unread = ? ");
                values.push(Value::Integer(unread.into()));
            }
            if let Some(starred) = query.is_starred {
                sql.push_str("AND t.starred = ? ");
                values.push(Value::Integer(starred.into()));
            }
            for label in query.labels {
                sql.push_str(
                    "AND EXISTS (
                       SELECT 1 FROM json_each(t.label_ids_json) j
                       LEFT JOIN labels l ON l.account_id = t.account_id AND l.id = j.value
                       WHERE j.value = ? OR l.name = ? COLLATE NOCASE
                     ) ",
                );
                values.push(Value::Text(label.clone()));
                values.push(Value::Text(label));
            }
            let now = Utc::now().timestamp_millis();
            if let Some(days) = query.newer_than_days {
                sql.push_str("AND t.last_message_at >= ? ");
                values.push(Value::Integer(now - i64::from(days) * 86_400_000));
            }
            if let Some(days) = query.older_than_days {
                sql.push_str("AND t.last_message_at <= ? ");
                values.push(Value::Integer(now - i64::from(days) * 86_400_000));
            }
            if query.has_attachment {
                sql.push_str(
                    "AND EXISTS (
                       SELECT 1 FROM attachments a
                       JOIN messages m2 ON m2.account_id = a.account_id AND m2.id = a.message_id
                       WHERE m2.account_id = t.account_id AND m2.thread_id = t.id
                     ) ",
                );
            }
            sql.push_str("GROUP BY t.account_id, t.id ORDER BY t.last_message_at DESC LIMIT ?");
            values.push(Value::Integer(limit.into()));
            let mut stmt = conn.prepare_cached(&sql).map_err(map_err)?;
            let rows = stmt
                .query_map(params_from_iter(values), row_to_thread)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn apply_thread_mutation(
        &self,
        scope: &AccountScope,
        op_id: &OpId,
        kind: &OutboxOpKind,
    ) -> CoreResult<i64> {
        let pool = self.pool.clone();
        let selected_account = scope.account().cloned();
        let op_id = op_id.clone();
        let kind = kind.clone();
        spawn_blocking(move || -> CoreResult<i64> {
            let (thread_ids, add, remove) = match &kind {
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
                    return Err(CoreError::InvalidAction(
                        "apply_thread_mutation requires a thread operation".into(),
                    ))
                }
            };
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            let account = match selected_account {
                Some(account) => account,
                None => {
                    let mut resolved: Option<AccountId> = None;
                    for tid in thread_ids {
                        let mut stmt = tx
                            .prepare_cached(
                                "SELECT account_id FROM threads WHERE id = ?1 LIMIT 2",
                            )
                            .map_err(map_err)?;
                        let accounts = stmt
                            .query_map(params![tid.as_str()], |row| row.get::<_, String>(0))
                            .map_err(map_err)?
                            .collect::<rusqlite::Result<Vec<_>>>()
                            .map_err(map_err)?;
                        if accounts.len() != 1 {
                            return Err(CoreError::InvalidAction(format!(
                                "thread {tid} is missing or exists in multiple accounts; choose one account"
                            )));
                        }
                        let candidate = AccountId::new(accounts[0].clone());
                        if resolved
                            .as_ref()
                            .is_some_and(|existing| existing != &candidate)
                        {
                            return Err(CoreError::InvalidAction(
                                "mutation spans multiple accounts; choose one account".into(),
                            ));
                        }
                        resolved = Some(candidate);
                    }
                    resolved.ok_or_else(|| {
                        CoreError::InvalidAction("mutation has no target account".into())
                    })?
                }
            };
            for tid in thread_ids {
                let current_json: Option<String> = tx
                    .query_row(
                        "SELECT label_ids_json FROM threads
                         WHERE account_id = ?1 AND id = ?2",
                        params![account.as_str(), tid.as_str()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(map_err)?;
                let mut labels: Vec<LabelId> = match current_json {
                    Some(s) => serde_json::from_str(&s).unwrap_or_default(),
                    None => {
                        return Err(CoreError::NotFound(format!(
                            "thread {tid} in account {account}"
                        )))
                    }
                };
                for r in remove {
                    labels.retain(|l| l != r);
                }
                for a in add {
                    if !labels.contains(a) {
                        labels.push(a.clone());
                    }
                }
                let unread = labels.iter().any(|l| l.as_str() == "UNREAD");
                let starred = labels.iter().any(|l| l.as_str() == "STARRED");
                let labels_json = serde_json::to_string(&labels)?;
                tx.execute(
                    "UPDATE threads
                     SET label_ids_json = ?1, unread = ?2, starred = ?3, updated_at = ?4
                     WHERE account_id = ?5 AND id = ?6",
                    params![
                        labels_json,
                        unread as i64,
                        starred as i64,
                        Utc::now().timestamp_millis(),
                        account.as_str(),
                        tid.as_str()
                    ],
                )
                .map_err(map_err)?;
            }

            let kind_name = outbox_kind_name(&kind);
            let payload_json = serde_json::to_string(&kind)?;
            tx.execute(
                "INSERT INTO outbox
                   (account_id, op_id, op_kind, payload_json, created_at, attempts, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 'pending')",
                params![
                    account.as_str(),
                    op_id.0,
                    kind_name,
                    payload_json,
                    Utc::now().timestamp_millis()
                ],
            )
            .map_err(map_err)?;
            let outbox_id = tx.last_insert_rowid();
            tx.commit().map_err(map_err)?;
            Ok(outbox_id)
        })
        .await
        .map_err(map_err)?
    }

    async fn list_labels(&self, scope: &AccountScope) -> CoreResult<Vec<Label>> {
        let pool = self.pool.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> CoreResult<Vec<Label>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, name, type, color, unread_count, total_count
                     FROM labels
                     WHERE ?1 IS NULL OR account_id = ?1
                     ORDER BY account_id, name",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![account], row_to_label)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn get_draft(&self, account: &AccountId, id: &DraftId) -> CoreResult<Option<Draft>> {
        let pool = self.pool.clone();
        let account = account.clone();
        let id = id.clone();
        spawn_blocking(move || -> CoreResult<Option<Draft>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, gmail_draft_id, thread_id, in_reply_to_message_id,
                            to_addrs, cc_addrs, bcc_addrs, subject, body_md, updated_at
                     FROM drafts WHERE account_id = ?1 AND id = ?2",
                )
                .map_err(map_err)?;
            stmt.query_row(params![account.as_str(), id.as_str()], row_to_draft)
                .optional()
                .map_err(map_err)
        })
        .await
        .map_err(map_err)?
    }

    async fn find_draft(&self, scope: &AccountScope, id: &DraftId) -> CoreResult<Option<Draft>> {
        let pool = self.pool.clone();
        let id = id.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> CoreResult<Option<Draft>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT account_id, id, gmail_draft_id, thread_id, in_reply_to_message_id,
                            to_addrs, cc_addrs, bcc_addrs, subject, body_md, updated_at
                     FROM drafts
                     WHERE id = ?1 AND (?2 IS NULL OR account_id = ?2)
                     LIMIT 2",
                )
                .map_err(map_err)?;
            let mut rows = stmt
                .query_map(params![id.as_str(), account], row_to_draft)
                .map_err(map_err)?;
            let first = rows.next().transpose().map_err(map_err)?;
            if rows.next().transpose().map_err(map_err)?.is_some() {
                return Err(CoreError::InvalidAction(format!(
                    "draft {id} exists in multiple accounts; choose one account"
                )));
            }
            Ok(first)
        })
        .await
        .map_err(map_err)?
    }

    async fn save_draft_local(&self, draft: &Draft) -> CoreResult<()> {
        let pool = self.pool.clone();
        let draft = draft.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "INSERT INTO drafts (account_id, id, gmail_draft_id, thread_id, in_reply_to_message_id,
                                     to_addrs, cc_addrs, bcc_addrs, subject, body_md,
                                     updated_at, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'draft')
                 ON CONFLICT(account_id, id) DO UPDATE SET
                   gmail_draft_id         = excluded.gmail_draft_id,
                   thread_id              = excluded.thread_id,
                   in_reply_to_message_id = excluded.in_reply_to_message_id,
                   to_addrs               = excluded.to_addrs,
                   cc_addrs               = excluded.cc_addrs,
                   bcc_addrs              = excluded.bcc_addrs,
                   subject                = excluded.subject,
                   body_md                = excluded.body_md,
                   updated_at             = excluded.updated_at",
                params![
                    draft.account_id.as_str(),
                    draft.id.as_str(),
                    draft.gmail_draft_id,
                    draft.thread_id.as_ref().map(|t| t.as_str().to_string()),
                    draft
                        .in_reply_to_message_id
                        .as_ref()
                        .map(|m| m.as_str().to_string()),
                    serde_json::to_string(&draft.to)?,
                    serde_json::to_string(&draft.cc)?,
                    serde_json::to_string(&draft.bcc)?,
                    draft.subject,
                    draft.body_md,
                    dt_to_ms(&draft.updated_at),
                ],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn delete_draft_local(&self, account: &AccountId, id: &DraftId) -> CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        let id = id.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "DELETE FROM drafts WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), id.as_str()],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn queue_draft_send(
        &self,
        account: &AccountId,
        draft_id: &DraftId,
        op_id: &OpId,
    ) -> CoreResult<i64> {
        let pool = self.pool.clone();
        let account = account.clone();
        let draft_id = draft_id.clone();
        let op_id = op_id.clone();
        spawn_blocking(move || -> CoreResult<i64> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            let changed = tx
                .execute(
                    "UPDATE drafts SET state = 'queued' WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), draft_id.as_str()],
                )
                .map_err(map_err)?;
            if changed == 0 {
                return Err(CoreError::NotFound("draft not found".into()));
            }
            let kind = OutboxOpKind::SendDraft {
                draft_id: draft_id.clone(),
            };
            tx.execute(
                "INSERT INTO outbox
                   (account_id, op_id, op_kind, payload_json, created_at, attempts, state)
                 VALUES (?1, ?2, 'send_draft', ?3, ?4, 0, 'pending')",
                params![
                    account.as_str(),
                    op_id.0,
                    serde_json::to_string(&kind)?,
                    Utc::now().timestamp_millis()
                ],
            )
            .map_err(map_err)?;
            let outbox_id = tx.last_insert_rowid();
            tx.commit().map_err(map_err)?;
            Ok(outbox_id)
        })
        .await
        .map_err(map_err)?
    }

    async fn schedule_send(
        &self,
        account: &AccountId,
        draft_id: &DraftId,
        send_at: DateTime<Utc>,
    ) -> CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        let draft_id = draft_id.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "INSERT INTO send_later (account_id, id, draft_id, send_at, state)
                 VALUES (?1, ?2, ?3, ?4, 'scheduled')",
                params![
                    account.as_str(),
                    uuid::Uuid::new_v4().simple().to_string(),
                    draft_id.as_str(),
                    dt_to_ms(&send_at)
                ],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn complete_send(
        &self,
        outbox_row_id: i64,
        account: &AccountId,
        draft_id: &DraftId,
    ) -> CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        let draft_id = draft_id.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            tx.execute(
                "UPDATE outbox SET state = 'done' WHERE id = ?1 AND account_id = ?2",
                params![outbox_row_id, account.as_str()],
            )
            .map_err(map_err)?;
            tx.execute(
                "UPDATE send_later SET state = 'sent'
                 WHERE account_id = ?1 AND draft_id = ?2",
                params![account.as_str(), draft_id.as_str()],
            )
            .map_err(map_err)?;
            tx.execute(
                "DELETE FROM drafts WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), draft_id.as_str()],
            )
            .map_err(map_err)?;
            tx.commit().map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn enqueue_outbox(
        &self,
        account: &AccountId,
        op_id: &OpId,
        kind: &OutboxOpKind,
    ) -> CoreResult<i64> {
        let pool = self.pool.clone();
        let account = account.clone();
        let op_id = op_id.clone();
        let kind_name = outbox_kind_name(kind);
        let payload_json = serde_json::to_string(kind)?;
        spawn_blocking(move || -> CoreResult<i64> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "INSERT INTO outbox
                   (account_id, op_id, op_kind, payload_json, created_at, attempts, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 'pending')",
                params![
                    account.as_str(),
                    op_id.0,
                    kind_name,
                    payload_json,
                    Utc::now().timestamp_millis()
                ],
            )
            .map_err(map_err)?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .map_err(map_err)?
    }

    async fn drain_pending_outbox(
        &self,
        account: &AccountId,
        max: u32,
    ) -> CoreResult<Vec<OutboxOp>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> CoreResult<Vec<OutboxOp>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id FROM outbox
                     WHERE account_id = ?1 AND state = 'pending' AND next_attempt_at <= ?2
                     ORDER BY id LIMIT ?3",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(
                    params![account.as_str(), Utc::now().timestamp_millis(), max as i64],
                    |r| r.get::<_, i64>("id"),
                )
                .map_err(map_err)?;
            // We can't easily return CoreResult from query_map's closure, so
            // collect rowids first, then re-fetch with our deserializing helper.
            let ids: Vec<i64> = rows
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            drop(stmt);

            let mut out = Vec::with_capacity(ids.len());
            let mut detail = conn
                .prepare_cached(
                    "SELECT id, account_id, op_id, op_kind, payload_json, created_at,
                            attempts, last_error
                     FROM outbox WHERE id = ?1",
                )
                .map_err(map_err)?;
            for id in ids {
                let op = detail
                    .query_row(params![id], row_to_outbox)
                    .map_err(map_err)?;
                out.push(op);
            }
            Ok(out)
        })
        .await
        .map_err(map_err)?
    }

    async fn mark_outbox_done(&self, id: i64) -> CoreResult<()> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "UPDATE outbox SET state = 'done', completed_at = ?1 WHERE id = ?2",
                params![Utc::now().timestamp_millis(), id],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn mark_outbox_failed(&self, id: i64, error: &str) -> CoreResult<()> {
        let pool = self.pool.clone();
        let error = error.to_string();
        spawn_blocking(move || -> CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            let row: Option<(i64, String, String, String)> = tx
                .query_row(
                    "SELECT attempts, account_id, op_kind, payload_json FROM outbox WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(map_err)?;
            let Some((attempts, account, op_kind, payload_json)) = row else {
                return Ok(());
            };
            let attempts = attempts + 1;
            let state = if attempts >= 5 { "failed" } else { "pending" };
            const BACKOFF_MS: [i64; 5] = [60_000, 300_000, 1_800_000, 7_200_000, 43_200_000];
            let next_attempt_at = if attempts < 5 {
                Utc::now().timestamp_millis() + BACKOFF_MS[attempts as usize - 1]
            } else {
                0
            };
            tx.execute(
                "UPDATE outbox
                 SET attempts = ?1, last_error = ?2, state = ?3, next_attempt_at = ?4
                 WHERE id = ?5",
                params![attempts, error, state, next_attempt_at, id],
            )
            .map_err(map_err)?;
            if attempts >= 5 && op_kind == "send_draft" {
                if let Ok(OutboxOpKind::SendDraft { draft_id }) =
                    serde_json::from_str::<OutboxOpKind>(&payload_json)
                {
                    tx.execute(
                        "UPDATE drafts SET state = 'failed'
                         WHERE account_id = ?1 AND id = ?2",
                        params![account, draft_id.as_str()],
                    )
                    .map_err(map_err)?;
                }
            }
            tx.commit().map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn outbox_summary(&self, scope: &AccountScope) -> CoreResult<OutboxSummary> {
        let pool = self.pool.clone();
        let account = scope.account().map(|value| value.as_str().to_string());
        spawn_blocking(move || -> CoreResult<OutboxSummary> {
            let conn = pool.get().map_err(map_err)?;
            conn.query_row(
                "SELECT
                    SUM(CASE WHEN state = 'pending' THEN 1 ELSE 0 END),
                    SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END),
                    (SELECT last_error FROM outbox
                     WHERE (?1 IS NULL OR account_id = ?1)
                       AND state IN ('pending', 'failed') AND last_error IS NOT NULL
                     ORDER BY id DESC LIMIT 1)
                 FROM outbox WHERE ?1 IS NULL OR account_id = ?1",
                params![account],
                |row| {
                    Ok(OutboxSummary {
                        pending: row.get::<_, Option<i64>>(0)?.unwrap_or(0) as u32,
                        failed: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u32,
                        last_error: row.get(2)?,
                    })
                },
            )
            .map_err(map_err)
        })
        .await
        .map_err(map_err)?
    }

    async fn retry_failed_outbox(&self, account: &AccountId) -> CoreResult<u32> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> CoreResult<u32> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "UPDATE outbox
                 SET state = 'pending', attempts = 0, next_attempt_at = 0
                 WHERE account_id = ?1 AND state = 'failed'",
                params![account.as_str()],
            )
            .map(|count| count as u32)
            .map_err(map_err)
        })
        .await
        .map_err(map_err)?
    }

    async fn get_history_cursor(&self, account: &AccountId) -> CoreResult<Option<u64>> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> CoreResult<Option<u64>> {
            let conn = pool.get().map_err(map_err)?;
            let cur: Option<i64> = conn
                .query_row(
                    "SELECT history_id FROM sync_state WHERE account_id = ?1",
                    params![account.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_err)?;
            Ok(cur.map(|v| v as u64))
        })
        .await
        .map_err(map_err)?
    }

    async fn set_history_cursor(&self, account: &AccountId, cursor: u64) -> CoreResult<()> {
        let pool = self.pool.clone();
        let account = account.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "INSERT INTO sync_state (account_id, history_id, last_incremental_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id) DO UPDATE SET
                   history_id          = excluded.history_id,
                   last_incremental_at = excluded.last_incremental_at",
                params![
                    account.as_str(),
                    cursor as i64,
                    Utc::now().timestamp_millis()
                ],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use mach_core::{action::Action, dispatcher::Dispatcher};
    use std::sync::Arc;

    fn account() -> AccountId {
        AccountId::new("test@example.com")
    }

    fn scope() -> AccountScope {
        AccountScope::One(account())
    }

    fn draft(id: &str) -> Draft {
        Draft {
            account_id: account(),
            id: DraftId::new(id),
            gmail_draft_id: None,
            thread_id: None,
            in_reply_to_message_id: None,
            to: vec!["recipient@example.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Subject".into(),
            body_md: "Body".into(),
            updated_at: Utc::now(),
        }
    }

    fn seed(pool: &DbPool, id: &str, subject: &str, body: &str, labels: &[&str]) {
        seed_for(pool, &account(), id, subject, body, labels);
    }

    fn seed_for(
        pool: &DbPool,
        account: &AccountId,
        id: &str,
        subject: &str,
        body: &str,
        labels: &[&str],
    ) {
        let conn = pool.get().unwrap();
        let labels_json = serde_json::to_string(&labels.to_vec()).unwrap();
        let now = Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO threads (account_id, id, history_id, snippet, subject, participants_json,
                                  last_message_at, message_count, unread, starred,
                                  label_ids_json, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4, '[]', ?5, 1, ?6, ?7, ?8, ?5)",
            params![
                account.as_str(),
                id,
                body,
                subject,
                now,
                labels.contains(&"UNREAD") as i64,
                labels.contains(&"STARRED") as i64,
                labels_json,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (account_id, id, thread_id, history_id, internal_date, from_addr,
                                   to_addrs, subject, snippet, body_plain, label_ids_json,
                                   fetched_full)
             VALUES (?1, ?2, ?3, 1, ?4, 'alice@example.com', '[\"me@example.com\"]', ?5, ?6, ?7, ?8, 1)",
            params![
                account.as_str(),
                format!("{id}-m"),
                id,
                now,
                subject,
                body,
                body,
                labels_json
            ],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn archive_persists_through_sqlite_dispatcher() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "t1", "hello", "hi there", &["INBOX", "UNREAD"]);
        let store = Arc::new(SqliteStore::new(pool.clone()));
        let dispatcher = Dispatcher::with_scope(store.clone(), scope());

        let outcome = dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        assert!(outcome.op_id.is_some());

        let t = store
            .get_thread(&scope(), &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(!t.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        // Outbox got the op for the sync engine.
        let pending = store.drain_pending_outbox(&account(), 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0].kind, OutboxOpKind::ModifyLabels { .. }));
    }

    #[tokio::test]
    async fn thread_mutation_rolls_back_when_outbox_insert_fails() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "t1", "hello", "hi there", &["INBOX", "UNREAD"]);
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_outbox BEFORE INSERT ON outbox
                 BEGIN
                   SELECT RAISE(ABORT, 'injected outbox failure');
                 END;",
            )
            .unwrap();

        let store = Arc::new(SqliteStore::new(pool));
        let dispatcher = Dispatcher::with_scope(store.clone(), scope());
        let result = dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await;
        assert!(result.is_err());

        let thread = store
            .get_thread(&scope(), &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert!(thread
            .label_ids
            .iter()
            .any(|label| label.as_str() == "INBOX"));
    }

    #[tokio::test]
    async fn label_upsert_matches_multi_account_primary_key() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool);
        let label = LabelUpsert {
            id: "INBOX".into(),
            name: "Inbox".into(),
            system: true,
            color: None,
        };

        store
            .upsert_labels(&account(), vec![label.clone()])
            .await
            .unwrap();
        store
            .upsert_labels(
                &account(),
                vec![LabelUpsert {
                    name: "Primary inbox".into(),
                    ..label
                }],
            )
            .await
            .unwrap();

        let labels = store.list_labels(&scope()).await.unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].name, "Primary inbox");
    }

    #[tokio::test]
    async fn history_deletions_remove_messages_and_threads() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "t1", "hello", "hi there", &["INBOX"]);
        let store = SqliteStore::new(pool);

        store
            .delete_messages(&account(), vec!["t1-m".into()])
            .await
            .unwrap();
        assert!(store
            .list_messages_in_thread(&scope(), &ThreadId::new("t1"))
            .await
            .unwrap()
            .is_empty());

        store.delete_thread(&account(), "t1".into()).await.unwrap();
        assert!(store
            .get_thread(&scope(), &ThreadId::new("t1"))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn fts_search_finds_seeded_thread() {
        let pool = open_in_memory().unwrap();
        seed(
            &pool,
            "t1",
            "Project status update",
            "Quarterly numbers attached.",
            &["INBOX"],
        );
        seed(&pool, "t2", "Lunch?", "Sushi or salad.", &["INBOX"]);

        let store = SqliteStore::new(pool);
        let hits = store
            .search_threads(&scope(), "quarterly", 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id.as_str(), "t1");
    }

    #[tokio::test]
    async fn search_operators_filter_threads() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "unread", "Update", "Status", &["INBOX", "UNREAD"]);
        seed(&pool, "starred", "Invoice", "Paid", &["INBOX", "STARRED"]);
        let store = SqliteStore::new(pool);

        let unread = store
            .search_threads(&scope(), "is:unread", 10)
            .await
            .unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].id.as_str(), "unread");

        let from = store
            .search_threads(&scope(), "from:alice", 10)
            .await
            .unwrap();
        assert_eq!(from.len(), 2);

        let starred = store
            .search_threads(&scope(), "is:starred", 10)
            .await
            .unwrap();
        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].id.as_str(), "starred");
    }

    #[tokio::test]
    async fn outbox_round_trips() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool.clone());
        let op_id = OpId::new();
        let id = store
            .enqueue_outbox(
                &account(),
                &op_id,
                &OutboxOpKind::ModifyLabels {
                    thread_ids: vec![ThreadId::new("t1")],
                    add: vec![],
                    remove: vec![LabelId::new("INBOX")],
                },
            )
            .await
            .unwrap();
        assert!(id > 0);

        let pending = store.drain_pending_outbox(&account(), 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].op_id, op_id);

        store.mark_outbox_done(id).await.unwrap();
        let pending = store.drain_pending_outbox(&account(), 10).await.unwrap();
        assert!(pending.is_empty());

        let completed_at: i64 = pool
            .get()
            .unwrap()
            .query_row(
                "SELECT completed_at FROM outbox WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            store
                .recent_done_outbox(&account(), completed_at)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .recent_done_outbox(&account(), completed_at + 1)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn outbox_failures_back_off_surface_and_can_be_retried() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool.clone());
        let id = store
            .enqueue_outbox(
                &account(),
                &OpId::new(),
                &OutboxOpKind::Trash {
                    thread_ids: vec![ThreadId::new("t1")],
                },
            )
            .await
            .unwrap();

        store.mark_outbox_failed(id, "temporary").await.unwrap();
        assert!(store
            .drain_pending_outbox(&account(), 10)
            .await
            .unwrap()
            .is_empty());
        let (attempts, state, next_attempt_at): (i64, String, i64) = pool
            .get()
            .unwrap()
            .query_row(
                "SELECT attempts, state, next_attempt_at FROM outbox WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((attempts, state.as_str()), (1, "pending"));
        assert!(next_attempt_at > Utc::now().timestamp_millis());

        let summary = store.outbox_summary(&AccountScope::All).await.unwrap();
        assert_eq!((summary.pending, summary.failed), (1, 0));
        assert_eq!(summary.last_error.as_deref(), Some("temporary"));

        for _ in 1..5 {
            store.mark_outbox_failed(id, "permanent").await.unwrap();
        }
        let summary = store.outbox_summary(&scope()).await.unwrap();
        assert_eq!((summary.pending, summary.failed), (0, 1));
        assert_eq!(store.retry_failed_outbox(&account()).await.unwrap(), 1);
        let summary = store.outbox_summary(&scope()).await.unwrap();
        assert_eq!((summary.pending, summary.failed), (1, 0));
        assert_eq!(
            store
                .drain_pending_outbox(&account(), 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn queue_draft_send_rolls_back_state_when_outbox_insert_fails() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool.clone());
        let draft = draft("atomic-queue");
        store.save_draft_local(&draft).await.unwrap();
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_send_outbox BEFORE INSERT ON outbox
                 BEGIN
                   SELECT RAISE(ABORT, 'injected send outbox failure');
                 END;",
            )
            .unwrap();

        assert!(store
            .queue_draft_send(&account(), &draft.id, &OpId::new())
            .await
            .is_err());
        let conn = pool.get().unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM drafts WHERE account_id = ?1 AND id = ?2",
                params![account().as_str(), draft.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let outbox_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        assert_eq!(state, "draft");
        assert_eq!(outbox_count, 0);
    }

    #[tokio::test]
    async fn complete_send_atomically_finishes_outbox_draft_and_schedule() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool.clone());
        let draft = draft("complete-send");
        store.save_draft_local(&draft).await.unwrap();
        store
            .schedule_send(&account(), &draft.id, Utc::now())
            .await
            .unwrap();
        let outbox_id = store
            .queue_draft_send(&account(), &draft.id, &OpId::new())
            .await
            .unwrap();

        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_draft_delete BEFORE DELETE ON drafts
                 BEGIN
                   SELECT RAISE(ABORT, 'injected draft delete failure');
                 END;",
            )
            .unwrap();
        assert!(store
            .complete_send(outbox_id, &account(), &draft.id)
            .await
            .is_err());
        {
            let conn = pool.get().unwrap();
            let outbox_state: String = conn
                .query_row(
                    "SELECT state FROM outbox WHERE id = ?1",
                    params![outbox_id],
                    |row| row.get(0),
                )
                .unwrap();
            let draft_state: String = conn
                .query_row(
                    "SELECT state FROM drafts WHERE account_id = ?1 AND id = ?2",
                    params![account().as_str(), draft.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            let send_later_state: String = conn
                .query_row(
                    "SELECT state FROM send_later WHERE account_id = ?1 AND draft_id = ?2",
                    params![account().as_str(), draft.id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(outbox_state, "pending");
            assert_eq!(draft_state, "queued");
            assert_eq!(send_later_state, "scheduled");
        }
        pool.get()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_draft_delete")
            .unwrap();

        store
            .complete_send(outbox_id, &account(), &draft.id)
            .await
            .unwrap();

        assert!(store
            .get_draft(&account(), &draft.id)
            .await
            .unwrap()
            .is_none());
        let conn = pool.get().unwrap();
        let outbox_state: String = conn
            .query_row(
                "SELECT state FROM outbox WHERE id = ?1",
                params![outbox_id],
                |row| row.get(0),
            )
            .unwrap();
        let send_later_state: String = conn
            .query_row(
                "SELECT state FROM send_later WHERE account_id = ?1 AND draft_id = ?2",
                params![account().as_str(), draft.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outbox_state, "done");
        assert_eq!(send_later_state, "sent");
    }

    #[tokio::test]
    async fn fifth_send_failure_dead_letters_outbox_and_draft() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool.clone());
        let draft = draft("dead-letter");
        store.save_draft_local(&draft).await.unwrap();
        let outbox_id = store
            .queue_draft_send(&account(), &draft.id, &OpId::new())
            .await
            .unwrap();

        for attempt in 1..=5 {
            store
                .mark_outbox_failed(outbox_id, &format!("failure {attempt}"))
                .await
                .unwrap();
        }

        assert!(store
            .drain_pending_outbox(&account(), 10)
            .await
            .unwrap()
            .is_empty());
        let conn = pool.get().unwrap();
        let (attempts, outbox_state): (i64, String) = conn
            .query_row(
                "SELECT attempts, state FROM outbox WHERE id = ?1",
                params![outbox_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let draft_state: String = conn
            .query_row(
                "SELECT state FROM drafts WHERE account_id = ?1 AND id = ?2",
                params![account().as_str(), draft.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 5);
        assert_eq!(outbox_state, "failed");
        assert_eq!(draft_state, "failed");
        drop(conn);
        assert_eq!(store.dead_letters(&account()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_message_parses_persisted_threading_headers() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "headers", "subject", "body", &["INBOX"]);
        let headers = MessageHeaders {
            message_id: Some("<message@example.com>".into()),
            in_reply_to: Some("<parent@example.com>".into()),
            references: Some("<root@example.com>".into()),
            reply_to: Some("reply@example.com".into()),
        };
        pool.get()
            .unwrap()
            .execute(
                "UPDATE messages SET headers_json = ?1 WHERE account_id = ?2 AND id = ?3",
                params![
                    serde_json::to_string(&headers).unwrap(),
                    account().as_str(),
                    "headers-m"
                ],
            )
            .unwrap();
        let store = SqliteStore::new(pool);

        let message = store
            .get_message(&scope(), &MessageId::new("headers-m"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.headers, Some(headers));
    }

    #[tokio::test]
    async fn history_cursor_persists() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool);
        assert!(store
            .get_history_cursor(&account())
            .await
            .unwrap()
            .is_none());
        store.set_history_cursor(&account(), 12345).await.unwrap();
        assert_eq!(
            store.get_history_cursor(&account()).await.unwrap(),
            Some(12345)
        );
        store.set_history_cursor(&account(), 99999).await.unwrap();
        assert_eq!(
            store.get_history_cursor(&account()).await.unwrap(),
            Some(99999)
        );
    }

    #[tokio::test]
    async fn list_threads_in_label_filters_correctly() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "t1", "a", "a", &["INBOX"]);
        seed(&pool, "t2", "b", "b", &["INBOX", "STARRED"]);
        seed(&pool, "t3", "c", "c", &["TRASH"]);

        let store = SqliteStore::new(pool);
        let inbox = store
            .list_threads_in_label(&scope(), &LabelId::new("INBOX"), 10)
            .await
            .unwrap();
        assert_eq!(inbox.len(), 2);

        let starred = store
            .list_threads_in_label(&scope(), &LabelId::new("STARRED"), 10)
            .await
            .unwrap();
        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].id.as_str(), "t2");
    }

    #[tokio::test]
    async fn list_threads_in_done_returns_only_archived_threads() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "inbox", "inbox", "inbox", &["INBOX"]);
        seed(&pool, "archived", "archived", "archived", &[]);
        seed(&pool, "trash", "trash", "trash", &["TRASH"]);

        let store = SqliteStore::new(pool);
        let done = store
            .list_threads_in_label(&scope(), &LabelId::new("DONE"), 10)
            .await
            .unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].id.as_str(), "archived");

        let inbox = store
            .list_threads_in_label(&scope(), &LabelId::new("INBOX"), 10)
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].id.as_str(), "inbox");
    }

    #[tokio::test]
    async fn unified_reads_merge_accounts_and_qualified_reads_disambiguate_ids() {
        let pool = open_in_memory().unwrap();
        let first = AccountId::new("first@example.com");
        let second = AccountId::new("second@example.com");
        seed_for(
            &pool,
            &first,
            "shared-id",
            "First mailbox",
            "one",
            &["INBOX"],
        );
        seed_for(
            &pool,
            &second,
            "shared-id",
            "Second mailbox",
            "two",
            &["INBOX"],
        );
        let store = SqliteStore::new(pool);

        let unified = store
            .list_threads_in_label(&AccountScope::All, &LabelId::new("INBOX"), 10)
            .await
            .unwrap();
        assert_eq!(unified.len(), 2);
        assert!(store
            .get_thread(&AccountScope::All, &ThreadId::new("shared-id"))
            .await
            .is_err());

        let selected = store
            .get_thread(
                &AccountScope::One(second.clone()),
                &ThreadId::new("shared-id"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.account_id, second);
        assert_eq!(selected.subject, "Second mailbox");
    }

    #[tokio::test]
    async fn scoped_mutation_and_outbox_never_cross_accounts() {
        let pool = open_in_memory().unwrap();
        let first = AccountId::new("first@example.com");
        let second = AccountId::new("second@example.com");
        seed_for(&pool, &first, "shared-id", "First", "one", &["INBOX"]);
        seed_for(&pool, &second, "shared-id", "Second", "two", &["INBOX"]);
        let store = Arc::new(SqliteStore::new(pool));
        let dispatcher = Dispatcher::with_scope(store.clone(), AccountScope::One(first.clone()));

        dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("shared-id")],
            })
            .await
            .unwrap();

        let archived = store
            .get_thread(
                &AccountScope::One(first.clone()),
                &ThreadId::new("shared-id"),
            )
            .await
            .unwrap()
            .unwrap();
        let untouched = store
            .get_thread(
                &AccountScope::One(second.clone()),
                &ThreadId::new("shared-id"),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!archived.label_ids.contains(&LabelId::new("INBOX")));
        assert!(untouched.label_ids.contains(&LabelId::new("INBOX")));
        assert_eq!(
            store.drain_pending_outbox(&first, 10).await.unwrap().len(),
            1
        );
        assert!(store
            .drain_pending_outbox(&second, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn history_cursors_are_independent_per_account() {
        let store = SqliteStore::new(open_in_memory().unwrap());
        let first = AccountId::new("first@example.com");
        let second = AccountId::new("second@example.com");
        store.set_history_cursor(&first, 10).await.unwrap();
        store.set_history_cursor(&second, 20).await.unwrap();
        assert_eq!(store.get_history_cursor(&first).await.unwrap(), Some(10));
        assert_eq!(store.get_history_cursor(&second).await.unwrap(), Some(20));
    }

    #[tokio::test]
    async fn legacy_rows_are_claimed_by_the_first_account() {
        let pool = open_in_memory().unwrap();
        let legacy = AccountId::new("__legacy__");
        seed_for(&pool, &legacy, "t1", "Legacy", "body", &["INBOX"]);
        let store = SqliteStore::new(pool);
        let account = AccountId::new("owner@example.com");

        store.claim_legacy_account(&account).await.unwrap();

        let claimed = store
            .get_thread(&AccountScope::One(account.clone()), &ThreadId::new("t1"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.account_id, account);
        assert_eq!(
            store
                .list_messages_in_thread(
                    &AccountScope::One(claimed.account_id.clone()),
                    &ThreadId::new("t1"),
                )
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
