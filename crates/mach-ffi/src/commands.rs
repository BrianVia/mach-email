//! Ported from `crates/mach-desktop/src/commands.rs`, with Tauri's
//! `AppHandle`/`State`/`emit` replaced by [`Core`] and its listener. Each
//! function returns the value the matching Tauri command hands to JS.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mach_core::ids::{AccountId, AccountScope, DraftId, LabelId, ThreadId};
use mach_core::keymap::Keymap;
use mach_core::store::{
    ActivityEntry, Draft, DraftAttachment, Label, MailStore, ScheduledSend, ThreadSummary,
};
use mach_core::{Action, ActionOutcome, Dispatcher, DraftPatch, UserConfig};
use mach_gmail::{GmailAccountPool, OutboxWorker, TickReport};
use mach_store::SqliteStore;
use serde::Serialize;
use tracing::warn;

use crate::EngineListener;

pub(crate) struct Core {
    pub store: Arc<SqliteStore>,
    pub scope: AccountScope,
    pub dispatcher: Dispatcher,
    pub body_fetchers: Arc<GmailAccountPool>,
    pub default_keymap_toml: String,
    pub user_keymap_toml: Option<String>,
    pub keymap: Keymap,
    pub synced_accounts: Mutex<HashSet<AccountId>>,
    /// Accounts with a sync tick in flight; see [`sync_account`].
    pub syncing_accounts: Arc<Mutex<HashSet<AccountId>>>,
    pub user_config: UserConfig,
    pub listener: Box<dyn EngineListener>,
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "via", "mach")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// Pretty `Result` shape sent to JS: either `{ ok: ... }` or `{ err: "..." }`.
#[derive(Serialize)]
#[serde(untagged)]
pub enum Out<T> {
    Ok { ok: T },
    Err { err: String },
}

impl<T> Out<T> {
    fn ok(v: T) -> Self {
        Out::Ok { ok: v }
    }
    fn err(e: impl std::fmt::Display) -> Self {
        Out::Err { err: e.to_string() }
    }
}

#[derive(Serialize)]
struct MailSyncedPayload {
    account: String,
    new_threads: Vec<ThreadSummary>,
}

#[derive(Serialize)]
struct SyncStatusPayload {
    account: String,
    ok: bool,
    error: Option<String>,
}

fn tick_changed(report: &TickReport) -> bool {
    report.unsnoozed > 0
        || report.sends_fired > 0
        || report.outbox.processed > 0
        || report
            .incremental
            .as_ref()
            .is_some_and(|stats| stats.events > 0)
}

fn new_thread_ids(before: &HashSet<ThreadId>, after: &[ThreadSummary]) -> Vec<ThreadSummary> {
    after
        .iter()
        .filter(|thread| !before.contains(&thread.id))
        .cloned()
        .collect()
}

impl Core {
    fn emit(&self, event: &str, payload: impl Serialize) {
        match serde_json::to_string(&payload) {
            Ok(json) => self.listener.on_event(event.to_string(), json),
            Err(error) => warn!(event, error = %error, "serializing sync event failed"),
        }
    }
}

pub(crate) async fn sync_accounts(core: &Core) -> serde_json::Value {
    let mut synced = 0;
    let mut failed = 0;
    let mut last_error = None;

    let results = futures::future::join_all(
        core.body_fetchers
            .accounts()
            .map(|account| async move { sync_account(core, &account).await }),
    )
    .await;
    for result in results {
        match result {
            Ok(true) => synced += 1,
            Ok(false) => continue,
            Err(message) => {
                failed += 1;
                last_error = Some(message);
            }
        }
    }

    serde_json::json!({
        "synced": synced,
        "failed": failed,
        "last_error": last_error,
    })
}

/// Marks an account as syncing until dropped, so overlapping ticks skip it
/// instead of queueing behind the account's sync lock.
struct InFlight {
    set: Arc<Mutex<HashSet<AccountId>>>,
    account: AccountId,
}

impl InFlight {
    fn claim(set: &Arc<Mutex<HashSet<AccountId>>>, account: &AccountId) -> Option<Self> {
        let fresh = set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(account.clone());
        fresh.then(|| Self {
            set: set.clone(),
            account: account.clone(),
        })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.account);
    }
}

