use chrono::Utc;
use anyhow::{Context, Result};
use mach_gmail::{config::OAuthConfig, credentials, oauth};

pub async fn login() -> Result<()> {
    let config = OAuthConfig::from_env()
        .context("OAuth client credentials not configured")?;
    let creds = oauth::login(&config).await?;
    credentials::save(&creds)?;
    println!("✓ Logged in as {}", creds.email);
    println!("  Access token expires {}", creds.expires_at.to_rfc3339());
    Ok(())
}

pub async fn status() -> Result<()> {
    match credentials::load()? {
        None => {
            println!("No credentials stored. Run `mach auth login`.");
        }
        Some(creds) => {
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
    }
    Ok(())
}

pub async fn logout() -> Result<()> {
    credentials::delete()?;
    println!("✓ Logged out");
    Ok(())
}
