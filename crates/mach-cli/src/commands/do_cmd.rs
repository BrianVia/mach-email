use std::io::Read;

use anyhow::{Context, Result};
use mach_core::Action;
use tracing::warn;

use crate::runtime;

/// Parse a single Action as JSON and dispatch it. The outcome is emitted as
/// pretty JSON on stdout — designed for shell pipes and LLM agents calling
/// `mach do` as a tool. Errors go to stderr and the process exits non-zero.
///
/// Special case: `open_thread` triggers a body backfill if the thread's
/// messages haven't been fetched at `format=full` yet. The first call may
/// take a beat (one round-trip to Gmail); subsequent calls are instant from
/// the local cache.
pub async fn run(action_json: &str) -> Result<()> {
    let raw = if action_json == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading action JSON from stdin")?;
        buf
    } else {
        action_json.to_string()
    };

    let action: Action =
        serde_json::from_str(&raw).with_context(|| format!("parsing action JSON: {raw}"))?;

    let store = runtime::open_store()?;
    let dispatcher = mach_core::Dispatcher::new(store.clone());

    // If this is an `open_thread`, do a body backfill first (silent no-op
    // when bodies are already cached). We only spin up a Gmail client if
    // env credentials are available — keeps offline `mach do` working when
    // every thread the user touches is already cached.
    if let Action::OpenThread { id } = &action {
        if let Err(e) = maybe_backfill_bodies(id.clone(), store.clone()).await {
            // Don't fail the whole action — fall through with whatever
            // cached state we have. The CLI user gets the cached view + a
            // warning on stderr.
            warn!(error = %e, "body backfill failed; serving cached state");
            eprintln!("warning: body backfill failed: {e}");
        }
    }

    let outcome = dispatcher
        .execute(action)
        .await
        .context("dispatcher rejected action")?;

    let json = serde_json::to_string_pretty(&outcome)?;
    println!("{json}");
    Ok(())
}

async fn maybe_backfill_bodies(
    thread_id: mach_core::ids::ThreadId,
    store: std::sync::Arc<mach_store::SqliteStore>,
) -> Result<()> {
    use mach_gmail::{config::OAuthConfig, BodyFetcher};
    // Preserve the intentional no-config offline mode while still surfacing
    // corrupt or unreadable persisted credentials to the caller.
    if OAuthConfig::from_env().is_err() {
        return Ok(());
    }
    let fetcher = BodyFetcher::from_stored_credentials(store)?;
    fetcher.fetch_if_needed(&thread_id).await?;
    Ok(())
}
