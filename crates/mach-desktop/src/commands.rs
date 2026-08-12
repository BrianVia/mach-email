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
use mach_gmail::{GmailAccountPool, OutboxWorker};
use mach_store::SqliteStore;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;
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
pub async fn open_thread(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Out<serde_json::Value>, String> {
    let id = ThreadId::new(thread_id);
    let Ok(Some(summary)) = state.store.get_thread(&state.scope, &id).await else {
        return Ok(Out::err("thread not found"));
    };
    let thread_scope = AccountScope::One(summary.account_id.clone());
    if let Some(fetcher) = state.body_fetchers.get(&summary.account_id) {
        if let Err(e) = fetcher.fetch_if_needed(&id).await {
            warn!(error = %e, "body backfill failed (open_thread)");
        }
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
pub fn account_status(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "email": if state.account_emails.len() == 1 {
            state.account_emails.first().cloned().unwrap_or_default()
        } else {
            format!("All accounts ({})", state.account_emails.len())
        },
        "accounts": state.account_emails,
        "online": !state.body_fetchers.is_empty(),
    })
}
