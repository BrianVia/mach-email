use anyhow::Result;
use directories::ProjectDirs;
use mach_core::ids::AccountId;
use mach_core::store::MailStore;
use mach_gmail::{config::OAuthConfig, GmailClient};

use crate::runtime;

/// Print environment + cache health. With `--simulate-gap`, deliberately
/// wipe the sync cursor to force gap recovery on the next `mach sync` — a
/// regression check for the most fragile branch of the sync engine.
pub async fn run(simulate_gap: bool, selected_account: Option<&str>) -> Result<()> {
    println!("== mach doctor ==\n");

    // Project dirs.
    if let Some(dirs) = ProjectDirs::from("com", "via", "mach") {
        println!("config dir:      {}", dirs.config_dir().display());
        println!("data dir:        {}", dirs.data_dir().display());
    } else {
        println!("⚠ could not resolve OS dirs");
    }

    // DB.
    let store = runtime::open_store().await?;

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

    let accounts: Vec<_> = mach_gmail::credentials::load_all()?
        .into_iter()
        .filter(|credentials| selected_account.map_or(true, |email| credentials.email == email))
        .collect();
    for credentials in accounts {
        let account = AccountId::new(credentials.email);
        println!("\naccount:         {account}");
        let cursor = store.get_history_cursor(&account).await?;
        println!(
            "history cursor:  {}",
            cursor
                .map(|value| value.to_string())
                .unwrap_or("(none — bootstrap first)".into())
        );
        let pending = store.drain_pending_outbox(&account, u32::MAX).await?.len();
        println!("outbox pending:  {pending}");
        if creds_ok {
            let client =
                GmailClient::from_stored_credentials(OAuthConfig::from_env()?, account.as_str())?;
            match client.get_profile().await {
                Ok(profile) => println!("remote history:  {} (live)", profile.history_id),
                Err(error) => println!("⚠ remote check failed: {error}"),
            }
        }
        if simulate_gap {
            store.set_history_cursor(&account, 1).await?;
            println!("✓ cursor reset to 1 — run `mach sync` to exercise gap recovery.");
        }
    }

    Ok(())
}
