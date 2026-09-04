//! Gmail cache synchronization: bootstrap, incremental history replay, and
//! conservative gap recovery.
//!
//! Bootstrap: snapshot `historyId` *before* the listing pass, fetch labels
//! and the last 30 days of threads, persist to the SQLite cache, store the
//! cursor. Capturing `historyId` after the listing creates a gap that loses
//! messages forever — the bug almost every TUI mail client gets wrong on the
//! first try. The order in [`bootstrap`] is deliberate.
//!
use std::{collections::HashSet, sync::Arc};

use anyhow::{Context, Result};
use chrono::{Duration, TimeZone, Utc};
use futures::stream::{self, StreamExt};
use mach_core::ids::{AccountId, AccountScope, LabelId, ThreadId};
use mach_core::store::{Label, MailStore, MessageHeaders};
use mach_core::{Action, Dispatcher};
use mach_store::{LabelUpsert, MessageUpsert, SqliteStore, ThreadUpsert};
use tracing::{info, warn};

use crate::client::{GmailClient, HistoryRecord, RemoteMessage, RemoteThread};
use crate::outbox::{DrainStats, OutboxWorker};

const BOOTSTRAP_QUERY: &str = "newer_than:30d";
const BOOTSTRAP_CONCURRENCY: usize = 10;

#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub unsnoozed: usize,
    pub sends_fired: usize,
    pub outbox: DrainStats,
    pub incremental: Option<IncrementalStats>,
}

/// Run one complete non-bootstrap sync pass for an authenticated account.
///
/// This is the sole owner of ordering scheduled local work, pushing queued
/// mutations, and then pulling Gmail history. Adapters decide when to call it
/// and how to present the returned report.
pub async fn sync_account_tick(
    account: &AccountId,
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
) -> Result<TickReport> {
    let now_ms = Utc::now().timestamp_millis();
    let dispatcher = Dispatcher::with_scope(store.clone(), AccountScope::One(account.clone()));

    let due = store.find_due_snoozes(account, now_ms).await?;
    for snooze in &due {
        let thread_id = ThreadId::new(snooze.thread_id.clone());
        dispatcher
            .execute(Action::AddLabel {
                thread_ids: vec![thread_id.clone()],
                label_id: LabelId::new("INBOX"),
            })
            .await
            .context("queueing INBOX restore for due snooze")?;
        dispatcher
            .execute(Action::RemoveLabel {
                thread_ids: vec![thread_id],
                label_id: LabelId::new(snooze.snoozed_label.clone()),
            })
            .await
            .context("queueing snooze-label removal")?;
    }

    let due_sends = store.find_due_sends(account, now_ms).await?;
    let mut sends_fired = 0;
    for send in &due_sends {
        match dispatcher
            .execute(Action::SendDraft {
                draft_id: mach_core::ids::DraftId::new(send.draft_id.clone()),
            })
            .await
        {
            Ok(_) => {
                store
                    .mark_send_later(account, &send.send_later_id, "sent")
                    .await?;
                sends_fired += 1;
            }
            Err(error) => {
                tracing::warn!(
                    send_later_id = %send.send_later_id,
                    %error,
                    "send_later dispatch failed; will retry next sync"
                );
            }
        }
    }

    let outbox = OutboxWorker::new(account.clone(), client.clone(), store.clone());
    let outbox = outbox.drain_once(200).await?;
    let incremental = incremental_sync(client, store).await?;

    Ok(TickReport {
        unsnoozed: due.len(),
        sends_fired,
        outbox,
        incremental: Some(incremental),
    })
}

