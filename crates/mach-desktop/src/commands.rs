//! Tauri invoke handlers — each one is a JS-callable function over IPC.
//!
//! Pattern: the frontend builds an `Action` JSON object and calls
//! `invoke('dispatch_action', { actionJson: '...' })`. The handler parses,
//! dispatches, returns the `ActionOutcome` as JSON. Reads use dedicated
//! handlers (`list_threads`, `open_thread`, `search`) so they can hit the
//! store directly without going through the Action surface — those don't
//! need optimistic-update semantics.

use mach_core::ids::{AccountScope, LabelId, ThreadId};
use mach_core::store::MailStore;
use mach_core::{Action, ActionOutcome};
use mach_gmail::{sync_account_tick, GmailAccountPool, OutboxWorker, TickReport};
use mach_store::SqliteStore;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tracing::warn;

use crate::AppState;

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

fn emit_sync_event<T: Clone + Serialize>(app: &AppHandle, event: &str, payload: T) {
    if let Err(error) = app.emit_to("main", event, payload) {
        warn!(event, error = %error, "emitting sync event failed");
    }
}

pub(crate) async fn sync_accounts(
    app: &AppHandle,
    store: &Arc<SqliteStore>,
    accounts: &GmailAccountPool,
) -> serde_json::Value {
    let mut synced = 0;
    let mut failed = 0;
    let mut last_error = None;

    for account in accounts.accounts() {
        let Some(fetcher) = accounts.get(account) else {
            continue;
        };
        if fetcher.client().needs_reauth().await {
            continue;
        }
        let email = account.as_str().to_string();
        match sync_account_tick(account, fetcher.client().clone(), store.clone()).await {
            Ok(report) => {
                synced += 1;
                if tick_changed(&report) {
                    emit_sync_event(
                        app,
                        "mail-synced",
                        MailSyncedPayload {
                            account: email.clone(),
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
            }
            Err(error) => {
                let message = format!("{error:#}");
                warn!(account = %account, error = %message, "account sync tick failed");
                failed += 1;
                last_error = Some(message.clone());
                emit_sync_event(
                    app,
                    "sync-status",
                    SyncStatusPayload {
                        account: email,
                        ok: false,
                        error: Some(message),
                    },
                );
            }
        }
    }

    serde_json::json!({
        "synced": synced,
        "failed": failed,
        "last_error": last_error,
    })
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
        let Some(fetcher) = accounts.get(account) else {
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
pub async fn sync_now(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Out<serde_json::Value>, String> {
    Ok(Out::ok(
        sync_accounts(&app, &state.store, &state.body_fetchers).await,
    ))
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
pub fn settings() -> serde_json::Value {
    let Some(contents) =
        crate::config_dir().and_then(|dir| std::fs::read_to_string(dir.join("settings.toml")).ok())
    else {
        return serde_json::json!({});
    };

    match toml::from_str::<toml::Value>(&contents) {
        Ok(settings) => serde_json::to_value(settings).unwrap_or_else(|error| {
            warn!(error = %error, "serializing settings failed");
            serde_json::json!({})
        }),
        Err(error) => {
            warn!(error = %error, "parsing settings.toml failed");
            serde_json::json!({})
        }
    }
}

#[tauri::command]
pub fn account_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let needs_reauth = mach_gmail::credentials::load_all()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|credentials| credentials.needs_reauth())
        .map(|credentials| credentials.email)
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "email": if state.account_emails.len() == 1 {
            state.account_emails.first().cloned().unwrap_or_default()
        } else {
            format!("All accounts ({})", state.account_emails.len())
        },
        "accounts": state.account_emails,
        "online": !state.body_fetchers.is_empty(),
        "needs_reauth": needs_reauth,
    }))
}
