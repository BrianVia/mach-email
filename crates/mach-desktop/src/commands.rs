//! Tauri invoke handlers — each one is a JS-callable function over IPC.
//!
//! Pattern: the frontend builds an `Action` JSON object and calls
//! `invoke('dispatch_action', { actionJson: '...' })`. The handler parses,
//! dispatches, returns the `ActionOutcome` as JSON. Reads use dedicated
//! handlers (`list_threads`, `open_thread`, `search`) so they can hit the
//! store directly without going through the Action surface — those don't
//! need optimistic-update semantics.

use std::sync::Arc;

use mach_core::ids::{LabelId, ThreadId};
use mach_core::store::MailStore;
use mach_core::{Action, ActionOutcome};
use serde::Serialize;
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
        Out::Err {
            err: e.to_string(),
        }
    }
}

#[tauri::command]
pub async fn dispatch_action(
    state: State<'_, AppState>,
    action_json: String,
) -> Result<ActionOutcome, String> {
    let action: Action = serde_json::from_str(&action_json)
        .map_err(|e| format!("parsing action JSON: {e}"))?;

    // Body backfill on open_thread, same shape as the CLI.
    if let Action::OpenThread { id } = &action {
        if let Some(fetcher) = state.body_fetcher.as_ref() {
            if let Err(e) = fetcher.fetch_if_needed(id).await {
                warn!(error = %e, "body backfill failed");
            }
        }
    }

    state
        .dispatcher
        .execute(action)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_threads(
    state: State<'_, AppState>,
    label_id: String,
    limit: u32,
) -> Result<Out<serde_json::Value>, String> {
    let lid = LabelId::new(label_id);
    match state.store.list_threads_in_label(&lid, limit).await {
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
    if let Some(fetcher) = state.body_fetcher.as_ref() {
        if let Err(e) = fetcher.fetch_if_needed(&id).await {
            warn!(error = %e, "body backfill failed (open_thread)");
        }
    }
    let Ok(Some(summary)) = state.store.get_thread(&id).await else {
        return Ok(Out::err("thread not found"));
    };
    let messages = state
        .store
        .list_messages_in_thread(&id)
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
    match state.store.search_threads(&query, limit).await {
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
    if let Err(e) = state.store.invalidate_thread_bodies(&id).await {
        return Ok(Out::err(e));
    }
    if let Some(fetcher) = state.body_fetcher.as_ref() {
        if let Err(e) = fetcher.fetch_if_needed(&id).await {
            warn!(error = %e, "refetch_thread: body backfill failed");
        }
    } else {
        return Ok(Out::err("offline — cannot refetch"));
    }
    let Ok(Some(summary)) = state.store.get_thread(&id).await else {
        return Ok(Out::err("thread not found"));
    };
    let messages = state
        .store
        .list_messages_in_thread(&id)
        .await
        .unwrap_or_default();
    Ok(Out::ok(serde_json::json!({
        "thread": summary,
        "messages": messages,
    })))
}

#[tauri::command]
pub fn keymap_toml(state: State<'_, AppState>) -> String {
    state.keymap_toml.clone()
}

#[tauri::command]
pub fn account_status(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "email": state.account_email,
        "online": state.body_fetcher.is_some(),
    })
}

// Used only to keep dead_code lint quiet for the shared Out helpers above.
#[allow(dead_code)]
fn _force_use() {
    let _ = Out::<()>::ok(());
    let _ = Out::<()>::err("");
    let _ = Arc::new(());
}
