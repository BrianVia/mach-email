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
pub mod sync;

pub use body_fetcher::{BodyFetcher, FetchOutcome};
pub use client::GmailClient;
pub use config::OAuthConfig;
pub use credentials::{CredsError, StoredCredentials};
pub use outbox::{DrainStats, OutboxWorker};
pub use sync::{bootstrap, incremental_sync, BootstrapStats, IncrementalStats};
