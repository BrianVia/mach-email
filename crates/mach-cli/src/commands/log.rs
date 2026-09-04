use anyhow::{bail, Context, Result};
use chrono::{Duration, Local, Utc};
use mach_core::{Action, MailStore};

use crate::runtime;

pub async fn list(since: &str, limit: u32, account: Option<&str>) -> Result<()> {
    let since_ms = Utc::now()
        .checked_sub_signed(parse_duration(since)?)
        .context("activity time range is outside the supported date range")?
        .timestamp_millis();
    let store = runtime::open_store().await?;
    let entries = store
        .list_activity(&runtime::account_scope(account), since_ms, limit)
        .await?;
    println!("time\taccount\tsummary\tstate");
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}{}",
            entry.at.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
            entry.account_id,
            entry.summary,
            entry.state,
            if entry.undone { " (undone)" } else { "" }
        );
    }
    Ok(())
}

pub async fn undo(id: i64, account: Option<&str>) -> Result<()> {
    let store = runtime::open_store().await?;
    let dispatcher = mach_core::Dispatcher::with_scope(store, runtime::account_scope(account));
    let outcome = dispatcher
        .execute(Action::UndoActivity { outbox_id: id })
        .await?;
    println!("{}", outcome.message);
    Ok(())
}

fn parse_duration(value: &str) -> Result<Duration> {
    let split = value.len().saturating_sub(1);
    let (amount, unit) = value.split_at(split);
    let amount: i64 = amount
        .parse()
        .with_context(|| format!("invalid duration: {value}"))?;
    if amount < 0 {
        bail!("invalid duration: {value}");
    }
    match unit {
        "m" => Ok(Duration::minutes(amount)),
        "h" => Ok(Duration::hours(amount)),
        "d" => Ok(Duration::days(amount)),
        _ => bail!("invalid duration: {value} (use m, h, or d)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_since_duration() {
        assert_eq!(parse_duration("24h").unwrap(), Duration::hours(24));
        assert!(parse_duration("soon").is_err());
    }
}