#[derive(Debug, Default, Clone)]
pub struct BootstrapStats {
    pub history_id: u64,
    pub email: String,
    pub labels: u32,
    pub threads: u32,
    pub messages: u32,
    pub failed_thread_fetches: u32,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct LoadOlderStats {
    pub fetched: u32,
    pub oldest_ms: Option<i64>,
}

pub(crate) struct HydratedThreads {
    pub(crate) threads: Vec<RemoteThread>,
    pub(crate) failed: u32,
}

pub(crate) async fn fetch_and_upsert_threads(
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
    account: &AccountId,
    ids: Vec<String>,
) -> Result<HydratedThreads> {
    let mut threads = Vec::with_capacity(ids.len());
    let mut failed = 0;
    let mut fetches = stream::iter(ids)
        .map(|id| {
            let client = client.clone();
            async move { client.get_thread_metadata(&id).await }
        })
        .buffer_unordered(BOOTSTRAP_CONCURRENCY);

    while let Some(result) = fetches.next().await {
        match result {
            Ok(thread) => threads.push(thread),
            Err(error) => {
                failed += 1;
                warn!(%error, "thread metadata fetch failed");
            }
        }
    }
    if !threads.is_empty() {
        store
            .upsert_threads(
                account,
                threads.iter().map(remote_thread_to_upsert).collect(),
            )
            .await?;
    }
    Ok(HydratedThreads { threads, failed })
}

fn label_to_gmail_query(
    label: &LabelId,
    name_lookup: impl Fn(&LabelId) -> Option<String>,
) -> String {
    match label.as_str() {
        "INBOX" => "in:inbox".into(),
        "STARRED" => "is:starred".into(),
        "SENT" => "in:sent".into(),
        "DRAFT" => "in:draft".into(),
        "TRASH" => "in:trash".into(),
        "SPAM" => "in:spam".into(),
        "DONE" => "-in:inbox -in:trash -in:spam".into(),
        "ALL" => String::new(),
        _ => name_lookup(label)
            .map(|name| format!("label:{name}"))
            .unwrap_or_else(|| format!("label:{}", label.as_str())),
    }
}

fn load_older_query(
    label: &LabelId,
    labels: &[Label],
    before_ms: i64,
    window_days: u32,
) -> Result<String> {
    let before = Utc
        .timestamp_millis_opt(before_ms)
        .single()
        .context("invalid load-older timestamp")?;
    let after = before
        .checked_sub_signed(Duration::days(i64::from(window_days)))
        .context("load-older window is outside the supported date range")?;
    let label_query = label_to_gmail_query(label, |wanted| {
        labels
            .iter()
            .find(|candidate| candidate.id == *wanted)
            .map(|candidate| candidate.name.clone())
    });
    Ok(format!(
        "{label_query} before:{} after:{}",
        before.format("%Y/%m/%d"),
        after.format("%Y/%m/%d")
    )
    .trim()
    .to_string())
}

pub async fn load_older(
    account: &AccountId,
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
    label: &LabelId,
    before_ms: i64,
    window_days: u32,
) -> Result<LoadOlderStats> {
    if window_days == 0 {
        anyhow::bail!("load-older window must be at least one day");
    }
    let meta_key = format!("oldest_loaded:{account}:{label}");
    let before_ms = store
        .get_meta(&meta_key)
        .await?
        .and_then(|value| value.parse::<i64>().ok())
        .map_or(before_ms, |stored| stored.min(before_ms));
    let labels = store
        .list_labels(&AccountScope::One(account.clone()))
        .await?;
    let query = load_older_query(label, &labels, before_ms, window_days)?;
    let mut ids = Vec::new();
    let mut page_token = None;
    loop {
        let page = client
            .list_threads_page(&query, page_token.as_deref())
            .await?;
        ids.extend(
            page.threads
                .unwrap_or_default()
                .into_iter()
                .map(|thread| thread.id),
        );
        match page.next_page_token {
            Some(token) => page_token = Some(token),
            None => break,
        }
    }

    let hydrated = fetch_and_upsert_threads(client, store.clone(), account, ids).await?;
    if hydrated.failed > 0 {
        anyhow::bail!("load older left {} thread(s) unresolved", hydrated.failed);
    }
    let oldest_ms = hydrated
        .threads
        .iter()
        .map(remote_thread_to_upsert)
        .map(|thread| thread.last_message_at_ms)
        .min();
    let next_before_ms = oldest_ms.unwrap_or(
        Utc.timestamp_millis_opt(before_ms)
            .single()
            .and_then(|before| before.checked_sub_signed(Duration::days(i64::from(window_days))))
            .context("load-older window is outside the supported date range")?
            .timestamp_millis(),
    );
    store
        .set_meta(&meta_key, &next_before_ms.to_string())
        .await?;
    Ok(LoadOlderStats {
        fetched: hydrated.threads.len() as u32,
        oldest_ms,
    })
}

pub async fn bootstrap(
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
) -> Result<BootstrapStats> {
    let mut stats = BootstrapStats::default();

    // 1. Snapshot historyId BEFORE listing. If we did this after, any messages
    //    that arrived during the listing pass would be invisible to subsequent
    //    incremental syncs.
    let profile = client.get_profile().await.context("get_profile")?;
    let account = AccountId::new(profile.email.clone());
    let history_id: u64 = profile
        .history_id
        .parse()
        .context("parsing snapshot historyId")?;
    stats.history_id = history_id;
    stats.email = profile.email.clone();
    info!(history_id, email = %profile.email, "snapshot taken");

    // 2. Labels.
    let remote_labels = client.list_labels().await.context("list_labels")?;
    let labels: Vec<LabelUpsert> = remote_labels
        .into_iter()
        .map(|l| LabelUpsert {
            id: l.id,
            name: l.name,
            system: l.label_type == "system",
            color: l.color.and_then(|c| c.background_color.or(c.text_color)),
        })
        .collect();
    stats.labels = labels.len() as u32;
    store.upsert_labels(&account, labels).await?;
    info!(count = stats.labels, "labels stored");

    // 3. List thread stubs (paginated, no parallelism — pageToken is sequential).
    let mut stubs = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let page = client
            .list_threads_page(BOOTSTRAP_QUERY, page_token.as_deref())
            .await
            .context("list_threads_page")?;
        if let Some(threads) = page.threads {
            stubs.extend(threads);
        }
        match page.next_page_token {
            Some(t) => page_token = Some(t),
            None => break,
        }
    }
    info!(count = stubs.len(), "thread stubs listed");

