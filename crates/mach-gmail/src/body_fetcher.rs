//! Lazy full-body fetcher.
//!
//! Bootstrap fetches `format=metadata` — fast, no bodies. The first time a
//! thread is opened, this service detects the gap and pulls `format=full`,
//! decodes the MIME tree, persists `body_plain`/`body_html`, flips the
//! `fetched_full` flag. Idempotent: subsequent calls are no-ops.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use mach_core::{
    ids::{AccountId, AccountScope, LabelId, ThreadId},
    store::{MailStore, ThreadSummary},
};
use mach_store::{MessageBodyUpdate, SqliteStore};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::body::{self, ParsedBody};
use crate::client::{GmailClient, RemoteMessage};

pub fn guess_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

fn attachment_cache_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "via", "mach")
        .map(|dirs| dirs.cache_dir().join("attachments"))
        .unwrap_or_else(|| std::env::temp_dir().join("mach-attachments"))
}

pub async fn fetch_attachment_cached(
    client: &GmailClient,
    account: &AccountId,
    message_id: &str,
    attachment_id: &str,
) -> Result<PathBuf> {
    let cache_dir = attachment_cache_dir();
    std::fs::create_dir_all(&cache_dir).context("creating attachment cache")?;
    let safe_account = account.as_str().replace(['/', '\\'], "_");
    let safe_attachment = attachment_id.replace(['/', '\\'], "_");
    let path = cache_dir.join(format!("{safe_account}-{safe_attachment}"));
    if path.is_file() {
        return Ok(path);
    }
    let bytes = client
        .get_attachment_bytes(message_id, attachment_id)
        .await
        .context("fetching attachment")?;
    std::fs::write(&path, bytes).context("caching attachment")?;
    Ok(path)
}

pub async fn save_attachment_to_downloads(
    client: &GmailClient,
    account: &AccountId,
    message_id: &str,
    attachment_id: &str,
    filename: &str,
) -> Result<PathBuf> {
    let cached = fetch_attachment_cached(client, account, message_id, attachment_id).await?;
    let user_dirs = directories::UserDirs::new().context("resolving home directory")?;
    let downloads = user_dirs
        .download_dir()
        .unwrap_or_else(|| user_dirs.home_dir());
    std::fs::create_dir_all(downloads).context("creating Downloads directory")?;
    let filename = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("attachment");
    let source = std::fs::read(cached).context("reading cached attachment")?;
    for number in 0.. {
        let candidate = if number == 0 {
            filename.to_string()
        } else {
            let path = Path::new(filename);
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or(filename);
            let extension = path.extension().and_then(|value| value.to_str());
            match extension {
                Some(extension) => format!("{stem} ({number}).{extension}"),
                None => format!("{stem} ({number})"),
            }
        };
        let path = downloads.join(candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&source).context("saving attachment")?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).context("creating attachment download"),
        }
    }
    unreachable!()
}

pub struct BodyFetcher {
    account: AccountId,
    client: Arc<GmailClient>,
    store: Arc<SqliteStore>,
    sync_lock: Mutex<()>,
}

/// Account-indexed Gmail services. This is the only owner of choosing which
/// authenticated Gmail client may read remote data for a cached account.
#[derive(Default)]
pub struct GmailAccountPool {
    fetchers: RwLock<HashMap<AccountId, Arc<BodyFetcher>>>,
}

#[derive(Debug, Default)]
pub struct PullReport {
    pub attempted: usize,
    pub succeeded: usize,
    pub failures: Vec<(AccountId, String)>,
    pub needs_reauth: Vec<AccountId>,
}

#[derive(Debug, Default)]
pub struct RemoteSearchReport {
    pub results: Vec<ThreadSummary>,
    pub failures: Vec<(AccountId, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Nothing to do — all messages already have full bodies.
    AlreadyCached,
    /// Fetched and persisted N messages.
    Fetched(usize),
}

impl BodyFetcher {
    pub fn new(account: AccountId, client: Arc<GmailClient>, store: Arc<SqliteStore>) -> Self {
        Self {
            account,
            client,
            store,
            sync_lock: Mutex::new(()),
        }
    }

    /// Build the online body service from configured OAuth client details and
    /// the persisted account credentials. Adapters decide whether failure
    /// means cached-only operation or a hard error.
    pub fn from_stored_credentials(account: &str, store: Arc<SqliteStore>) -> Result<Self> {
        let config = crate::config::OAuthConfig::load()?;
        let client = GmailClient::from_stored_credentials(config, account)?;
        Ok(Self::new(AccountId::new(account), client, store))
    }

    pub fn client(&self) -> &Arc<GmailClient> {
        &self.client
    }

    pub async fn sync_tick(&self) -> Result<crate::sync::TickReport> {
        let _guard = self.sync_lock.lock().await;
        crate::sync::sync_account_tick(&self.account, self.client.clone(), self.store.clone()).await
    }

