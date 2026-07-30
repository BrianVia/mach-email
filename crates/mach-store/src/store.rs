use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use mach_core::{
    action::OpId,
    error::{CoreError, CoreResult},
    ids::{DraftId, LabelId, MessageId, ThreadId},
    store::{Draft, Label, MailStore, Message, OutboxOp, OutboxOpKind, ThreadSummary},
};
use rusqlite::{params, OptionalExtension, Row};
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
    /// Replace the labels table contents wholesale. Cheaper than diffing on
    /// bootstrap; for incremental updates the sync engine should patch.
    pub async fn upsert_labels(&self, labels: Vec<LabelUpsert>) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            for l in labels {
                tx.execute(
                    "INSERT INTO labels (id, name, type, color) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(id) DO UPDATE SET
                       name  = excluded.name,
                       type  = excluded.type,
                       color = excluded.color",
                    params![
                        l.id,
                        l.name,
                        if l.system { "system" } else { "user" },
                        l.color
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
    pub async fn upsert_threads(&self, threads: Vec<ThreadUpsert>) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
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
                    "INSERT INTO threads (id, history_id, snippet, subject, participants_json,
                                          last_message_at, message_count, unread, starred,
                                          label_ids_json, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT(id) DO UPDATE SET
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
                        "INSERT INTO messages (id, thread_id, history_id, internal_date,
                                               from_addr, to_addrs, cc_addrs, subject,
                                               snippet, body_plain, label_ids_json, fetched_full)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                         ON CONFLICT(id) DO UPDATE SET
                           history_id     = excluded.history_id,
                           internal_date  = excluded.internal_date,
                           from_addr      = excluded.from_addr,
                           to_addrs       = excluded.to_addrs,
                           cc_addrs       = excluded.cc_addrs,
                           subject        = excluded.subject,
                           snippet        = excluded.snippet,
                           body_plain     = COALESCE(excluded.body_plain, messages.body_plain),
                           label_ids_json = excluded.label_ids_json,
                           fetched_full   = MAX(excluded.fetched_full, messages.fetched_full)",
                        params![
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
}

/// One thread whose `MACH/Snoozed/<rfc3339>` label is past its time. The
/// scheduler reads these in `find_due_snoozes` and un-snoozes them.
#[derive(Debug, Clone)]
pub struct DueSnooze {
    pub thread_id: String,
    pub snoozed_label: String,
}

/// One scheduled send. `send_later_due` returns these in `find_due_sends`.
#[derive(Debug, Clone)]
pub struct DueSend {
    pub send_later_id: String,
    pub draft_id: String,
}

impl SqliteStore {
    /// Return all snoozed threads whose `MACH/Snoozed/<rfc3339>` label is
    /// past `now`. Walks every thread row, parses the JSON label array,
    /// extracts the matching label. O(threads) — fine at personal scale.
    pub async fn find_due_snoozes(&self, now_ms: i64) -> mach_core::CoreResult<Vec<DueSnooze>> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> mach_core::CoreResult<Vec<DueSnooze>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached("SELECT id, label_ids_json FROM threads")
                .map_err(map_err)?;
            let mut due = Vec::new();
            let rows = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(map_err)?;
            for row in rows {
                let (id, labels_json) = row.map_err(map_err)?;
                let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                for l in labels {
                    let Some(rest) = l.strip_prefix("MACH/Snoozed/") else { continue };
                    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(rest) else { continue };
                    if ts.timestamp_millis() <= now_ms {
                        due.push(DueSnooze {
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
    pub async fn find_due_sends(&self, now_ms: i64) -> mach_core::CoreResult<Vec<DueSend>> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> mach_core::CoreResult<Vec<DueSend>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, draft_id FROM send_later
                     WHERE state = 'scheduled' AND send_at <= ?1",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![now_ms], |r| {
                    Ok(DueSend {
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
    pub async fn mark_send_later(&self, id: &str, state: &str) -> mach_core::CoreResult<()> {
        let pool = self.pool.clone();
        let id = id.to_string();
        let state = state.to_string();
        spawn_blocking(move || -> mach_core::CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "UPDATE send_later SET state = ?1 WHERE id = ?2",
                params![state, id],
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
        thread_id: &mach_core::ids::ThreadId,
    ) -> mach_core::CoreResult<usize> {
        let pool = self.pool.clone();
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
                     WHERE thread_id = ?1",
                    params![tid],
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
        updates: Vec<MessageBodyUpdate>,
    ) -> mach_core::CoreResult<usize> {
        let pool = self.pool.clone();
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
                         WHERE id = ?4",
                        params![u.body_plain, u.body_html, u.inline_images_json, u.id],
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
    Utc.timestamp_millis_opt(ms).single().unwrap_or_else(Utc::now)
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
    Ok(Message {
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
        label_ids: serde_json::from_str(&label_ids_json).unwrap_or_default(),
        fetched_full: row.get::<_, i64>("fetched_full").unwrap_or(0) != 0,
        inline_images,
    })
}

fn row_to_label(row: &Row) -> rusqlite::Result<Label> {
    let label_type: String = row.get("type")?;
    Ok(Label {
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

fn row_to_outbox(row: &Row) -> CoreResult<OutboxOp> {
    let payload_json: String = row.get("payload_json").map_err(map_err)?;
    let kind: OutboxOpKind = serde_json::from_str(&payload_json)?;
    Ok(OutboxOp {
        id: row.get("id").map_err(map_err)?,
        op_id: OpId(row.get::<_, String>("op_id").map_err(map_err)?),
        kind,
        created_at: ms_to_dt(row.get("created_at").map_err(map_err)?),
        attempts: row.get::<_, i64>("attempts").map_err(map_err)? as u32,
        last_error: row.get("last_error").map_err(map_err)?,
    })
}

#[async_trait]
impl MailStore for SqliteStore {
    async fn get_thread(&self, id: &ThreadId) -> CoreResult<Option<ThreadSummary>> {
        let pool = self.pool.clone();
        let id = id.clone();
        spawn_blocking(move || -> CoreResult<Option<ThreadSummary>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, subject, snippet, participants_json, last_message_at,
                            message_count, unread, starred, label_ids_json
                     FROM threads WHERE id = ?1",
                )
                .map_err(map_err)?;
            stmt.query_row(params![id.as_str()], row_to_thread)
                .optional()
                .map_err(map_err)
        })
        .await
        .map_err(map_err)?
    }

    async fn list_threads_in_label(
        &self,
        label: &LabelId,
        limit: u32,
    ) -> CoreResult<Vec<ThreadSummary>> {
        let pool = self.pool.clone();
        let label = label.clone();
        spawn_blocking(move || -> CoreResult<Vec<ThreadSummary>> {
            let conn = pool.get().map_err(map_err)?;
            // Filter via JSON containment. With label-id strings always quoted
            // in the JSON array, a LIKE check is safe — no false positives.
            let needle = format!("\"{}\"", label.as_str());
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, subject, snippet, participants_json, last_message_at,
                            message_count, unread, starred, label_ids_json
                     FROM threads
                     WHERE label_ids_json LIKE '%' || ?1 || '%'
                     ORDER BY last_message_at DESC
                     LIMIT ?2",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![needle, limit as i64], row_to_thread)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn list_messages_in_thread(&self, id: &ThreadId) -> CoreResult<Vec<Message>> {
        let pool = self.pool.clone();
        let id = id.clone();
        spawn_blocking(move || -> CoreResult<Vec<Message>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, thread_id, internal_date, from_addr, to_addrs, cc_addrs,
                            subject, snippet, body_plain, body_html, label_ids_json,
                            fetched_full, inline_images_json
                     FROM messages WHERE thread_id = ?1 ORDER BY internal_date",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![id.as_str()], row_to_message)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn search_threads(&self, query: &str, limit: u32) -> CoreResult<Vec<ThreadSummary>> {
        let pool = self.pool.clone();
        // FTS5 treats `-`, `:`, `"`, `*` and `()` as operators. For now we
        // treat the user's input as literal text by wrapping in a phrase
        // (doubled-quote escaping). When we add a real advanced-search
        // surface, we'll bypass this and pass FTS5 syntax through unchanged.
        let query = format!("\"{}\"", query.replace('"', "\"\""));
        spawn_blocking(move || -> CoreResult<Vec<ThreadSummary>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT t.id, t.subject, t.snippet, t.participants_json, t.last_message_at,
                            t.message_count, t.unread, t.starred, t.label_ids_json
                     FROM messages_fts f
                     JOIN messages m ON m.rowid = f.rowid
                     JOIN threads t  ON t.id    = m.thread_id
                     WHERE messages_fts MATCH ?1
                     GROUP BY t.id
                     ORDER BY MAX(t.last_message_at) DESC
                     LIMIT ?2",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![&query, limit as i64], row_to_thread)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn modify_labels_local(
        &self,
        thread_ids: &[ThreadId],
        add: &[LabelId],
        remove: &[LabelId],
    ) -> CoreResult<()> {
        let pool = self.pool.clone();
        let thread_ids: Vec<ThreadId> = thread_ids.to_vec();
        let add: Vec<LabelId> = add.to_vec();
        let remove: Vec<LabelId> = remove.to_vec();
        spawn_blocking(move || -> CoreResult<()> {
            let mut conn = pool.get().map_err(map_err)?;
            let tx = conn.transaction().map_err(map_err)?;
            for tid in &thread_ids {
                let current_json: Option<String> = tx
                    .query_row(
                        "SELECT label_ids_json FROM threads WHERE id = ?1",
                        params![tid.as_str()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(map_err)?;
                let mut labels: Vec<LabelId> = match current_json {
                    Some(s) => serde_json::from_str(&s).unwrap_or_default(),
                    None => continue,
                };
                for r in &remove {
                    labels.retain(|l| l != r);
                }
                for a in &add {
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
                     WHERE id = ?5",
                    params![
                        labels_json,
                        unread as i64,
                        starred as i64,
                        Utc::now().timestamp_millis(),
                        tid.as_str()
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

    async fn list_labels(&self) -> CoreResult<Vec<Label>> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> CoreResult<Vec<Label>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, name, type, color, unread_count, total_count
                     FROM labels ORDER BY name",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map([], row_to_label)
                .map_err(map_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(map_err)?;
            Ok(rows)
        })
        .await
        .map_err(map_err)?
    }

    async fn get_draft(&self, id: &DraftId) -> CoreResult<Option<Draft>> {
        let pool = self.pool.clone();
        let id = id.clone();
        spawn_blocking(move || -> CoreResult<Option<Draft>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, gmail_draft_id, thread_id, in_reply_to_message_id,
                            to_addrs, cc_addrs, bcc_addrs, subject, body_md, updated_at
                     FROM drafts WHERE id = ?1",
                )
                .map_err(map_err)?;
            stmt.query_row(params![id.as_str()], row_to_draft)
                .optional()
                .map_err(map_err)
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
                "INSERT INTO drafts (id, gmail_draft_id, thread_id, in_reply_to_message_id,
                                     to_addrs, cc_addrs, bcc_addrs, subject, body_md,
                                     updated_at, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'draft')
                 ON CONFLICT(id) DO UPDATE SET
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

    async fn delete_draft_local(&self, id: &DraftId) -> CoreResult<()> {
        let pool = self.pool.clone();
        let id = id.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute("DELETE FROM drafts WHERE id = ?1", params![id.as_str()])
                .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn enqueue_outbox(&self, op_id: &OpId, kind: &OutboxOpKind) -> CoreResult<i64> {
        let pool = self.pool.clone();
        let op_id = op_id.clone();
        let kind_name = outbox_kind_name(kind);
        let payload_json = serde_json::to_string(kind)?;
        spawn_blocking(move || -> CoreResult<i64> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "INSERT INTO outbox (op_id, op_kind, payload_json, created_at, attempts, state)
                 VALUES (?1, ?2, ?3, ?4, 0, 'pending')",
                params![
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

    async fn drain_pending_outbox(&self, max: u32) -> CoreResult<Vec<OutboxOp>> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> CoreResult<Vec<OutboxOp>> {
            let conn = pool.get().map_err(map_err)?;
            let mut stmt = conn
                .prepare_cached(
                    "SELECT id, op_id, op_kind, payload_json, created_at, attempts, last_error
                     FROM outbox WHERE state = 'pending' ORDER BY id LIMIT ?1",
                )
                .map_err(map_err)?;
            let rows = stmt
                .query_map(params![max as i64], |r| Ok(r.get::<_, i64>("id")?))
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
                    "SELECT id, op_id, op_kind, payload_json, created_at, attempts, last_error
                     FROM outbox WHERE id = ?1",
                )
                .map_err(map_err)?;
            for id in ids {
                let op = detail
                    .query_row(params![id], |row| {
                        // Hop back through CoreResult by wrapping the JSON
                        // parse failure as a synthesized rusqlite error.
                        row_to_outbox(row).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    e.to_string(),
                                )),
                            )
                        })
                    })
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
                "UPDATE outbox SET state = 'done' WHERE id = ?1",
                params![id],
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
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "UPDATE outbox SET attempts = attempts + 1, last_error = ?1 WHERE id = ?2",
                params![error, id],
            )
            .map_err(map_err)?;
            Ok(())
        })
        .await
        .map_err(map_err)?
    }

    async fn get_history_cursor(&self) -> CoreResult<Option<u64>> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> CoreResult<Option<u64>> {
            let conn = pool.get().map_err(map_err)?;
            let cur: Option<i64> = conn
                .query_row(
                    "SELECT history_id FROM sync_state WHERE account_id = 'default'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_err)?;
            Ok(cur.map(|v| v as u64))
        })
        .await
        .map_err(map_err)?
    }

    async fn set_history_cursor(&self, cursor: u64) -> CoreResult<()> {
        let pool = self.pool.clone();
        spawn_blocking(move || -> CoreResult<()> {
            let conn = pool.get().map_err(map_err)?;
            conn.execute(
                "INSERT INTO sync_state (account_id, history_id, last_incremental_at)
                 VALUES ('default', ?1, ?2)
                 ON CONFLICT(account_id) DO UPDATE SET
                   history_id          = excluded.history_id,
                   last_incremental_at = excluded.last_incremental_at",
                params![cursor as i64, Utc::now().timestamp_millis()],
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
    use mach_core::{action::Action, dispatcher::Dispatcher, store::ThreadSummary};
    use std::sync::Arc;

    fn seed(pool: &DbPool, id: &str, subject: &str, body: &str, labels: &[&str]) {
        let conn = pool.get().unwrap();
        let labels_json = serde_json::to_string(&labels.to_vec()).unwrap();
        let now = Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO threads (id, history_id, snippet, subject, participants_json,
                                  last_message_at, message_count, unread, starred,
                                  label_ids_json, updated_at)
             VALUES (?1, 1, ?2, ?3, '[]', ?4, 1, ?5, ?6, ?7, ?4)",
            params![
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
            "INSERT INTO messages (id, thread_id, history_id, internal_date, from_addr,
                                   to_addrs, subject, snippet, body_plain, label_ids_json,
                                   fetched_full)
             VALUES (?1, ?2, 1, ?3, 'alice@example.com', '[\"me@example.com\"]', ?4, ?5, ?6, ?7, 1)",
            params![format!("{id}-m"), id, now, subject, body, body, labels_json],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn archive_persists_through_sqlite_dispatcher() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "t1", "hello", "hi there", &["INBOX", "UNREAD"]);
        let store = Arc::new(SqliteStore::new(pool.clone()));
        let dispatcher = Dispatcher::new(store.clone());

        let outcome = dispatcher
            .execute(Action::Archive {
                thread_ids: vec![ThreadId::new("t1")],
            })
            .await
            .unwrap();
        assert!(outcome.op_id.is_some());

        let t = store.get_thread(&ThreadId::new("t1")).await.unwrap().unwrap();
        assert!(!t.label_ids.iter().any(|l| l.as_str() == "INBOX"));

        // Outbox got the op for the sync engine.
        let pending = store.drain_pending_outbox(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0].kind, OutboxOpKind::ModifyLabels { .. }));
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
        let hits = store.search_threads("quarterly", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id.as_str(), "t1");
    }

    #[tokio::test]
    async fn outbox_round_trips() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool);
        let op_id = OpId::new();
        let id = store
            .enqueue_outbox(
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

        let pending = store.drain_pending_outbox(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].op_id, op_id);

        store.mark_outbox_done(id).await.unwrap();
        let pending = store.drain_pending_outbox(10).await.unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn history_cursor_persists() {
        let pool = open_in_memory().unwrap();
        let store = SqliteStore::new(pool);
        assert!(store.get_history_cursor().await.unwrap().is_none());
        store.set_history_cursor(12345).await.unwrap();
        assert_eq!(store.get_history_cursor().await.unwrap(), Some(12345));
        store.set_history_cursor(99999).await.unwrap();
        assert_eq!(store.get_history_cursor().await.unwrap(), Some(99999));
    }

    #[tokio::test]
    async fn list_threads_in_label_filters_correctly() {
        let pool = open_in_memory().unwrap();
        seed(&pool, "t1", "a", "a", &["INBOX"]);
        seed(&pool, "t2", "b", "b", &["INBOX", "STARRED"]);
        seed(&pool, "t3", "c", "c", &["TRASH"]);

        let store = SqliteStore::new(pool);
        let inbox = store
            .list_threads_in_label(&LabelId::new("INBOX"), 10)
            .await
            .unwrap();
        assert_eq!(inbox.len(), 2);

        let starred = store
            .list_threads_in_label(&LabelId::new("STARRED"), 10)
            .await
            .unwrap();
        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].id.as_str(), "t2");
    }

    // Silence unused-import warnings on ThreadSummary in this module.
    #[allow(dead_code)]
    fn _use_thread_summary(_t: ThreadSummary) {}
}