    // 4. Fetch metadata for each stub. Bounded concurrency — Gmail caps at
    //    ~250 concurrent connections per user, but bootstrap should stay
    //    well-mannered. 10 in flight gives ~10x speedup without aggression.
    let hydrated = fetch_and_upsert_threads(
        client.clone(),
        store.clone(),
        &account,
        stubs.into_iter().map(|stub| stub.id).collect(),
    )
    .await?;
    let threads = hydrated.threads;
    stats.failed_thread_fetches = hydrated.failed;
    info!(
        ok = threads.len(),
        failed = stats.failed_thread_fetches,
        "thread fetch complete"
    );

    // 5. The shared hydration path has persisted the fetched metadata.
    let upserts: Vec<ThreadUpsert> = threads.iter().map(remote_thread_to_upsert).collect();
    stats.threads = upserts.len() as u32;
    stats.messages = upserts.iter().map(|t| t.messages.len() as u32).sum();

    if stats.failed_thread_fetches > 0 {
        anyhow::bail!(
            "bootstrap left {} thread(s) unresolved; cursor was not advanced",
            stats.failed_thread_fetches
        );
    }

    // 6. Persist the cursor LAST — only after a successful write, so a
    //    crash mid-bootstrap on a fresh DB still triggers a re-bootstrap
    //    rather than an incremental sync against a half-populated cache.
    store.set_history_cursor(&account, history_id).await?;

    info!(?stats, "bootstrap complete");
    Ok(stats)
}