    /// Check the local cache; if any message in the thread lacks a full body,
    /// fetch `format=full`, walk every message's MIME tree, and persist.
    pub async fn fetch_if_needed(&self, thread_id: &ThreadId) -> Result<FetchOutcome> {
        let messages = self
            .store
            .list_messages_in_thread(&AccountScope::One(self.account.clone()), thread_id)
            .await
            .context("listing messages from cache")?;

        if messages.is_empty() {
            return Ok(FetchOutcome::AlreadyCached);
        }
        if messages.iter().all(|m| m.fetched_full) {
            debug!(thread = thread_id.as_str(), "all messages cached");
            return Ok(FetchOutcome::AlreadyCached);
        }

        info!(thread = thread_id.as_str(), "fetching full bodies");
        let full = self
            .client
            .get_thread_full(thread_id.as_str())
            .await
            .with_context(|| format!("get_thread_full({thread_id})"))?;

        let mut parsed_messages = Vec::with_capacity(full.messages.len());
        let mut updates = Vec::with_capacity(full.messages.len());
        for message in &full.messages {
            let mut parsed = message
                .payload
                .as_ref()
                .map(body::extract)
                .unwrap_or_default();
            if parsed.calendar.is_none() {
                if let Some(attachment_id) = parsed.calendar_attachment_id.as_deref() {
                    let bytes = self
                        .client
                        .get_attachment_bytes(&message.id, attachment_id)
                        .await
                        .context("fetching calendar attachment")?;
                    parsed.calendar =
                        Some(String::from_utf8(bytes).context("calendar attachment is not UTF-8")?);
                }
            }
            updates.push(parse_message_into_update(message, &parsed));
            parsed_messages.push((message, parsed));
        }

        let touched = self
            .store
            .update_message_bodies(&self.account, updates)
            .await
            .context("persisting full bodies")?;
        for (message, body) in parsed_messages {
            self.store
                .upsert_attachments(&self.account, &message.id, body.attachments)
                .await
                .context("persisting attachments")?;
        }
        info!(touched, "body backfill complete");
        Ok(FetchOutcome::Fetched(touched))
    }
}

impl GmailAccountPool {
    pub fn from_stored_credentials(store: Arc<SqliteStore>) -> Result<Self> {
        let accounts = crate::credentials::load_all()?;
        let mut fetchers = HashMap::with_capacity(accounts.len());
        // One account with broken credentials must not take the others
        // offline; skip it loudly and keep the rest of the pool alive.
        for credentials in accounts {
            if credentials.needs_reauth() {
                warn!(
                    account = credentials.email,
                    "skipping account: re-authentication required"
                );
                continue;
            }
            let account = AccountId::new(credentials.email);
            match BodyFetcher::from_stored_credentials(account.as_str(), store.clone()) {
                Ok(fetcher) => {
                    fetchers.insert(account, Arc::new(fetcher));
                }
                Err(e) => {
                    warn!(
                        account = account.as_str(),
                        error = format!("{e:#}"),
                        "skipping account: client setup failed"
                    );
                }
            }
        }
        Ok(Self {
            fetchers: RwLock::new(fetchers),
        })
    }

    pub async fn add(
        &self,
        credentials: &crate::StoredCredentials,
        store: Arc<SqliteStore>,
    ) -> Result<()> {
        let config = crate::config::OAuthConfig::load()?;
        self.add_with_config(credentials, config, store)
    }

    fn add_with_config(
        &self,
        credentials: &crate::StoredCredentials,
        config: crate::config::OAuthConfig,
        store: Arc<SqliteStore>,
    ) -> Result<()> {
        let account = AccountId::new(credentials.email.clone());
        let client = GmailClient::from_credentials(config, credentials.clone())?;
        self.fetchers
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                account.clone(),
                Arc::new(BodyFetcher::new(account, client, store)),
            );
        Ok(())
    }

    pub fn get(&self, account: &AccountId) -> Option<Arc<BodyFetcher>> {
        self.fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(account)
            .cloned()
    }

