use std::collections::HashSet;

use anyhow::Result;
use mach_core::{ids::AccountId, MailStore};

use crate::runtime;

pub async fn list(selected_account: Option<&str>) -> Result<()> {
    let store = runtime::open_store().await?;
    let entries = store
        .list_outbox(&runtime::account_scope(selected_account))
        .await?;
    println!("account\tid\tkind\tattempts\tstate\tlast_error");
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            entry.account_id,
            entry.id,
            entry.kind,
            entry.attempts,
            entry.state,
            entry.last_error.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

pub async fn retry(selected_account: Option<&str>) -> Result<()> {
    let store = runtime::open_store().await?;
    let accounts = if let Some(account) = selected_account {
        HashSet::from([AccountId::new(account)])
    } else {
        store
            .list_outbox(&runtime::account_scope(None))
            .await?
            .into_iter()
            .filter(|entry| entry.state == "failed")
            .map(|entry| entry.account_id)
            .collect()
    };
    let mut retried = 0;
    for account in accounts {
        retried += store.retry_failed_outbox(&account).await?;
    }
    println!("{retried} outbox operation(s) queued for retry");
    Ok(())
}
