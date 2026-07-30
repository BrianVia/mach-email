use anyhow::{Context, Result};
use chrono::Utc;
use mach_core::ids::{AccountId, AccountScope, LabelId, ThreadId};
use mach_core::{Action, Dispatcher, MailStore};
use mach_gmail::{config::OAuthConfig, GmailClient, OutboxWorker};

use crate::runtime;

pub async fn run(bootstrap: bool, selected_account: Option<&str>) -> Result<()> {
    let config = OAuthConfig::from_env().context("OAuth client credentials not configured")?;
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

    // Default `mach sync` = drain pending mutations + un-snooze + fire
    // due send-laters + pull new history.
    let now_ms = Utc::now().timestamp_millis();
    let dispatcher = Dispatcher::with_scope(store.clone(), AccountScope::One(account.clone()));

    // 1) Sweep snoozes that have come due.
    let due = store.find_due_snoozes(&account, now_ms).await?;
    if !due.is_empty() {
        for d in &due {
            let tid = ThreadId::new(d.thread_id.clone());
            // Add INBOX back, drop the MACH/Snoozed label.
            dispatcher
                .execute(Action::AddLabel {
                    thread_ids: vec![tid.clone()],
                    label_id: LabelId::new("INBOX"),
                })
                .await
                .context("queueing INBOX restore for due snooze")?;
            dispatcher
                .execute(Action::RemoveLabel {
                    thread_ids: vec![tid],
                    label_id: LabelId::new(d.snoozed_label.clone()),
                })
                .await
                .context("queueing snooze-label removal")?;
        }
        println!("[{email}] ⏰ Un-snoozed {} thread(s)", due.len());
    }

    // 2) Fire due send-later drafts.
    let due_sends = store.find_due_sends(&account, now_ms).await?;
    let mut fired_sends = 0usize;
    for s in &due_sends {
        match dispatcher
            .execute(Action::SendDraft {
                draft_id: mach_core::ids::DraftId::new(s.draft_id.clone()),
            })
            .await
        {
            Ok(_) => {
                store
                    .mark_send_later(&account, &s.send_later_id, "sent")
                    .await?;
                fired_sends += 1;
            }
            Err(e) => {
                eprintln!(
                    "send_later {} failed: {} (will retry next sync)",
                    s.send_later_id, e
                );
            }
        }
    }
    if fired_sends > 0 {
        println!("[{email}] ✉ Fired {fired_sends} send-later draft(s)");
    }

    // 3) Drain pending outbox to Gmail.
    let outbox = OutboxWorker::new(account, client.clone(), store.clone());
    let drain = outbox.drain_once(200).await?;
    if drain.processed > 0 || drain.failed > 0 {
        println!(
            "[{email}] ↑ Outbox: {} processed, {} failed",
            drain.processed, drain.failed,
        );
    }

    let stats = mach_gmail::incremental_sync(client, store).await?;
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