    pub fn accounts(&self) -> impl Iterator<Item = AccountId> {
        self.fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn is_empty(&self) -> bool {
        self.fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    pub fn pubsub_client(&self) -> Option<Arc<GmailClient>> {
        self.fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .next()
            .map(|fetcher| fetcher.client.clone())
    }

    /// Pull new Gmail history for every account in scope. This deliberately
    /// has no outbox or scheduled-send effects: UI cache freshness must never
    /// surprise the user by delivering old local mutations.
    pub async fn pull_updates(&self, scope: &AccountScope) -> PullReport {
        let selected: Vec<_> = self
            .fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(account, _)| scope.account().map_or(true, |wanted| wanted == *account))
            .map(|(account, fetcher)| (account.clone(), fetcher.clone()))
            .collect();
        let attempted = selected.len();
        let results = stream::iter(selected)
            .map(|(account, fetcher)| async move {
                if fetcher.client.needs_reauth().await {
                    return (account, None, true);
                }
                let result = fetcher.pull_updates().await;
                let needs_reauth = fetcher.client.needs_reauth().await;
                (account, Some(result), needs_reauth)
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;

        let mut report = PullReport {
            attempted,
            ..PullReport::default()
        };
        for (account, result, needs_reauth) in results {
            if needs_reauth {
                report.needs_reauth.push(account.clone());
            }
            match result {
                Some(Ok(())) => report.succeeded += 1,
                Some(Err(error)) => report.failures.push((account, format!("{error:#}"))),
                None => {}
            }
        }
        report
    }

    /// Search Gmail's complete index for every account in scope, cache the
    /// matching metadata, and return one date-sorted account-qualified list.
    pub async fn search_remote(
        &self,
        scope: &AccountScope,
        query: &str,
        limit: u32,
    ) -> RemoteSearchReport {
        let selected: Vec<_> = self
            .fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(account, _)| scope.account().map_or(true, |wanted| wanted == *account))
            .map(|(account, fetcher)| (account.clone(), fetcher.clone()))
            .collect();
        let query = query.to_string();
        let mut searches = stream::iter(selected)
            .map(|(account, fetcher)| {
                let query = query.clone();
                async move {
                    let result = crate::search::search_account(
                        &account,
                        fetcher.client.clone(),
                        fetcher.store.clone(),
                        &query,
                        limit,
                    )
                    .await;
                    (account, result)
                }
            })
            .buffer_unordered(4);

        let mut report = RemoteSearchReport::default();
        while let Some((account, result)) = searches.next().await {
            match result {
                Ok(results) => report.results.extend(results),
                Err(error) => report.failures.push((account, format!("{error:#}"))),
            }
        }
        report.results.sort_by(|left, right| {
            right
                .last_message_at
                .cmp(&left.last_message_at)
                .then_with(|| left.account_id.as_str().cmp(right.account_id.as_str()))
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        report
            .results
            .dedup_by(|left, right| left.account_id == right.account_id && left.id == right.id);
        report.results.truncate(limit as usize);
        report
    }

    pub async fn load_older(
        &self,
        scope: &AccountScope,
        label: &LabelId,
        before_ms: i64,
    ) -> Result<crate::sync::LoadOlderStats> {
        let selected: Vec<_> = self
            .fetchers
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(account, _)| scope.account().map_or(true, |wanted| wanted == *account))
            .map(|(account, fetcher)| (account.clone(), fetcher.clone()))
            .collect();
        let results = stream::iter(selected)
            .map(|(account, fetcher)| {
                let label = label.clone();
                async move {
                    crate::sync::load_older(
                        &account,
                        fetcher.client.clone(),
                        fetcher.store.clone(),
                        &label,
                        before_ms,
                        30,
                    )
                    .await
                    .with_context(|| format!("loading older mail for {account}"))
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;

        let mut total = crate::sync::LoadOlderStats::default();
        for result in results {
            let stats = result?;
            total.fetched += stats.fetched;
            total.oldest_ms = match (total.oldest_ms, stats.oldest_ms) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (None, value) | (value, None) => value,
            };
        }
        Ok(total)
    }
}

impl BodyFetcher {
    async fn pull_updates(&self) -> Result<()> {
        let _guard = self.sync_lock.lock().await;
        if let Err(error) =
            crate::sync::ensure_watch(&self.account, &self.client, &self.store).await
        {
            warn!(account = %self.account, %error, "Gmail watch renewal failed; polling remains active");
        }
        if self
            .store
            .get_history_cursor(&self.account)
            .await?
            .is_none()
        {
            crate::sync::bootstrap(self.client.clone(), self.store.clone()).await?;
        } else {
            crate::sync::incremental_sync(self.client.clone(), self.store.clone()).await?;
        }
        Ok(())
    }
}

fn parse_message_into_update(m: &RemoteMessage, parsed: &ParsedBody) -> MessageBodyUpdate {
    // Prefer raw plain. Fall back to html→text via html2text inside
    // ParsedBody::effective_plain. We persist both: body_plain feeds the
    // FTS5 index + TUI; body_html sticks around for the desktop renderer.
    let body_plain = parsed.effective_plain();
    let body_html = parsed.html.clone();
    let inline_images_json = if parsed.inline_images.is_empty() {
        None
    } else {
        serde_json::to_string(&parsed.inline_images).ok()
    };

    MessageBodyUpdate {
        id: m.id.clone(),
        body_plain,
        body_html,
        calendar_ics: parsed.calendar.clone(),
        inline_images_json,
    }
}

#[cfg(test)]
mod pool_tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn add_makes_account_visible() {
        let store = Arc::new(SqliteStore::new(mach_store::open_in_memory().unwrap()));
        let credentials = crate::StoredCredentials {
            email: "new@example.com".into(),
            refresh_token: "refresh".into(),
            access_token: "access".into(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            needs_reauth: None,
        };
        let pool = GmailAccountPool::default();

        pool.add_with_config(
            &credentials,
            crate::OAuthConfig {
                client_id: "client".into(),
                client_secret: "secret".into(),
            },
            store,
        )
        .unwrap();

        let account = AccountId::new("new@example.com");
        assert_eq!(pool.accounts().collect::<Vec<_>>(), vec![account.clone()]);
        assert!(pool.get(&account).is_some());
    }
}
