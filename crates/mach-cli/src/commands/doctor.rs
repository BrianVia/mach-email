use anyhow::Result;
use directories::ProjectDirs;
use mach_core::ids::AccountId;
use mach_core::store::MailStore;
use mach_gmail::{
    config::{OAuthConfig, OAuthConfigSource},
    GmailClient,
};

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

    // OAuth client config.
    let config = OAuthConfig::load_with_source();
    let source = match &config {
        Ok((_, OAuthConfigSource::Env)) => "✓ env",
        Ok((_, OAuthConfigSource::File)) => "✓ file",
        Err(_) => "✗ none (read-only mode)",
    };
    println!("OAuth client:    {source}");

    let accounts: Vec<_> = mach_gmail::credentials::load_all()?
        .into_iter()
        .filter(|credentials| selected_account.map_or(true, |email| credentials.email == email))
        .collect();
    for credentials in accounts {
        let needs_reauth = credentials.needs_reauth();
        let account = AccountId::new(credentials.email);
        println!("\naccount:         {account}");
        if needs_reauth {
            println!("NEEDS RE-AUTH: {account} — run: mach auth login");
        }
        let cursor = store.get_history_cursor(&account).await?;
        println!(
            "history cursor:  {}",
            cursor
                .map(|value| value.to_string())
                .unwrap_or("(none — bootstrap first)".into())
        );
        let pending = store.drain_pending_outbox(&account, u32::MAX).await?.len();
        println!("outbox pending:  {pending}");
        let draft_counts = store.draft_state_counts(&account).await?;
        let draft_summary = if draft_counts.is_empty() {
            "(none)".to_string()
        } else {
            draft_counts
                .iter()
                .map(|entry| format!("{}={}", entry.state, entry.count))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!("drafts by state: {draft_summary}");
        let dead_letters = store.dead_letters(&account).await?;
        println!("dead letters:    {}", dead_letters.len());
        for dead_letter in dead_letters {
            println!(
                "  #{} {}: {}",
                dead_letter.id,
                dead_letter.op_kind,
                dead_letter.last_error.as_deref().unwrap_or("(no error)")
            );
        }
        if !needs_reauth {
            if let Ok((config, _)) = &config {
                let client =
                    GmailClient::from_stored_credentials(config.clone(), account.as_str())?;
                match client.get_profile().await {
                    Ok(profile) => println!("remote history:  {} (live)", profile.history_id),
                    Err(error) => println!("⚠ remote check failed: {error}"),
                }
            }
        }
        if simulate_gap {
            store.set_history_cursor(&account, 1).await?;
            println!("✓ cursor reset to 1 — run `mach sync` to exercise gap recovery.");
        }
    }

    Ok(())
}
