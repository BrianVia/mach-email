use anyhow::Result;

use crate::runtime;

pub async fn run() -> Result<()> {
    let store = runtime::open_store()?;
    mach_tui::run(store).await
}
