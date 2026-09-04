//! Gmail HTTP client, OAuth flow, and sync engine.
//!
//! The sync engine in `sync.rs` is the highest-risk file in the workspace —
//! `historyId` gap recovery is where most TUI mail clients silently drift.

pub mod body;
pub mod body_fetcher;
pub mod client;
pub mod config;
pub mod credentials;
pub mod oauth;
pub mod outbox;
mod search;
pub mod sync;
mod unsubscribe;

pub use body_fetcher::{
    fetch_attachment_cached, guess_mime_type, save_attachment_to_downloads, BodyFetcher,
    FetchOutcome, GmailAccountPool, PullReport, RemoteSearchReport,
};
pub use client::GmailClient;
pub use config::OAuthConfig;
pub use credentials::{CredsError, StoredCredentials};
pub use outbox::{DrainStats, OutboxWorker};
pub use sync::{
    bootstrap, incremental_sync, load_older, sync_account_tick, BootstrapStats, IncrementalStats,
    LoadOlderStats, TickReport,
};
pub use unsubscribe::one_click_unsubscribe;
