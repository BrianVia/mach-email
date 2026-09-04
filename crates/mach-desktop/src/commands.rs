//! Tauri invoke handlers — each one is a JS-callable function over IPC.
//!
//! Pattern: the frontend builds an `Action` JSON object and calls
//! `invoke('dispatch_action', { actionJson: '...' })`. The handler parses,
//! dispatches, returns the `ActionOutcome` as JSON. Reads use dedicated
//! handlers (`list_threads`, `open_thread`, `search`) so they can hit the
//! store directly without going through the Action surface — those don't
//! need optimistic-update semantics.

use mach_core::ids::{AccountId, AccountScope, DraftId, LabelId, ThreadId};
use mach_core::store::{
    ActivityEntry, Draft, DraftAttachment, Label, MailStore, ScheduledSend, ThreadSummary,
};
use mach_core::{Action, ActionOutcome, Dispatcher, DraftPatch};
use mach_gmail::{GmailAccountPool, OutboxWorker, TickReport};
use mach_store::SqliteStore;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::warn;

use crate::AppState;

#[tauri::command]
pub async fn save_attachment(
    state: State<'_, AppState>,
    account: String,
    message_id: String,
    attachment_id: String,
    filename: String,
) -> Result<String, String> {
    let account = AccountId::new(account);
    let fetcher = state
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

#[tauri::command]
pub fn stage_attachment(name: String, bytes: Vec<u8>) -> Result<DraftAttachment, String> {
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

#[derive(Clone, Serialize)]
struct MailSyncedPayload {
    account: String,
    new_threads: Vec<ThreadSummary>,
}

#[derive(Clone, Serialize)]
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

pub(crate) fn new_thread_ids(
    before: &HashSet<ThreadId>,
    after: &[ThreadSummary],
) -> Vec<ThreadSummary> {
    after
        .iter()
        .filter(|thread| !before.contains(&thread.id))
        .cloned()
        .collect()
}

fn emit_sync_event<T: Clone + Serialize>(app: &AppHandle, event: &str, payload: T) {
    if let Err(error) = app.emit_to("main", event, payload) {
        warn!(event, error = %error, "emitting sync event failed");
    }
}

pub(crate) async fn sync_accounts(
    app: &AppHandle,
    accounts: &GmailAccountPool,
) -> serde_json::Value {
    let mut synced = 0;
    let mut failed = 0;
    let mut last_error = None;

    for account in accounts.accounts() {
        match sync_account(app, accounts, &account).await {
            Ok(true) => {
                synced += 1;
            }
            Ok(false) => continue,
            Err(message) => {
                failed += 1;
                last_error = Some(message.clone());
            }
        }
    }

    serde_json::json!({
        "synced": synced,
        "failed": failed,
        "last_error": last_error,
    })
}

pub(crate) async fn sync_account(
    app: &AppHandle,
    accounts: &GmailAccountPool,
    account: &AccountId,
) -> Result<bool, String> {
    let Some(fetcher) = accounts.get(account) else {
        return Ok(false);
    };
    if fetcher.client().needs_reauth().await {
        return Ok(false);
    }
    let state = app.state::<AppState>();
    let email = account.as_str().to_string();
    let scope = AccountScope::One(account.clone());
    let before = state
        .store
        .list_threads_in_label(&scope, &LabelId::new("INBOX"), 500)
        .await
        .map(|threads| threads.into_iter().map(|thread| thread.id).collect());
    match fetcher.sync_tick().await {
        Ok(report) => {
            let seen_before = !state
                .synced_accounts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(account.clone());
            let new_threads = if seen_before {
                match before {
                    Ok(before) => state
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
                emit_sync_event(
                    app,
                    "mail-synced",
                    MailSyncedPayload {
                        account: email.clone(),
                        new_threads,
                    },
                );
            }
            emit_sync_event(
                app,
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
            let message = format!("{error:#}");
            warn!(account = %account, error = %message, "account sync tick failed");
            emit_sync_event(
                app,
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

#[tauri::command]
pub async fn dispatch_action(
    state: State<'_, AppState>,
    action_json: String,
) -> Result<ActionOutcome, String> {
    let action: Action =
        serde_json::from_str(&action_json).map_err(|e| format!("parsing action JSON: {e}"))?;

    state
        .dispatcher
        .execute(action)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn drain_outbox(
    store: &Arc<SqliteStore>,
    accounts: &GmailAccountPool,
) -> serde_json::Value {
    let mut processed = 0;
    let mut failed = 0;
    let mut last_error = None;

    for account in accounts.accounts() {
        let Some(fetcher) = accounts.get(&account) else {
            continue;
        };
        let worker = OutboxWorker::new(account.clone(), fetcher.client().clone(), store.clone());
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
                last_error = Some(error.to_string());
            }
        }
    }

    serde_json::json!({
        "processed": processed,
        "failed": failed,
        "last_error": last_error,
    })
}

#[tauri::command]
pub async fn flush_outbox(state: State<'_, AppState>) -> Result<Out<serde_json::Value>, String> {
    Ok(Out::ok(
        drain_outbox(&state.store, &state.body_fetchers).await,
    ))
}

#[tauri::command]
pub async fn unsubscribe_post(url: String) -> Result<(), String> {
    mach_gmail::one_click_unsubscribe(&url)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn unsubscribe_mailto(
    state: State<'_, AppState>,
    account_id: String,
    to: String,
    subject: String,
) -> Result<(), String> {
    let account = AccountId::new(account_id);
    let dispatcher =
        Dispatcher::with_scope(state.store.clone(), AccountScope::One(account.clone()));
    let composed = dispatcher
        .execute(Action::ComposeNew)
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
    let fetcher = state
        .body_fetchers
        .get(&account)
        .ok_or("account is offline")?;
    let report = OutboxWorker::new(account, fetcher.client().clone(), state.store.clone())
        .drain_once(200)
        .await
        .map_err(|error| error.to_string())?;
    if report.failed > 0 {
        return Err(format!("{} outbox operation(s) failed", report.failed));
    }
    Ok(())
}

#[tauri::command]
pub async fn outbox_summary(
    state: State<'_, AppState>,
) -> Result<Out<mach_core::OutboxSummary>, String> {
    Ok(match state.store.outbox_summary(&state.scope).await {
        Ok(summary) => Out::ok(summary),
        Err(error) => Out::err(error),
    })
}

#[tauri::command]
pub async fn list_activity(
    state: State<'_, AppState>,
    since_ms: i64,
    limit: u32,
) -> Result<Out<Vec<ActivityEntry>>, String> {
    Ok(
        match state
            .store
            .list_activity(&state.scope, since_ms, limit)
            .await
        {
            Ok(entries) => Out::ok(entries),
            Err(error) => Out::err(error),
        },
    )
}

#[tauri::command]
pub async fn retry_outbox(state: State<'_, AppState>) -> Result<Out<u32>, String> {
    let entries = match state.store.list_outbox(&state.scope).await {
        Ok(entries) => entries,
        Err(error) => return Ok(Out::err(error)),
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
        match state.store.retry_failed_outbox(&account).await {
            Ok(count) => retried += count,
            Err(error) => return Ok(Out::err(error)),
        }
    }
    Ok(Out::ok(retried))
}

#[tauri::command]
pub async fn sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Out<serde_json::Value>, String> {
    Ok(Out::ok(sync_accounts(&app, &state.body_fetchers).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mach_core::ids::AccountId;

    fn thread(id: &str) -> ThreadSummary {
        ThreadSummary {
            account_id: AccountId::new("me@example.com"),
            id: ThreadId::new(id),
            subject: String::new(),
            snippet: String::new(),
            participants: Vec::new(),
            last_message_at: Utc::now(),
            message_count: 1,
            unread: true,
            starred: false,
            label_ids: vec![LabelId::new("INBOX")],
        }
    }

    #[test]
    fn finds_only_new_thread_summaries() {
        let before = HashSet::from([ThreadId::new("existing")]);
        let after = vec![thread("new"), thread("existing")];

        let found = new_thread_ids(&before, &after);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, ThreadId::new("new"));
    }
}

#[tauri::command]
pub async fn list_threads(
    state: State<'_, AppState>,
    label_id: String,
    limit: u32,
) -> Result<Out<serde_json::Value>, String> {
    let lid = LabelId::new(label_id);
    match state
        .store
        .list_threads_in_label(&state.scope, &lid, limit)
        .await
    {
        Ok(threads) => Ok(Out::ok(serde_json::to_value(threads).unwrap())),
        Err(e) => Ok(Out::err(e)),
    }
}

#[tauri::command]
pub async fn list_labels(state: State<'_, AppState>) -> Result<Out<Vec<Label>>, String> {
    match state.store.list_labels(&state.scope).await {
        Ok(labels) => Ok(Out::ok(labels)),
        Err(e) => Ok(Out::err(e)),
    }
}

#[tauri::command]
pub async fn list_scheduled(state: State<'_, AppState>) -> Result<Out<Vec<ScheduledSend>>, String> {
    Ok(match state.store.list_scheduled(&state.scope).await {
        Ok(sends) => Out::ok(sends),
        Err(error) => Out::err(error),
    })
}

#[tauri::command]
pub async fn open_draft(
    state: State<'_, AppState>,
    draft_id: String,
) -> Result<Out<Draft>, String> {
    Ok(
        match state
            .store
            .find_draft(&state.scope, &DraftId::new(draft_id))
            .await
        {
            Ok(Some(draft)) => Out::ok(draft),
            Ok(None) => Out::err("draft not found"),
            Err(error) => Out::err(error),
        },
    )
}

#[tauri::command]
pub fn send_later_presets() -> Vec<(&'static str, chrono::DateTime<chrono::Utc>)> {
    mach_core::send_later_presets(chrono::Local::now())
}

#[tauri::command]
pub async fn load_older(
    state: State<'_, AppState>,
    label: String,
    before_ms: i64,
) -> Result<Out<mach_gmail::LoadOlderStats>, String> {
    match state
        .body_fetchers
        .load_older(&state.scope, &LabelId::new(label), before_ms)
        .await
    {
        Ok(stats) => Ok(Out::ok(stats)),
        Err(error) => Ok(Out::err(error)),
    }
}

#[tauri::command]
pub async fn open_thread(
    state: State<'_, AppState>,
    thread_id: String,
    fetch: bool,
) -> Result<Out<serde_json::Value>, String> {
    let id = ThreadId::new(thread_id);
    let Ok(Some(summary)) = state.store.get_thread(&state.scope, &id).await else {
        return Ok(Out::err("thread not found"));
    };
    let thread_scope = AccountScope::One(summary.account_id.clone());
    let body_fetch_error = if fetch {
        match state.body_fetchers.get(&summary.account_id) {
            Some(fetcher) => match fetcher.fetch_if_needed(&id).await {
                Ok(_) => None,
                Err(e) => {
                    warn!(
                        error = format!("{e:#}"),
                        "body backfill failed (open_thread)"
                    );
                    Some(format!("{e:#}"))
                }
            },
            None => Some(format!("no Gmail client for {}", summary.account_id)),
        }
    } else {
        None
    };
    let messages = state
        .store
        .list_messages_in_thread(&thread_scope, &id)
        .await
        .unwrap_or_default();
    // Only report the failure when it actually cost the user content —
    // opening an already-cached thread offline is fine.
    let missing_bodies = messages.iter().any(|m| !m.fetched_full);
    Ok(Out::ok(serde_json::json!({
        "thread": summary,
        "messages": messages,
        "body_fetch_error": if missing_bodies { body_fetch_error } else { None },
    })))
}

#[tauri::command]
pub async fn search(
    state: State<'_, AppState>,
    query: String,
    limit: u32,
) -> Result<Out<serde_json::Value>, String> {
    match state
        .store
        .search_threads(&state.scope, &query, limit)
        .await
    {
        Ok(threads) => Ok(Out::ok(serde_json::to_value(threads).unwrap())),
        Err(e) => Ok(Out::err(e)),
    }
}

/// Force a fresh `format=full` fetch for a thread. Wipes cached
/// `body_plain`/`body_html`/`inline_images_json` and re-runs the body
/// fetcher. Used by the desktop's `ctrl+r` in reading mode.
#[tauri::command]
pub async fn refetch_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Out<serde_json::Value>, String> {
    let id = ThreadId::new(thread_id);
    let Ok(Some(summary)) = state.store.get_thread(&state.scope, &id).await else {
        return Ok(Out::err("thread not found"));
    };
    let thread_scope = AccountScope::One(summary.account_id.clone());
    let Some(fetcher) = state.body_fetchers.get(&summary.account_id) else {
        return Ok(Out::err("offline — cannot refetch"));
    };
    if let Err(e) = state
        .store
        .invalidate_thread_bodies(&summary.account_id, &id)
        .await
    {
        return Ok(Out::err(e));
    }
    if let Err(e) = fetcher.fetch_if_needed(&id).await {
        warn!(error = %e, "refetch_thread: body backfill failed");
    }
    let messages = state
        .store
        .list_messages_in_thread(&thread_scope, &id)
        .await
        .unwrap_or_default();
    Ok(Out::ok(serde_json::json!({
        "thread": summary,
        "messages": messages,
    })))
}

#[tauri::command]
pub fn keymap_sources(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "defaults": &state.default_keymap_toml,
        "user": &state.user_keymap_toml,
    })
}

#[tauri::command]
pub fn settings(state: State<'_, AppState>) -> serde_json::Value {
    let mut settings = crate::config_dir()
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
    settings["account_labels"] = serde_json::json!(state.user_config.accounts);
    settings
}

#[tauri::command]
pub fn snippets(state: State<'_, AppState>) -> BTreeMap<String, String> {
    state.user_config.snippets.clone()
}

#[tauri::command]
pub fn account_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let mut accounts = state
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
        "account_labels": state.user_config.accounts,
        "online": !state.body_fetchers.is_empty(),
        "needs_reauth": needs_reauth,
    }))
}

#[tauri::command]
pub async fn add_account(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    let credentials = mach_gmail::add_account(state.store.clone())
        .await
        .map_err(|error| format!("{error:#}"))?;
    state
        .body_fetchers
        .add(&credentials, state.store.clone())
        .await
        .map_err(|error| format!("{error:#}"))?;
    let email = credentials.email;
    emit_sync_event(
        &app,
        "sync-status",
        SyncStatusPayload {
            account: email.clone(),
            ok: true,
            error: None,
        },
    );
    emit_sync_event(
        &app,
        "mail-synced",
        MailSyncedPayload {
            account: email.clone(),
            new_threads: Vec::new(),
        },
    );
    Ok(email)
}
