//! UniFFI bridge for the native macOS app. [`MachEngine`] does what
//! `crates/mach-desktop/src/main.rs` does before `tauri::Builder` and owns the
//! tokio runtime that runs the sync loop. Every Tauri command has a method of
//! the same name returning the JSON the Tauri command returns to JS.

mod commands;

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use commands::Core;
use mach_core::ids::{AccountId, AccountScope};
use mach_core::keymap::{KeyContext, Keymap, Mode, Resolution};
use mach_core::{Dispatcher, UserConfig};
use mach_gmail::GmailAccountPool;
use mach_store::SqliteStore;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EngineError {
    #[error("{message}")]
    Failed { message: String },
}

impl EngineError {
    fn new(message: impl std::fmt::Display) -> Self {
        Self::Failed {
            message: message.to_string(),
        }
    }
}

/// Receives the events the Tauri app emits to its window: `mail-synced` and
/// `sync-status`, with the same JSON payloads.
#[uniffi::export(callback_interface)]
pub trait EngineListener: Send + Sync {
    fn on_event(&self, name: String, payload_json: String);
}

#[derive(uniffi::Record)]
pub struct AttachmentData {
    pub bytes: Vec<u8>,
    pub mime: String,
}

#[derive(uniffi::Object)]
pub struct MachEngine {
    core: Arc<Core>,
    runtime: tokio::runtime::Runtime,
}

fn json(value: impl Serialize) -> Result<String, EngineError> {
    serde_json::to_string(&value).map_err(EngineError::new)
}

impl MachEngine {
    /// Runs a command on the engine runtime so Swift's executor never has to
    /// drive tokio futures itself.
    async fn run<T, F, Fut>(&self, command: F) -> Result<String, EngineError>
    where
        F: FnOnce(Arc<Core>) -> Fut,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        let value = self
            .runtime
            .spawn(command(self.core.clone()))
            .await
            .map_err(EngineError::new)?
            .map_err(EngineError::new)?;
        json(value)
    }
}

#[uniffi::export]
impl MachEngine {
    #[uniffi::constructor]
    pub fn start(listener: Box<dyn EngineListener>) -> Result<Arc<Self>, EngineError> {
        // Dev builds pick up MACH_GOOGLE_CLIENT_ID/SECRET from a .env in cwd.
        let _ = dotenvy::dotenv();
        init_logging();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("mach-engine")
            .build()
            .map_err(EngineError::new)?;
        let core = runtime
            .block_on(open_core(listener))
            .map_err(|error| EngineError::new(format!("{error:#}")))?;
        let core = Arc::new(core);
        runtime.spawn(sync_loop(core.clone()));
        if let (Some(subscription), Some(client)) = (
            mach_gmail::config::pubsub_subscription(),
            core.body_fetchers.pubsub_client(),
        ) {
            let core = core.clone();
            runtime.spawn(async move {
                let result = mach_gmail::pubsub_pull_loop(client, &subscription, move |email| {
                    let core = core.clone();
                    async move {
                        let _ = commands::sync_account(&core, &AccountId::new(email)).await;
                    }
                })
                .await;
                if let Err(error) = result {
                    warn!(%error, "Pub/Sub pull loop stopped; polling remains active");
                }
            });
        }
        Ok(Arc::new(Self { core, runtime }))
    }

    pub fn frontend_log(&self, level: String, message: String) -> Result<String, EngineError> {
        commands::frontend_log(&level, &message);
        json(())
    }

    pub async fn save_attachment(
        &self,
        account: String,
        message_id: String,
        attachment_id: String,
        filename: String,
    ) -> Result<String, EngineError> {
        self.run(|core| async move {
            commands::save_attachment(&core, account, message_id, attachment_id, filename).await
        })
        .await
    }

    pub fn stage_attachment(&self, name: String, bytes: Vec<u8>) -> Result<String, EngineError> {
        json(commands::stage_attachment(name, bytes).map_err(EngineError::new)?)
    }

    pub async fn dispatch_action(&self, action_json: String) -> Result<String, EngineError> {
        self.run(|core| async move { commands::dispatch_action(&core, action_json).await })
            .await
    }

