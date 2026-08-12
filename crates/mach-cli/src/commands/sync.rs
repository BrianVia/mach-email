use anyhow::{Context, Result};
use mach_core::ids::AccountId;
use mach_core::MailStore;
use mach_gmail::{config::OAuthConfig, GmailClient};

use crate::runtime;

pub async fn run(bootstrap: bool, selected_account: Option<&str>) -> Result<()> {
    let config = OAuthConfig::load().context("OAuth client credentials not configured")?;
    let store = runtime::open_store().await?;
    let accounts: Vec<_> = mach_gmail::credentials::load_all()?
        .into_iter()
        .filter(|credentials| selected_account.map_or(true, |email| credentials.email == email))
        .collect();
    if accounts.is_empty() {
        anyhow::bail!("no matching Gmail accounts; run `mach auth login`");
    }
    let mut failures = Vec::new();
    for credentials in accounts {
        let email = credentials.email;
        if let Err(error) = sync_account(bootstrap, &email, config.clone(), store.clone()).await {
            eprintln!("[{email}] sync failed: {error:#}");
            failures.push(email);
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("sync failed for: {}", failures.join(", "));
    }
    Ok(())
}

async fn sync_account(
    bootstrap: bool,
    email: &str,
    config: OAuthConfig,
    store: std::sync::Arc<mach_store::SqliteStore>,
) -> Result<()> {
    let account = AccountId::new(email);
    let client = GmailClient::from_stored_credentials(config, email)?;

    if bootstrap_account(bootstrap, email, client.clone(), store.clone()).await? {
        return Ok(());
    }

    let report = mach_gmail::sync_account_tick(&account, client, store).await?;
    if report.unsnoozed > 0 {
        println!("[{email}] ⏰ Un-snoozed {} thread(s)", report.unsnoozed);
    }
    if report.sends_fired > 0 {
        println!(
            "[{email}] ✉ Fired {} send-later draft(s)",
            report.sends_fired
        );
    }
    if report.outbox.processed > 0 || report.outbox.failed > 0 {
        println!(
            "[{email}] ↑ Outbox: {} processed, {} failed",
            report.outbox.processed, report.outbox.failed,
        );
    }
    if let Some(stats) = report.incremental {
        if stats.gap_recovered {
            println!(
                "[{email}] ⚠ Gap recovery: rebuilt last 7 days ({} threads), cursor {}",
                stats.threads_refetched, stats.new_cursor
            );
        } else {
            println!(
                "[{email}] ✓ Incremental: {} events, {} threads refetched, cursor {}",
                stats.events, stats.threads_refetched, stats.new_cursor
            );
        }
    }
    Ok(())
}

/// Ensure one authenticated account has an initial local snapshot. Both login
/// and normal sync use this boundary so "new account" behavior has one owner.
pub(crate) async fn bootstrap_account(
    force: bool,
    email: &str,
    client: std::sync::Arc<GmailClient>,
    store: std::sync::Arc<mach_store::SqliteStore>,
) -> Result<bool> {
    let account = AccountId::new(email);
    if !force && store.get_history_cursor(&account).await?.is_some() {
        return Ok(false);
    }
    if !force {
        println!("[{email}] No sync cursor; bootstrapping account");
    }
    let stats = mach_gmail::bootstrap(client, store).await?;
    println!(
        "✓ Bootstrap complete: {} threads / {} messages / {} labels (account: {}, history_id: {})",
        stats.threads, stats.messages, stats.labels, stats.email, stats.history_id
    );
    if stats.failed_thread_fetches > 0 {
        println!(
            "  ⚠ {} thread fetch(es) failed — see logs (rerun to retry)",
            stats.failed_thread_fetches
        );
    }
    Ok(true)
}
