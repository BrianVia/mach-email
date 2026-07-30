//! Gmail cache synchronization: bootstrap, incremental history replay, and
//! conservative gap recovery.
//!
//! Bootstrap: snapshot `historyId` *before* the listing pass, fetch labels
//! and the last 30 days of threads, persist to the SQLite cache, store the
//! cursor. Capturing `historyId` after the listing creates a gap that loses
//! messages forever — the bug almost every TUI mail client gets wrong on the
//! first try. The order in [`bootstrap`] is deliberate.
//!
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use mach_core::ids::AccountId;
use mach_core::store::MailStore;
use mach_store::{LabelUpsert, MessageUpsert, SqliteStore, ThreadUpsert};
use tracing::{info, warn};

use crate::client::{GmailClient, RemoteMessage, RemoteThread};

const BOOTSTRAP_QUERY: &str = "newer_than:30d";
const BOOTSTRAP_CONCURRENCY: usize = 10;

#[derive(Debug, Default, Clone)]
pub struct BootstrapStats {
    pub history_id: u64,
    pub email: String,
    pub labels: u32,
    pub threads: u32,
    pub messages: u32,
    pub failed_thread_fetches: u32,
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
    let mut threads: Vec<RemoteThread> = Vec::with_capacity(stubs.len());
    let mut futs = stream::iter(stubs.into_iter().map(|s| s.id))
        .map({
            let client = client.clone();
            move |id| {
                let client = client.clone();
                async move { client.get_thread_metadata(&id).await }
            }
        })
        .buffer_unordered(BOOTSTRAP_CONCURRENCY);

    while let Some(result) = futs.next().await {
        match result {
            Ok(t) => threads.push(t),
            Err(e) => {
                warn!(error = %e, "thread fetch failed");
                stats.failed_thread_fetches += 1;
            }
        }
    }
    info!(
        ok = threads.len(),
        failed = stats.failed_thread_fetches,
        "thread fetch complete"
    );

    // 5. Convert to upsert structs and persist.
    let upserts: Vec<ThreadUpsert> = threads.iter().map(remote_thread_to_upsert).collect();
    stats.threads = upserts.len() as u32;
    stats.messages = upserts.iter().map(|t| t.messages.len() as u32).sum();
    store.upsert_threads(&account, upserts).await?;

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

fn remote_thread_to_upsert(t: &RemoteThread) -> ThreadUpsert {
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
    /// Distinct threads we re-fetched as a result of history events.
    pub threads_refetched: u32,
    /// `true` if the server replied 404 (gap) and we triggered a
    /// re-bootstrap on the last 7 days.
    pub gap_recovered: bool,
    pub new_cursor: u64,
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

    use futures::stream::{self, StreamExt};
    let mut threads: Vec<RemoteThread> = Vec::with_capacity(stubs.len());
    let mut failed_fetches = 0usize;
    let client_for_stream = client.clone();
    let mut futs = stream::iter(stubs.into_iter().map(|s| s.id))
        .map(move |id| {
            let client = client_for_stream.clone();
            async move { client.get_thread_metadata(&id).await }
        })
        .buffer_unordered(10);
    while let Some(res) = futs.next().await {
        match res {
            Ok(thread) => threads.push(thread),
            Err(error) => {
                failed_fetches += 1;
                warn!(error = %error, "thread fetch failed during gap recovery");
            }
        }
    }

    let upserts: Vec<ThreadUpsert> = threads.iter().map(remote_thread_to_upsert).collect();
    let refetched = upserts.len() as u32;
    if !upserts.is_empty() {
        store.upsert_threads(account, upserts).await?;
    }
    if failed_fetches > 0 {
        anyhow::bail!(
            "gap recovery left {failed_fetches} thread(s) unresolved; cursor was not advanced"
        );
    }
    store.set_history_cursor(account, new_cursor).await?;

    Ok(IncrementalStats {
        events: 0,
        threads_refetched: refetched,
        gap_recovered: true,
        new_cursor,
    })
}