pub(crate) fn remote_thread_to_upsert(t: &RemoteThread) -> ThreadUpsert {
    let history_id: u64 = t.history_id.parse().unwrap_or(0);
    let messages: Vec<MessageUpsert> = t.messages.iter().map(remote_message_to_upsert).collect();

    let last_message_at_ms = messages
        .iter()
        .map(|m| m.internal_date_ms)
        .max()
        .unwrap_or(0);
    let last = t.messages.last();
    let subject = last
        .and_then(|m| m.header("Subject"))
        .unwrap_or("")
        .to_string();
    let snippet = last
        .and_then(|m| m.snippet.as_deref())
        .unwrap_or("")
        .to_string();

    // Union of label IDs across all messages — that's how Gmail computes a
    // thread's effective labels. Dedup with a HashSet.
    let mut label_set = std::collections::HashSet::new();
    for m in &t.messages {
        for l in &m.label_ids {
            label_set.insert(l.clone());
        }
    }
    let label_ids: Vec<String> = label_set.into_iter().collect();

    // Participants: from-headers across messages, deduped, in original order.
    let mut participants: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for m in &t.messages {
        if let Some(from) = m.header("From") {
            let from = from.to_string();
            if seen.insert(from.clone()) {
                participants.push(from);
            }
        }
    }

    ThreadUpsert {
        id: t.id.clone(),
        history_id,
        subject,
        snippet,
        participants,
        last_message_at_ms,
        label_ids,
        messages,
    }
}

fn remote_message_to_upsert(m: &RemoteMessage) -> MessageUpsert {
    let to_addrs = m.header("To").map(parse_address_list).unwrap_or_default();
    let cc_addrs = m.header("Cc").map(parse_address_list).unwrap_or_default();
    let headers = MessageHeaders {
        message_id: m.header("Message-Id").map(str::to_string),
        in_reply_to: m.header("In-Reply-To").map(str::to_string),
        references: m.header("References").map(str::to_string),
        reply_to: m.header("Reply-To").map(str::to_string),
        list_unsubscribe: m.header("List-Unsubscribe").map(str::to_string),
        list_unsubscribe_post: m.header("List-Unsubscribe-Post").map(str::to_string),
    };

    MessageUpsert {
        id: m.id.clone(),
        thread_id: m.thread_id.clone(),
        history_id: 0, // metadata format doesn't include per-message historyId
        internal_date_ms: m.internal_date_ms(),
        from: m.header("From").unwrap_or("").to_string(),
        to: to_addrs,
        cc: cc_addrs,
        subject: m.header("Subject").unwrap_or("").to_string(),
        snippet: m.snippet.clone().unwrap_or_default(),
        label_ids: m.label_ids.clone(),
        body_plain: None, // metadata format omits bodies; full fetch on first open
        headers_json: Some(serde_json::to_string(&headers).expect("serializing message headers")),
    }
}

/// Crude RFC 5322 address-list split. Good enough for `To: a@x, b@y` —
/// quoted display names with embedded commas (`"Last, First" <x@y>`) will
/// produce wrong splits. Replace with `mail-parser` when we add full-format
/// fetching.
fn parse_address_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Debug, Default, Clone)]
pub struct IncrementalStats {
    pub events: u32,
    pub echoes_suppressed: u32,
    /// Distinct threads we re-fetched as a result of history events.
    pub threads_refetched: u32,
    pub muted_archived: u32,
    /// `true` if the server replied 404 (gap) and we triggered a
    /// re-bootstrap on the last 7 days.
    pub gap_recovered: bool,
    pub new_cursor: u64,
}

fn is_echo(record: &HistoryRecord, expected: &HashSet<(String, String, bool)>) -> bool {
    if expected.is_empty()
        || !record.messages_added.is_empty()
        || !record.messages_deleted.is_empty()
    {
        return false;
    }

    let mut effects = HashSet::new();
    for (entries, added) in [
        (&record.labels_added, true),
        (&record.labels_removed, false),
    ] {
        for entry in entries {
            if entry.label_ids.is_empty() {
                return false;
            }
            effects.extend(
                entry
                    .label_ids
                    .iter()
                    .map(|label| (entry.message.thread_id.clone(), label.clone(), added)),
            );
        }
    }
    let net: HashSet<_> = effects
        .iter()
        .filter(|(thread, label, added)| {
            !effects.contains(&(thread.clone(), label.clone(), !added))
        })
        .cloned()
        .collect();

    !net.is_empty() && net.is_subset(expected)
}