pub(crate) async fn sync_account(core: &Core, account: &AccountId) -> Result<bool, String> {
    let Some(fetcher) = core.body_fetchers.get(account) else {
        return Ok(false);
    };
    if fetcher.client().needs_reauth().await {
        return Ok(false);
    }
    let Some(_in_flight) = InFlight::claim(&core.syncing_accounts, account) else {
        return Ok(false);
    };
    let email = account.as_str().to_string();
    let scope = AccountScope::One(account.clone());
    let before = core
        .store
        .list_threads_in_label(&scope, &LabelId::new("INBOX"), 500)
        .await
        .map(|threads| threads.into_iter().map(|thread| thread.id).collect());
    match fetcher.sync_tick().await {
        Ok(report) => {
            let seen_before = !core
                .synced_accounts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(account.clone());
            let new_threads = if seen_before {
                match before {
                    Ok(before) => core
                        .store
                        .list_threads_in_label(&scope, &LabelId::new("INBOX"), 500)
                        .await
                        .map(|after| new_thread_ids(&before, &after))
                        .unwrap_or_else(|error| {
                            warn!(account = %account, error = %error, "loading new inbox threads failed");
                            Vec::new()
                        }),
                    Err(error) => {
                        warn!(account = %account, error = %error, "snapshotting inbox threads failed");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            if seen_before && (tick_changed(&report) || !new_threads.is_empty()) {
                core.emit(
                    "mail-synced",
                    MailSyncedPayload {
                        account: email.clone(),
                        new_threads,
                    },
                );
            }
            core.emit(
                "sync-status",
                SyncStatusPayload {
                    account: email,
                    ok: true,
                    error: None,
                },
            );
            Ok(true)
        }
        Err(error) => {
            let message = format!("[{account}] {error:#}");
            warn!(account = %account, error = %message, "account sync tick failed");
            core.emit(
                "sync-status",
                SyncStatusPayload {
                    account: email,
                    ok: false,
                    error: Some(message.clone()),
                },
            );
            Err(message)
        }
    }
}

pub(crate) fn frontend_log(level: &str, message: &str) {
    match level {
        "error" => tracing::error!(target: "mach::frontend", "{message}"),
        "warn" => tracing::warn!(target: "mach::frontend", "{message}"),
        _ => tracing::info!(target: "mach::frontend", "{message}"),
    }
}

pub(crate) async fn save_attachment(
    core: &Core,
    account: String,
    message_id: String,
    attachment_id: String,
    filename: String,
) -> Result<String, String> {
    let account = AccountId::new(account);
    let fetcher = core
        .body_fetchers
        .get(&account)
        .ok_or_else(|| "account is offline".to_string())?;
    mach_gmail::save_attachment_to_downloads(
        fetcher.client(),
        &account,
        &message_id,
        &attachment_id,
        &filename,
    )
    .await
    .map(|path| path.display().to_string())
    .map_err(|error| format!("{error:#}"))
}

pub(crate) fn stage_attachment(name: String, bytes: Vec<u8>) -> Result<DraftAttachment, String> {
    let filename = std::path::Path::new(&name)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment")
        .to_string();
    let cache = directories::ProjectDirs::from("com", "via", "mach")
        .map(|dirs| dirs.cache_dir().join("staged"))
        .unwrap_or_else(|| std::env::temp_dir().join("mach-staged"));
    std::fs::create_dir_all(&cache).map_err(|error| error.to_string())?;
    let path = cache.join(format!("{}-{filename}", uuid::Uuid::new_v4()));
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    Ok(DraftAttachment {
        mime_type: mach_gmail::guess_mime_type(&path).into(),
        path: path.display().to_string(),
        filename,
    })
}

pub(crate) async fn dispatch_action(core: &Core, action_json: String) -> Result<ActionOutcome, String> {
    let action: Action =
        serde_json::from_str(&action_json).map_err(|e| format!("parsing action JSON: {e}"))?;
    core.dispatcher.execute(action).await.map_err(|e| e.to_string())
}

pub(crate) async fn flush_outbox(core: &Core) -> Out<serde_json::Value> {
    let mut processed = 0;
    let mut failed = 0;
    let mut last_error = None;

    for account in core.body_fetchers.accounts() {
        let Some(fetcher) = core.body_fetchers.get(&account) else {
            continue;
        };
        let worker = OutboxWorker::new(account.clone(), fetcher.client().clone(), core.store.clone());
        match worker.drain_once(200).await {
            Ok(stats) => {
                processed += stats.processed;
                failed += stats.failed;
                if stats.failed > 0 {
                    last_error = Some(format!(
                        "{} outbox operation(s) failed for {}",
                        stats.failed, account
                    ));
                }
            }
            Err(error) => {
                failed += 1;
                last_error = Some(format!("[{account}] {error:#}"));
            }
        }
    }

    Out::ok(serde_json::json!({
        "processed": processed,
        "failed": failed,
        "last_error": last_error,
    }))
}

pub(crate) async fn unsubscribe_post(url: String) -> Result<(), String> {
    mach_gmail::one_click_unsubscribe(&url)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn unsubscribe_mailto(
    core: &Core,
    account_id: String,
    to: String,
    subject: String,
) -> Result<(), String> {
    let account = AccountId::new(account_id);
    let dispatcher = Dispatcher::with_scope(core.store.clone(), AccountScope::One(account.clone()));
    let composed = dispatcher
        .execute(Action::ComposeNew { account: None })
        .await
        .map_err(|error| error.to_string())?;
    let draft: Draft = serde_json::from_value(
        composed
            .data
            .and_then(|data| data.get("draft").cloned())
            .ok_or("compose_new returned no draft")?,
    )
    .map_err(|error| error.to_string())?;
    dispatcher
        .execute(Action::SaveDraft {
            draft_id: draft.id.clone(),
            patch: DraftPatch {
                to: Some(vec![to]),
                subject: Some(subject),
                body_md: Some("Unsubscribe".into()),
                ..DraftPatch::default()
            },
        })
        .await
        .map_err(|error| error.to_string())?;
    dispatcher
        .execute(Action::SendDraft { draft_id: draft.id })
        .await
        .map_err(|error| error.to_string())?;
    let fetcher = core.body_fetchers.get(&account).ok_or("account is offline")?;
    let report = OutboxWorker::new(account, fetcher.client().clone(), core.store.clone())
        .drain_once(200)
        .await
        .map_err(|error| error.to_string())?;
    if report.failed > 0 {
        return Err(format!("{} outbox operation(s) failed", report.failed));
    }
    Ok(())
}

pub(crate) async fn outbox_summary(core: &Core) -> Out<mach_core::OutboxSummary> {
    match core.store.outbox_summary(&core.scope).await {
        Ok(summary) => Out::ok(summary),
        Err(error) => Out::err(error),
    }
}

pub(crate) async fn list_activity(core: &Core, since_ms: i64, limit: u32) -> Out<Vec<ActivityEntry>> {
    match core.store.list_activity(&core.scope, since_ms, limit).await {
        Ok(entries) => Out::ok(entries),
        Err(error) => Out::err(error),
    }
}

pub(crate) async fn retry_outbox(core: &Core) -> Out<u32> {
    let entries = match core.store.list_outbox(&core.scope).await {
        Ok(entries) => entries,
        Err(error) => return Out::err(error),
    };
    let mut accounts = entries
        .into_iter()
        .filter(|entry| entry.state == "failed")
        .map(|entry| entry.account_id)
        .collect::<Vec<_>>();
    accounts.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    accounts.dedup();
    let mut retried = 0;
    for account in accounts {
        match core.store.retry_failed_outbox(&account, true).await {
            Ok(count) => retried += count,
            Err(error) => return Out::err(error),
        }
    }
    Out::ok(retried)
}

pub(crate) async fn list_threads(core: &Core, label_id: String, limit: u32) -> Out<Vec<ThreadSummary>> {
    match core
        .store
        .list_threads_in_label(&core.scope, &LabelId::new(label_id), limit)
        .await
    {
        Ok(threads) => Out::ok(threads),
        Err(e) => Out::err(e),
    }
}

pub(crate) async fn list_labels(core: &Core) -> Out<Vec<Label>> {
    match core.store.list_labels(&core.scope).await {
        Ok(labels) => Out::ok(labels),
        Err(e) => Out::err(e),
    }
}

pub(crate) async fn list_scheduled(core: &Core) -> Out<Vec<ScheduledSend>> {
    match core.store.list_scheduled(&core.scope).await {
        Ok(sends) => Out::ok(sends),
        Err(error) => Out::err(error),
    }
}

pub(crate) async fn open_draft(core: &Core, draft_id: String) -> Out<Draft> {
    match core.store.find_draft(&core.scope, &DraftId::new(draft_id)).await {
        Ok(Some(draft)) => Out::ok(draft),
        Ok(None) => Out::err("draft not found"),
        Err(error) => Out::err(error),
    }
}

pub(crate) fn send_later_presets() -> Vec<(&'static str, chrono::DateTime<chrono::Utc>)> {
    mach_core::send_later_presets(chrono::Local::now())
}

pub(crate) async fn load_older(core: &Core, label: String, before_ms: i64) -> Out<mach_gmail::LoadOlderStats> {
    match core
        .body_fetchers
        .load_older(&core.scope, &LabelId::new(label), before_ms)
        .await
    {
        Ok(stats) => Out::ok(stats),
        Err(error) => Out::err(error),
    }
}

pub(crate) async fn open_thread(core: &Core, thread_id: String, fetch: bool) -> Out<serde_json::Value> {
    let id = ThreadId::new(thread_id);
    let Ok(Some(summary)) = core.store.get_thread(&core.scope, &id).await else {
        return Out::err("thread not found");
    };
    let thread_scope = AccountScope::One(summary.account_id.clone());
    let body_fetch_error = if fetch {
        match core.body_fetchers.get(&summary.account_id) {
            Some(fetcher) => match fetcher.fetch_if_needed(&id).await {
                Ok(_) => None,
                Err(e) => {
                    warn!(error = format!("{e:#}"), "body backfill failed (open_thread)");
                    Some(format!("{e:#}"))
                }
            },
            None => Some(format!("no Gmail client for {}", summary.account_id)),
        }
    } else {
        None
    };
    let messages = core
        .store
        .list_messages_in_thread(&thread_scope, &id)
        .await
        .unwrap_or_default();
    // Only report the failure when it actually cost the user content.
    let missing_bodies = messages.iter().any(|m| !m.fetched_full);
    Out::ok(serde_json::json!({
        "thread": summary,
        "messages": messages,
        "body_fetch_error": if missing_bodies { body_fetch_error } else { None },
    }))
}

pub(crate) async fn search(core: &Core, query: String, limit: u32) -> Out<Vec<ThreadSummary>> {
    match core.store.search_threads(&core.scope, &query, limit).await {
        Ok(threads) => Out::ok(threads),
        Err(e) => Out::err(e),
    }
}

pub(crate) async fn refetch_thread(core: &Core, thread_id: String) -> Out<serde_json::Value> {
    let id = ThreadId::new(thread_id);
    let Ok(Some(summary)) = core.store.get_thread(&core.scope, &id).await else {
        return Out::err("thread not found");
    };
    let thread_scope = AccountScope::One(summary.account_id.clone());
    let Some(fetcher) = core.body_fetchers.get(&summary.account_id) else {
        return Out::err("offline — cannot refetch");
    };
    if let Err(e) = core
        .store
        .invalidate_thread_bodies(&summary.account_id, &id)
        .await
    {
        return Out::err(e);
    }
    if let Err(e) = fetcher.fetch_if_needed(&id).await {
        warn!(error = %e, "refetch_thread: body backfill failed");
    }
    let messages = core
        .store
        .list_messages_in_thread(&thread_scope, &id)
        .await
        .unwrap_or_default();
    Out::ok(serde_json::json!({
        "thread": summary,
        "messages": messages,
    }))
}

pub(crate) fn keymap_sources(core: &Core) -> serde_json::Value {
    serde_json::json!({
        "defaults": &core.default_keymap_toml,
        "user": &core.user_keymap_toml,
    })
}

pub(crate) fn settings(core: &Core) -> serde_json::Value {
    let mut settings = config_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("settings.toml")).ok())
        .map_or_else(
            || serde_json::json!({}),
            |contents| match toml::from_str::<toml::Value>(&contents) {
                Ok(settings) => serde_json::to_value(settings).unwrap_or_else(|error| {
                    warn!(error = %error, "serializing settings failed");
                    serde_json::json!({})
                }),
                Err(error) => {
                    warn!(error = %error, "parsing settings.toml failed");
                    serde_json::json!({})
                }
            },
        );
    settings["account_labels"] = serde_json::json!(core.user_config.accounts);
    settings
}

pub(crate) fn snippets(core: &Core) -> BTreeMap<String, String> {
    core.user_config.snippets.clone()
}

pub(crate) fn account_status(core: &Core) -> Result<serde_json::Value, String> {
    let mut accounts = core
        .body_fetchers
        .accounts()
        .map(|account| account.to_string())
        .collect::<Vec<_>>();
    accounts.sort();
    let needs_reauth = mach_gmail::credentials::load_all()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|credentials| credentials.needs_reauth())
        .map(|credentials| credentials.email)
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "email": if accounts.len() == 1 {
            accounts.first().cloned().unwrap_or_default()
        } else {
            format!("All accounts ({})", accounts.len())
        },
        "accounts": accounts,
        "account_labels": core.user_config.accounts,
        "online": !core.body_fetchers.is_empty(),
        "needs_reauth": needs_reauth,
    }))
}

pub(crate) async fn add_account(core: &Core, login_hint: Option<String>) -> Result<String, String> {
    let credentials = mach_gmail::add_account(core.store.clone(), login_hint.as_deref())
        .await
        .map_err(|error| format!("{error:#}"))?;
    core.body_fetchers
        .add(&credentials, core.store.clone())
        .await
        .map_err(|error| format!("{error:#}"))?;
    let email = credentials.email;
    core.emit(
        "sync-status",
        SyncStatusPayload {
            account: email.clone(),
            ok: true,
            error: None,
        },
    );
    core.emit(
        "mail-synced",
        MailSyncedPayload {
            account: email.clone(),
            new_threads: Vec::new(),
        },
    );
    Ok(email)
}
