use anyhow::Result;
use chrono::Utc;
use mach_core::ids::AccountId;

use crate::runtime;

pub async fn run(selected_account: Option<&str>) -> Result<()> {
    let store = runtime::open_store().await?;
    let config = runtime::user_config()?;
    let push = mach_gmail::config::pubsub_topic().is_some();
    let accounts = mach_gmail::credentials::load_all()?
        .into_iter()
        .filter(|credentials| selected_account.map_or(true, |account| credentials.email == account))
        .collect::<Vec<_>>();

    print!("email\tnickname\tunread\tneeds_reauth\tlast_incremental_sync");
    if push {
        print!("\twatch");
    }
    println!();
    for credentials in accounts {
        let account = AccountId::new(credentials.email.clone());
        let overview = store.account_overview(&account).await?;
        let nickname = config.account_label(account.as_str());
        print!(
            "{}\t{}\t{}\t{}\t{}",
            account,
            if nickname == account.as_str() {
                "-"
            } else {
                nickname
            },
            overview.unread,
            credentials.needs_reauth(),
            overview
                .last_incremental_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "-".into()),
        );
        if push {
            print!("\t{}", watch_status(overview.watch_expiration));
        }
        println!();
    }
    Ok(())
}

pub fn watch_status(expiration: Option<i64>) -> String {
    match expiration {
        None => "not registered".into(),
        Some(value) if mach_gmail::should_renew(Some(value), Utc::now().timestamp_millis()) => {
            format!("renewal due ({value})")
        }
        Some(value) => format!("active ({value})"),
    }
}