    pub async fn flush_outbox(&self) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::flush_outbox(&core).await) })
            .await
    }

    pub async fn unsubscribe_post(&self, url: String) -> Result<String, EngineError> {
        self.run(|_| commands::unsubscribe_post(url)).await
    }

    pub async fn unsubscribe_mailto(
        &self,
        account_id: String,
        to: String,
        subject: String,
    ) -> Result<String, EngineError> {
        self.run(|core| async move {
            commands::unsubscribe_mailto(&core, account_id, to, subject).await
        })
        .await
    }

    pub async fn outbox_summary(&self) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::outbox_summary(&core).await) })
            .await
    }

    pub async fn list_activity(&self, since_ms: i64, limit: u32) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::list_activity(&core, since_ms, limit).await) })
            .await
    }

    pub async fn retry_outbox(&self) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::retry_outbox(&core).await) })
            .await
    }

    pub async fn sync_now(&self) -> Result<String, EngineError> {
        self.run(|core| async move {
            Ok(commands::Out::Ok {
                ok: commands::sync_accounts(&core).await,
            })
        })
        .await
    }

    pub async fn list_threads(&self, label_id: String, limit: u32) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::list_threads(&core, label_id, limit).await) })
            .await
    }

    pub async fn list_labels(&self) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::list_labels(&core).await) })
            .await
    }

    pub async fn list_scheduled(&self) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::list_scheduled(&core).await) })
            .await
    }

    pub async fn open_draft(&self, draft_id: String) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::open_draft(&core, draft_id).await) })
            .await
    }

    pub fn send_later_presets(&self) -> Result<String, EngineError> {
        json(commands::send_later_presets())
    }

    pub async fn load_older(&self, label: String, before_ms: i64) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::load_older(&core, label, before_ms).await) })
            .await
    }

    pub async fn open_thread(&self, thread_id: String, fetch: bool) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::open_thread(&core, thread_id, fetch).await) })
            .await
    }

    pub async fn search(&self, query: String, limit: u32) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::search(&core, query, limit).await) })
            .await
    }

    pub async fn refetch_thread(&self, thread_id: String) -> Result<String, EngineError> {
        self.run(|core| async move { Ok(commands::refetch_thread(&core, thread_id).await) })
            .await
    }

    pub fn keymap_sources(&self) -> Result<String, EngineError> {
        json(commands::keymap_sources(&self.core))
    }

    pub fn settings(&self) -> Result<String, EngineError> {
        json(commands::settings(&self.core))
    }

    pub fn snippets(&self) -> Result<String, EngineError> {
        json(commands::snippets(&self.core))
    }

    pub fn account_status(&self) -> Result<String, EngineError> {
        json(commands::account_status(&self.core).map_err(EngineError::new)?)
    }

    pub async fn add_account(&self, login_hint: Option<String>) -> Result<String, EngineError> {
        self.run(|core| async move { commands::add_account(&core, login_hint).await })
            .await
    }

    /// Bytes for `mach://attachment/<account>/<message_id>/<attachment_id>`,
    /// cached on disk like the Tauri URI handler.
    pub async fn attachment(
        &self,
        account: String,
        message_id: String,
        attachment_id: String,
    ) -> Result<AttachmentData, EngineError> {
        let core = self.core.clone();
        self.runtime
            .spawn(async move {
                let account = AccountId::new(account);
                let fetcher = core
                    .body_fetchers
                    .get(&account)
                    .ok_or_else(|| EngineError::new("offline - account unavailable"))?;
                let path = mach_gmail::fetch_attachment_cached(
                    fetcher.client(),
                    &account,
                    &message_id,
                    &attachment_id,
                )
                .await
                .map_err(|error| {
                    warn!(error = %error, "attachment fetch failed");
                    EngineError::new(error)
                })?;
                let bytes = std::fs::read(path).map_err(EngineError::new)?;
                let mime = sniff_mime(&bytes).to_string();
                Ok(AttachmentData { bytes, mime })
            })
            .await
            .map_err(EngineError::new)?
    }

    /// Resolves an accumulated chord (`"g"`, `"g i"`) in `mode` against the
    /// merged keymap. `context_json` is
    /// `{"selection":[..],"current_thread":..,"current_message":..,"current_draft":..}`
    /// (every field optional). Returns the JSON of the desktop's `Resolution`
    /// type in `ui/src/lib/keymap.ts`:
    /// `{"kind":"action","action":{..}}`,
    /// `{"kind":"prefix","continuations":[{"next":"i","action_name":"open_label"}]}`,
    /// or `{"kind":"unbound"}`.
    pub fn resolve_key(&self, mode: String, chord_sequence: String, context_json: String) -> String {
        resolve_key(&self.core.keymap, &mode, &chord_sequence, &context_json).to_string()
    }

    /// `[{"chord":"e","name":"archive"},..]` for `mode`, sorted by chord, like
    /// the desktop keymap's `bindings(mode)` that feeds the command palette.
    pub fn keymap_bindings(&self, mode: String) -> String {
        let bindings = parse_mode(&mode)
            .map(|mode| self.core.keymap.bindings_for(mode))
            .unwrap_or_default();
        serde_json::Value::Array(
            bindings
                .into_iter()
                .map(|(chord, name)| serde_json::json!({ "chord": chord, "name": name }))
                .collect(),
        )
        .to_string()
    }
}

