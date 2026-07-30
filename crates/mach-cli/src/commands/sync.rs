use anyhow::{Context, Result};
use chrono::Utc;
use mach_core::ids::{LabelId, ThreadId};
use mach_core::{Action, Dispatcher};
use mach_gmail::{config::OAuthConfig, GmailClient, OutboxWorker};

use crate::runtime;

pub async fn run(bootstrap: bool) -> Result<()> {
    let config = OAuthConfig::from_env().context("OAuth client credentials not configured")?;
    let client = GmailClient::from_keyring(config)?;
    let store = runtime::open_store()?;

    if bootstrap {
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
        return Ok(());
    }

    // Default `mach sync` = drain pending mutations + un-snooze + fire
    // due send-laters + pull new history.
    let now_ms = Utc::now().timestamp_millis();
    let dispatcher = Dispatcher::new(store.clone());

    // 1) Sweep snoozes that have come due.
    let due = store.find_due_snoozes(now_ms).await?;
    if !due.is_empty() {
        for d in &due {
            let tid = ThreadId::new(d.thread_id.clone());
            // Add INBOX back, drop the MACH/Snoozed label.
            let _ = dispatcher
                .execute(Action::AddLabel {
                    thread_ids: vec![tid.clone()],
                    label_id: LabelId::new("INBOX"),
                })
                .await;
            let _ = dispatcher
                .execute(Action::RemoveLabel {
                    thread_ids: vec![tid],
                    label_id: LabelId::new(d.snoozed_label.clone()),
                })
                .await;
        }
        println!("⏰ Un-snoozed {} thread(s)", due.len());
    }

    // 2) Fire due send-later drafts.
    let due_sends = store.find_due_sends(now_ms).await?;
    for s in &due_sends {
        match dispatcher
            .execute(Action::SendDraft {
                draft_id: mach_core::ids::DraftId::new(s.draft_id.clone()),
            })
            .await
        {
            Ok(_) => {
                store.mark_send_later(&s.send_later_id, "sent").await?;
            }
            Err(e) => {
                eprintln!(
                    "send_later {} failed: {} (will retry next sync)",
                    s.send_later_id, e
                );
            }
        }
    }
    if !due_sends.is_empty() {
        println!("✉ Fired {} send-later draft(s)", due_sends.len());
    }

    // 3) Drain pending outbox to Gmail.
    let outbox = OutboxWorker::new(client.clone(), store.clone());
    let drain = outbox.drain_once(200).await?;
    if drain.processed > 0 || drain.failed > 0 {
        println!(
            "↑ Outbox: {} processed, {} failed",
            drain.processed, drain.failed
        );
    }

    let stats = mach_gmail::incremental_sync(client, store).await?;
    if stats.gap_recovered {
        println!(
            "⚠ Gap recovery: rebuilt last 7 days ({} threads), cursor {}",
            stats.threads_refetched, stats.new_cursor
        );
    } else {
        println!(
            "✓ Incremental: {} events, {} threads refetched, cursor {}",
            stats.events, stats.threads_refetched, stats.new_cursor
        );
    }
    Ok(())
}
