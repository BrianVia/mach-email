use anyhow::Result;
use directories::ProjectDirs;
use mach_core::store::MailStore;
use mach_gmail::{config::OAuthConfig, GmailClient};

use crate::runtime;

/// Print environment + cache health. With `--simulate-gap`, deliberately
/// wipe the sync cursor to force gap recovery on the next `mach sync` — a
/// regression check for the most fragile branch of the sync engine.
pub async fn run(simulate_gap: bool) -> Result<()> {
    println!("== mach doctor ==\n");

    // Project dirs.
    if let Some(dirs) = ProjectDirs::from("com", "via", "mach") {
        println!("config dir:      {}", dirs.config_dir().display());
        println!("data dir:        {}", dirs.data_dir().display());
    } else {
        println!("⚠ could not resolve OS dirs");
    }

    // DB.
    let store = runtime::open_store()?;
    let cursor = store.get_history_cursor().await?;
    println!(
        "history cursor:  {}",
        cursor
            .map(|c| c.to_string())
            .unwrap_or("(none — bootstrap first)".into())
    );

    // OAuth env.
    let creds_ok = OAuthConfig::from_env().is_ok();
    println!(
        "MACH_GOOGLE_*    {}",
        if creds_ok {
            "✓ set"
        } else {
            "✗ missing (read-only mode)"
        }
    );

    if creds_ok {
        let client = GmailClient::from_stored_credentials(OAuthConfig::from_env()?)?;
        let email = client.email().await;
        println!("account:         {email}");
        match client.get_profile().await {
            Ok(p) => println!("remote history:  {} (live)", p.history_id),
            Err(e) => println!("⚠ remote check failed: {e}"),
        }
    }

    let pending = store.drain_pending_outbox(1).await?.len();
    println!("outbox pending:  {pending}");

    if simulate_gap {
        println!("\n--simulate-gap: wiping history cursor to force gap recovery on next sync.");
        // We set it to a clearly-stale value (1). Gmail will 404 the
        // history endpoint and we'll fall through to the 7-day rebuild.
        store.set_history_cursor(1).await?;
        println!("✓ cursor reset to 1 — run `mach sync` to exercise gap recovery.");
    }

    Ok(())
}