/// Bare action names that are adapter commands rather than `Action`s, so
/// `mach_core::keymap` reports them unbound. The TUI special-cases the same
/// names.
const ADAPTER_ACTIONS: [&str; 2] = ["add_account", "load_older"];

#[derive(Deserialize, Default)]
#[serde(default)]
struct KeyContextJson {
    selection: Vec<String>,
    current_thread: Option<String>,
    current_message: Option<String>,
    current_draft: Option<String>,
}

fn parse_mode(mode: &str) -> Option<Mode> {
    serde_json::from_value(serde_json::Value::String(mode.to_string())).ok()
}

fn resolve_key(keymap: &Keymap, mode: &str, chord: &str, context_json: &str) -> serde_json::Value {
    let unbound = serde_json::json!({ "kind": "unbound" });
    let Some(mode) = parse_mode(mode) else {
        return unbound;
    };
    let context: KeyContextJson = serde_json::from_str(context_json).unwrap_or_default();
    let context = KeyContext {
        selection: context.selection,
        current_thread: context.current_thread,
        current_message: context.current_message,
        current_draft: context.current_draft,
    };
    match keymap.resolve(mode, chord, &context) {
        Resolution::Action(action) => serde_json::json!({ "kind": "action", "action": action }),
        Resolution::AdapterAction(name) => {
            serde_json::json!({ "kind": "action", "action": { "kind": name } })
        }
        Resolution::Prefix(continuations) => serde_json::json!({
            "kind": "prefix",
            "continuations": continuations
                .into_iter()
                .map(|c| serde_json::json!({ "next": c.next, "action_name": c.action_name }))
                .collect::<Vec<_>>(),
        }),
        Resolution::Unbound => keymap
            .bindings_for(mode)
            .into_iter()
            .find(|(bound, name)| bound == chord && ADAPTER_ACTIONS.contains(&name.as_str()))
            .map_or(unbound, |(_, name)| {
                serde_json::json!({ "kind": "action", "action": { "kind": name } })
            }),
    }
}

