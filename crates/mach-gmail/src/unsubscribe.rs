use anyhow::{bail, Context, Result};

pub async fn one_click_unsubscribe(url: &str) -> Result<()> {
    if url::Url::parse(url)
        .ok()
        .map(|url| url.scheme().to_string())
        .as_deref()
        != Some("https")
    {
        bail!("one-click unsubscribe requires an HTTPS URL");
    }
    reqwest::Client::new()
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .context("sending one-click unsubscribe")?
        .error_for_status()
        .context("one-click unsubscribe failed")?;
    Ok(())
}