fn should_auto_archive(label_ids: &[String], muted_label_id: &str) -> bool {
    label_ids.iter().any(|label| label == muted_label_id)
        && label_ids.iter().any(|label| label == "INBOX")
}

/// Incremental sync. Reads the stored cursor, asks Gmail for everything
/// since, re-fetches the touched threads, persists, advances cursor.
///
/// On 404 (cursor too old — Gmail keeps ~7 days of history): falls back to
/// `gap_recover`, which re-bootstraps a 7-day window and reconciles against
/// the local cache by message ID.
pub async fn incremental_sync(
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
) -> Result<IncrementalStats> {
    use mach_core::store::MailStore;
    let mut stats = IncrementalStats::default();
    let account = AccountId::new(client.email().await);

    let cursor = store
        .get_history_cursor(&account)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no history cursor — bootstrap first"))?;
    info!(cursor, "incremental sync starting");

    let recent = store
        .recent_done_outbox(&account, Utc::now().timestamp_millis() - 60_000)
        .await?;
    let mut expected = HashSet::new();
    for op in recent {
        match op.kind {
            mach_core::store::OutboxOpKind::ModifyLabels {
                thread_ids,
                add,
                remove,
            } => {
                for thread in thread_ids {
                    expected.extend(
                        add.iter()
                            .map(|label| {
                                (
                                    thread.as_str().to_string(),
                                    label.as_str().to_string(),
                                    true,
                                )
                            })
                            .chain(remove.iter().map(|label| {
                                (
                                    thread.as_str().to_string(),
                                    label.as_str().to_string(),
                                    false,
                                )
                            })),
                    );
                }
            }
            mach_core::store::OutboxOpKind::Trash { thread_ids } => {
                for thread in thread_ids {
                    expected.insert((thread.as_str().to_string(), "TRASH".into(), true));
                    expected.insert((thread.as_str().to_string(), "INBOX".into(), false));
                }
            }
            _ => {}
        }
    }

    let mut next_token: Option<String> = None;
    let mut touched_threads: std::collections::HashSet<String> = Default::default();
    let mut deleted_messages: std::collections::HashSet<String> = Default::default();
    let mut latest_history_id = cursor;

    loop {
        let page = match client.list_history(cursor, next_token.as_deref()).await {
            Ok(p) => p,
            Err(e) if e.to_string().contains("404") => {
                warn!("history gap detected — falling back to 7-day re-bootstrap");
                return gap_recover(client, store, &account).await;
            }
            Err(e) => return Err(e),
        };
        for record in page.history {
            stats.events += 1;
            if is_echo(&record, &expected) {
                stats.echoes_suppressed += 1;
                continue;
            }
            for r in record
                .messages_added
                .iter()
                .chain(&record.labels_added)
                .chain(&record.labels_removed)
            {
                touched_threads.insert(r.message.thread_id.clone());
            }
            for deleted in &record.messages_deleted {
                touched_threads.insert(deleted.message.thread_id.clone());
                deleted_messages.insert(deleted.message.id.clone());
            }
        }
        if let Some(hid) = page.history_id {
            let parsed = hid.parse::<u64>().context("parsing history page cursor")?;
            latest_history_id = latest_history_id.max(parsed);
        }
        match page.next_page_token {
            Some(t) => next_token = Some(t),
            None => break,
        }
    }

    // Re-fetch every touched thread (cheap — only the changed ones). Bounded
    // concurrency to play nice with Gmail's quota.
    use futures::stream::{self, StreamExt};
    const FETCH_CONCURRENCY: usize = 10;

    store
        .delete_messages(&account, deleted_messages.into_iter().collect())
        .await?;

    let thread_ids: Vec<String> = touched_threads.into_iter().collect();
    let mut threads: Vec<RemoteThread> = Vec::with_capacity(thread_ids.len());
    let mut failures = Vec::new();
    let client_for_stream = client.clone();
    let mut futs = stream::iter(thread_ids.into_iter())
        .map(move |id| {
            let client = client_for_stream.clone();
            async move {
                let result = client.get_thread_metadata_optional(&id).await;
                (id, result)
            }
        })
        .buffer_unordered(FETCH_CONCURRENCY);
    while let Some((id, result)) = futs.next().await {
        match result {
            Ok(Some(thread)) => threads.push(thread),
            Ok(None) => store.delete_thread(&account, id).await?,
            Err(error) => {
                warn!(thread_id = id, error = %error, "thread refetch failed during incremental");
                failures.push((id, error.to_string()));
            }
        }
    }
    stats.threads_refetched = threads.len() as u32;

    if !threads.is_empty() {
        let upserts: Vec<ThreadUpsert> = threads.iter().map(remote_thread_to_upsert).collect();
        store.upsert_threads(&account, upserts).await?;

        let scope = AccountScope::One(account.clone());
        if let Some(muted_label_id) = store
            .list_labels(&scope)
            .await?
            .into_iter()
            .find(|label| label.name == "MACH/Muted")
            .map(|label| label.id)
        {
            let dispatcher = Dispatcher::with_scope(store.clone(), scope);
            for thread in &threads {
                let labels = remote_thread_to_upsert(thread).label_ids;
                if should_auto_archive(&labels, muted_label_id.as_str()) {
                    dispatcher
                        .execute(Action::Archive {
                            thread_ids: vec![ThreadId::new(thread.id.clone())],
                        })
                        .await
                        .context("auto-archiving muted thread")?;
                    stats.muted_archived += 1;
                }
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "incremental sync left {} thread(s) unresolved; cursor was not advanced",
            failures.len()
        );
    }

    store
        .set_history_cursor(&account, latest_history_id)
        .await?;
    stats.new_cursor = latest_history_id;
    info!(?stats, "incremental sync complete");
    Ok(stats)
}

/// Re-bootstrap the last 7 days when the cursor is too stale. Reconciles
/// by *upserting* — we don't drop the rest of the cache, just refresh what
/// fits in the window.
async fn gap_recover(
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
    account: &AccountId,
) -> Result<IncrementalStats> {
    info!("gap_recover: rebuilding the last 7 days");

    let profile = client.get_profile().await?;
    let new_cursor: u64 = profile
        .history_id
        .parse()
        .context("parsing gap-recovery historyId")?;

    let mut stubs = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let page = client
            .list_threads_page("newer_than:7d", page_token.as_deref())
            .await?;
        if let Some(threads) = page.threads {
            stubs.extend(threads);
        }
        match page.next_page_token {
            Some(t) => page_token = Some(t),
            None => break,
        }
    }

    let hydrated = fetch_and_upsert_threads(
        client,
        store.clone(),
        account,
        stubs.into_iter().map(|stub| stub.id).collect(),
    )
    .await?;
    let refetched = hydrated.threads.len() as u32;
    if hydrated.failed > 0 {
        anyhow::bail!(
            "gap recovery left {} thread(s) unresolved; cursor was not advanced",
            hydrated.failed
        );
    }
    store.set_history_cursor(account, new_cursor).await?;

    Ok(IncrementalStats {
        events: 0,
        echoes_suppressed: 0,
        threads_refetched: refetched,
        muted_archived: 0,
        gap_recovered: true,
        new_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(value: serde_json::Value) -> HistoryRecord {
        serde_json::from_value(value).unwrap()
    }

    fn inbox_removed() -> HistoryRecord {
        history(serde_json::json!({
            "labelsRemoved": [{
                "message": {"id": "message", "threadId": "thread"},
                "labelIds": ["INBOX"]
            }]
        }))
    }

    #[test]
    fn exact_echo_is_suppressed() {
        let expected = HashSet::from([("thread".into(), "INBOX".into(), false)]);
        assert!(is_echo(&inbox_removed(), &expected));
    }

    #[test]
    fn echo_with_unexpected_label_is_not_suppressed() {
        let expected = HashSet::from([("thread".into(), "INBOX".into(), false)]);
        let extra = history(serde_json::json!({
            "labelsRemoved": [{
                "message": {"id": "message", "threadId": "thread"},
                "labelIds": ["INBOX", "UNREAD"]
            }]
        }));
        assert!(!is_echo(&extra, &expected));
    }

    #[test]
    fn echo_with_added_message_is_not_suppressed() {
        let expected = HashSet::from([("thread".into(), "INBOX".into(), false)]);
        let message_added = history(serde_json::json!({
            "messagesAdded": [{"message": {"id": "message", "threadId": "thread"}}],
            "labelsRemoved": [{
                "message": {"id": "message", "threadId": "thread"},
                "labelIds": ["INBOX"]
            }]
        }));
        assert!(!is_echo(&message_added, &expected));
    }

    #[test]
    fn empty_expectations_never_suppress() {
        assert!(!is_echo(&inbox_removed(), &HashSet::new()));
    }

    #[test]
    fn muted_inbox_threads_are_auto_archived() {
        assert!(should_auto_archive(
            &["INBOX".into(), "Label_42".into()],
            "Label_42"
        ));
        assert!(!should_auto_archive(&["Label_42".into()], "Label_42"));
        assert!(!should_auto_archive(&["INBOX".into()], "Label_42"));
    }

    #[test]
    fn older_query_maps_labels_and_date_window() {
        let user_id = LabelId::new("Label_42");
        let labels = vec![Label {
            account_id: AccountId::new("me@example.com"),
            id: user_id.clone(),
            name: "Receipts".into(),
            system: false,
            color: None,
            unread_count: None,
        }];
        let before = Utc.with_ymd_and_hms(2026, 9, 4, 12, 0, 0).unwrap();

        assert_eq!(
            label_to_gmail_query(&LabelId::new("INBOX"), |_| None),
            "in:inbox"
        );
        assert_eq!(
            label_to_gmail_query(&LabelId::new("DONE"), |_| None),
            "-in:inbox -in:trash -in:spam"
        );
        assert_eq!(
            load_older_query(&user_id, &labels, before.timestamp_millis(), 30).unwrap(),
            "label:Receipts before:2026/09/04 after:2026/08/05"
        );
    }

    #[test]
    fn message_upsert_serializes_only_present_threading_headers() {
        let remote: RemoteMessage = serde_json::from_value(serde_json::json!({
            "id": "message",
            "threadId": "thread",
            "labelIds": ["INBOX"],
            "snippet": "body",
            "internalDate": "1",
            "payload": {
                "headers": [
                    {"name": "Message-ID", "value": "<message@example.com>"},
                    {"name": "Reply-To", "value": "reply@example.com"},
                    {"name": "List-Unsubscribe", "value": "<https://example.com/u>"},
                    {"name": "List-Unsubscribe-Post", "value": "List-Unsubscribe=One-Click"}
                ]
            }
        }))
        .unwrap();

        let upsert = remote_message_to_upsert(&remote);
        let headers: serde_json::Value =
            serde_json::from_str(upsert.headers_json.as_deref().unwrap()).unwrap();
        assert_eq!(headers["message_id"], "<message@example.com>");
        assert_eq!(headers["reply_to"], "reply@example.com");
        assert_eq!(headers["list_unsubscribe"], "<https://example.com/u>");
        assert_eq!(
            headers["list_unsubscribe_post"],
            "List-Unsubscribe=One-Click"
        );
        assert!(headers.get("in_reply_to").is_none());
        assert!(headers.get("references").is_none());
    }
}
