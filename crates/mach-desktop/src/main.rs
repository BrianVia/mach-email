//! mach-desktop — Tauri 2.x native app.
//!
//! The Rust backend holds the singleton `Dispatcher` and `BodyFetcher` and
//! exposes them as Tauri commands. The frontend (SolidJS, in `../ui/`)
//! calls `invoke('dispatch_action', { actionJson })` for every mutation —
//! same `Action` JSON the CLI's `mach do` accepts and the MCP tool surface
//! ships. One protocol, three surfaces.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use mach_core::ids::AccountScope;
use mach_core::Dispatcher;
use mach_core::UserConfig;
use mach_gmail::GmailAccountPool;
use mach_store::SqliteStore;
use tracing::{info, warn};

pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub scope: AccountScope,
    pub dispatcher: Dispatcher,
    pub body_fetchers: Arc<GmailAccountPool>,
    pub default_keymap_toml: String,
    pub user_keymap_toml: Option<String>,
    pub account_emails: Vec<String>,
}

fn config_dir() -> Option<PathBuf> {
    ProjectDirs::from("com", "via", "mach").map(|dirs| dirs.config_dir().to_path_buf())
}

fn db_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "via", "mach")
        .context("could not resolve OS application-data directory")?;
    Ok(dirs.data_dir().join("mach.db"))
}

fn load_user_keymap_toml() -> Option<String> {
    std::fs::read_to_string(config_dir()?.join("keymap.toml")).ok()
}

#[tokio::main]
async fn main() -> Result<()> {
    // Pick up MACH_GOOGLE_CLIENT_ID/SECRET from the repo's .env if invoked
    // from the workspace root. Silent if not found — env vars set in the
    // shell still win.
    let _ = dotenvy::dotenv();

    // Finder/desktop launches have no visible stderr, so log to a file in the
    // data dir (truncated per launch — last session only). Falls back to
    // stderr when the file can't be opened.
    let log_writer: Box<dyn Fn() -> Box<dyn std::io::Write + Send> + Send + Sync> = match db_path()
        .ok()
        .and_then(|db| db.parent().map(|dir| dir.join("desktop.log")))
        .and_then(|path| {
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::File::create(path).ok()
        }) {
        Some(file) => {
            let file = std::sync::Arc::new(file);
            Box::new(move || match file.try_clone() {
                Ok(clone) => Box::new(clone) as _,
                Err(_) => Box::new(std::io::stderr()) as _,
            })
        }
        None => Box::new(|| Box::new(std::io::stderr()) as _),
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,mach=info".into()),
        )
        .with_ansi(false)
        .with_writer(log_writer)
        .init();

    let db = db_path()?;
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pool =
        mach_store::open(&db).with_context(|| format!("opening sqlite at {}", db.display()))?;
    let store = Arc::new(SqliteStore::new(pool));
    let accounts = mach_gmail::credentials::load_all()?;
    if accounts.len() == 1 {
        store
            .claim_legacy_account(&mach_core::ids::AccountId::new(accounts[0].email.clone()))
            .await?;
    }
    let scope = AccountScope::All;
    let user_config = config_dir()
        .map(|dir| UserConfig::load(&dir.join("config.toml")))
        .transpose()
        .context("loading user config")?
        .unwrap_or_default();
    let dispatcher =
        Dispatcher::with_scope(store.clone(), scope.clone()).with_user_config(user_config);

    let body_fetchers = match GmailAccountPool::from_stored_credentials(store.clone()) {
        Ok(pool) => {
            info!(accounts = pool.accounts().count(), "gmail clients up");
            Arc::new(pool)
        }
        Err(e) => {
            warn!(error = %e, "no body fetchers; running offline");
            Arc::new(GmailAccountPool::default())
        }
    };
    let account_emails = accounts.into_iter().map(|account| account.email).collect();

    let state = AppState {
        store,
        scope,
        dispatcher,
        body_fetchers,
        default_keymap_toml: mach_core::keymap::DEFAULT_KEYMAP_TOML.to_string(),
        user_keymap_toml: load_user_keymap_toml(),
        account_emails,
    };

    let cache_dir = ProjectDirs::from("com", "via", "mach")
        .map(|d| d.cache_dir().join("attachments"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mach-attachments"));
    std::fs::create_dir_all(&cache_dir).ok();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            use tauri::Manager;

            let app_handle = app.handle().clone();
            let state = app.state::<AppState>();
            let store = state.store.clone();
            let body_fetchers = state.body_fetchers.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                interval.tick().await;
                loop {
                    interval.tick().await;
                    commands::sync_accounts(&app_handle, &store, &body_fetchers).await;
                }
            });
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("mach", {
            let cache_dir = cache_dir.clone();
            move |ctx, req, responder| {
                let cache_dir = cache_dir.clone();
                let app_handle = ctx.app_handle().clone();
                tokio::spawn(async move {
                    let resp = serve_mach_uri(req, app_handle, &cache_dir).await;
                    responder.respond(resp);
                });
            }
        })
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::dispatch_action,
            commands::list_threads,
            commands::list_labels,
            commands::load_older,
            commands::open_thread,
            commands::refetch_thread,
            commands::search,
            commands::keymap_sources,
            commands::settings,
            commands::account_status,
            commands::flush_outbox,
            commands::outbox_summary,
            commands::retry_outbox,
            commands::sync_now,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri app");

    Ok(())
}