async fn open_core(listener: Box<dyn EngineListener>) -> anyhow::Result<Core> {
    let dirs = directories::ProjectDirs::from("com", "via", "mach")
        .context("could not resolve OS application-data directory")?;
    let db = dirs.data_dir().join("mach.db");
    std::fs::create_dir_all(dirs.data_dir())?;
    let pool =
        mach_store::open(&db).with_context(|| format!("opening sqlite at {}", db.display()))?;
    let store = Arc::new(SqliteStore::new(pool));
    let accounts = mach_gmail::credentials::load_all()?;
    if accounts.len() == 1 {
        store
            .claim_legacy_account(&AccountId::new(accounts[0].email.clone()))
            .await?;
    }
    let scope = AccountScope::All;
    let config_dir = commands::config_dir();
    let user_config = config_dir
        .as_ref()
        .map(|dir| UserConfig::load(&dir.join("config.toml")))
        .transpose()
        .context("loading user config")?
        .unwrap_or_default();
    let known_accounts = accounts
        .iter()
        .map(|credentials| AccountId::new(credentials.email.clone()))
        .collect::<Vec<_>>();
    let dispatcher =
        Dispatcher::with_scope(store.clone(), scope.clone()).with_user_config(user_config.clone());
    let dispatcher = match user_config.default_account(&known_accounts) {
        Some(account) => dispatcher.with_default_account(account),
        None => dispatcher,
    };
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
    let user_keymap_toml = config_dir.and_then(|dir| std::fs::read_to_string(dir.join("keymap.toml")).ok());
    let mut keymap = Keymap::default_keymap();
    if let Some(user) = &user_keymap_toml {
        match Keymap::from_toml(user) {
            Ok(user) => keymap.merge(user),
            Err(error) => warn!(%error, "parsing keymap.toml failed; using defaults"),
        }
    }
    Ok(Core {
        store,
        scope,
        dispatcher,
        body_fetchers,
        default_keymap_toml: mach_core::keymap::DEFAULT_KEYMAP_TOML.to_string(),
        user_keymap_toml,
        keymap,
        synced_accounts: Mutex::new(HashSet::new()),
        syncing_accounts: Arc::new(Mutex::new(HashSet::new())),
        user_config,
        listener,
    })
}

async fn sync_loop(core: Arc<Core>) {
    let mut interval = tokio::time::interval(Duration::from_secs(15));
    interval.tick().await;
    loop {
        interval.tick().await;
        // One task per account so a slow mailbox never delays the others; an
        // account still mid-tick skips this round.
        for account in core.body_fetchers.accounts() {
            let core = core.clone();
            tokio::spawn(async move {
                let _ = commands::sync_account(&core, &account).await;
            });
        }
    }
}

/// Logs to `native.log` next to the database, truncated per launch. A
/// separate file from the Tauri app's `desktop.log` so both can run at once.
fn init_logging() {
    let file = directories::ProjectDirs::from("com", "via", "mach").and_then(|dirs| {
        std::fs::create_dir_all(dirs.data_dir()).ok()?;
        std::fs::File::create(dirs.data_dir().join("native.log")).ok()
    });
    let builder = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,mach=info".into()),
        )
        .with_ansi(false);
    let _ = match file {
        Some(file) => builder.with_writer(Mutex::new(file)).try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };
}

/// Cheap mime sniffing from magic bytes, enough for the common image formats
/// inline emails use.
fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        "image/png"
    } else if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" {
        "image/jpeg"
    } else if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"heic" | b"heix" | b"mif1" | b"msf1")
    {
        "image/heic"
    } else if bytes.len() >= 4 && &bytes[..4] == b"<svg" {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_key_matches_the_desktop_resolution_json() {
        let keymap = Keymap::default_keymap();
        let ctx = r#"{"selection":["t1"]}"#;

        let prefix = resolve_key(&keymap, "normal", "g", ctx);
        assert_eq!(prefix["kind"], "prefix");
        assert!(prefix["continuations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["next"] == "i"));

        let archive = resolve_key(&keymap, "normal", "e", ctx);
        assert_eq!(archive["kind"], "action");
        assert_eq!(archive["action"]["kind"], "archive");
        assert_eq!(archive["action"]["thread_ids"], serde_json::json!(["t1"]));

        let older = resolve_key(&keymap, "normal", "shift+l", "not json");
        assert_eq!(older, serde_json::json!({"kind":"action","action":{"kind":"load_older"}}));

        assert_eq!(resolve_key(&keymap, "normal", "zz", ctx), serde_json::json!({"kind":"unbound"}));
        assert_eq!(resolve_key(&keymap, "bogus", "e", ctx), serde_json::json!({"kind":"unbound"}));
    }
}
