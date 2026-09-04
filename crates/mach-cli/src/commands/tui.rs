use anyhow::Result;

use crate::runtime;

pub async fn run(account: Option<&str>) -> Result<()> {
    let store = runtime::open_store().await?;
    let scope = runtime::account_scope(account);
    mach_tui::run_with_user_config(store, scope, runtime::user_config()?).await
}
