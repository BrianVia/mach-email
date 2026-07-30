use anyhow::{Context, Result};
use chrono::Utc;
use mach_gmail::{config::OAuthConfig, credentials, oauth};

pub async fn login() -> Result<()> {
    let config = OAuthConfig::from_env().context("OAuth client credentials not configured")?;
    // Migrate and claim a historical single-account database before adding a
    // second credential. Opening again afterward covers a first login after
    // credentials were removed while the cached database remained.
    let _ = crate::runtime::open_store().await?;
    let creds = oauth::login(&config).await?;
    credentials::save(&creds)?;
    let _ = crate::runtime::open_store().await?;
    println!("✓ Logged in as {}", creds.email);
    println!("  Access token expires {}", creds.expires_at.to_rfc3339());
    Ok(())
}

pub async fn status(account: Option<&str>) -> Result<()> {
    let selected: Vec<_> = credentials::load_all()?
        .into_iter()
        .filter(|credentials| account.map_or(true, |email| credentials.email == email))
        .collect();
    if selected.is_empty() {
        println!("No matching credentials stored. Run `mach auth login`.");
    }
    for creds in selected {
        println!("Account:        {}", creds.email);
        println!("Token expires:  {}", creds.expires_at.to_rfc3339());
        let now = Utc::now();
        if creds.expires_at <= now {
            println!("Status:         access token expired (will refresh on next API call)");
        } else {
            let mins = (creds.expires_at - now).num_minutes();
            println!("Status:         valid for {mins} more minute(s)");
        }
    }
    Ok(())
}

pub async fn logout(account: Option<&str>, all: bool) -> Result<()> {
    if all {
        credentials::delete_all()?;
        println!("✓ Logged out all accounts");
        return Ok(());
    }
    let accounts = credentials::load_all()?;
    let email = match account {
        Some(email) => email,
        None if accounts.len() == 1 => &accounts[0].email,
        None => anyhow::bail!("multiple accounts are stored; pass --account <email> or --all"),
    };
    credentials::delete(email)?;
    println!("✓ Logged out {email}");
    Ok(())
}