/// Serve `mach://attachment/<message_id>/<attachment_id>`. Caches to disk
/// at `<cache_dir>/<attachment_id>` so subsequent loads are instant.
async fn serve_mach_uri(
    req: tauri::http::Request<Vec<u8>>,
    app: tauri::AppHandle,
    cache_dir: &std::path::Path,
) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::Response;
    use tauri::Manager;

    let uri = req.uri().to_string();
    // Expected: mach://attachment/<message_id>/<attachment_id>
    let path = uri
        .strip_prefix("mach://attachment/")
        .or_else(|| uri.strip_prefix("mach://localhost/attachment/"))
        .unwrap_or("");
    let mut parts = path.split('/');
    let (Some(account), Some(msg_id), Some(att_id)) = (parts.next(), parts.next(), parts.next())
    else {
        return Response::builder()
            .status(400)
            .body(b"bad mach:// URI".to_vec())
            .unwrap();
    };

    // Cache hit?
    let account_id = mach_core::ids::AccountId::new(account);
    let Some(fetcher) = app
        .state::<AppState>()
        .body_fetchers
        .get(&account_id)
        .cloned()
    else {
        return Response::builder()
            .status(503)
            .body(b"offline - account unavailable".to_vec())
            .unwrap();
    };
    let cache_path = cache_dir.join(format!("{account}-{att_id}"));
    if let Ok(bytes) = std::fs::read(&cache_path) {
        let mime = sniff_mime(&bytes);
        return Response::builder()
            .header("Content-Type", mime)
            .header("Cache-Control", "max-age=86400, immutable")
            .body(bytes)
            .unwrap();
    }

    // Cache miss — fetch via Gmail.
    let client = fetcher.client();
    match client.get_attachment_bytes(msg_id, att_id).await {
        Ok(bytes) => {
            let _ = std::fs::write(&cache_path, &bytes);
            let mime = sniff_mime(&bytes);
            Response::builder()
                .header("Content-Type", mime)
                .header("Cache-Control", "max-age=86400, immutable")
                .body(bytes)
                .unwrap()
        }
        Err(e) => {
            tracing::warn!(error = %e, "attachment fetch failed");
            Response::builder()
                .status(502)
                .body(format!("{e}").into_bytes())
                .unwrap()
        }
    }
}

/// Cheap mime sniffing from magic bytes — enough for the common image
/// formats inline emails use.
fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        "image/png"
    } else if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" {
        "image/jpeg"
    } else if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.len() >= 4 && &bytes[..4] == b"<svg" {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}
