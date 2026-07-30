use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use oauth2::{
    basic::BasicClient, reqwest::async_http_client, AuthUrl, AuthorizationCode, ClientId,
    ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl, RefreshToken, Scope, TokenResponse,
    TokenUrl,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, warn};
use url::Url;

use crate::config::OAuthConfig;
use crate::credentials::StoredCredentials;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const PROFILE_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/profile";

/// Scopes requested at consent time. Narrower than `gmail.full` to keep the
/// app-verification bar low; broader than `gmail.readonly` so we can write.
/// `openid email` only exists so we can identify the account post-consent.
const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.modify",
    "https://www.googleapis.com/auth/gmail.send",
    "https://www.googleapis.com/auth/gmail.compose",
    "openid",
    "email",
];

/// Run the installed-app PKCE OAuth flow end-to-end and return a bundle ready
/// to persist via [`crate::credentials::save`].
///
/// Flow: bind a loopback listener on a random port → open the browser at
/// Google's consent screen with that port as the redirect → wait for the
/// browser to GET `/?code=...&state=...` → exchange the code → fetch the
/// account email so we can show the user what they linked.
pub async fn login(config: &OAuthConfig) -> Result<StoredCredentials> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind loopback listener for OAuth redirect")?;
    let port = listener.local_addr()?.port();
    debug!(port, "loopback listener bound");

    let client = BasicClient::new(
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
        AuthUrl::new(AUTH_URL.into())?,
        Some(TokenUrl::new(TOKEN_URL.into())?),
    )
    .set_redirect_uri(RedirectUrl::new(format!("http://127.0.0.1:{port}"))?);

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let mut req = client.authorize_url(CsrfToken::new_random);
    for scope in SCOPES {
        req = req.add_scope(Scope::new((*scope).into()));
    }
    let (auth_url, csrf_token) = req
        // `offline` is what makes Google issue a refresh_token; without it
        // we'd silently get only a 1-hour access token and have to re-consent.
        .add_extra_param("access_type", "offline")
        // `consent` forces the consent screen even if previously approved —
        // necessary because Google only re-issues a refresh_token on the
        // first consent. Without this, re-runs return no refresh_token.
        .add_extra_param("prompt", "consent")
        .set_pkce_challenge(pkce_challenge)
        .url();

    println!("Opening browser to authorize mach…");
    println!("  {auth_url}");
    println!("  (if it doesn't open, paste the URL above)");
    let _ = webbrowser::open(auth_url.as_str());

    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(180), listener.accept())
        .await
        .map_err(|_| anyhow!("OAuth flow timed out after 3 minutes"))??;

    let mut buf = vec![0u8; 8192];
    let n = socket.read(&mut buf).await?;
    let request =
        std::str::from_utf8(&buf[..n]).map_err(|_| anyhow!("non-UTF-8 OAuth redirect request"))?;

    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty redirect request"))?;
    // GET /?code=...&state=... HTTP/1.1
    let path = first_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed redirect request line: {first_line:?}"))?;
    let url = Url::parse(&format!("http://127.0.0.1:{port}{path}"))?;

    let mut code = None;
    let mut state = None;
    let mut provider_error = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => provider_error = Some(v.into_owned()),
            _ => {}
        }
    }

    let success_body = "<!doctype html><meta charset=utf-8><title>mach</title>\
         <body style='font:14px -apple-system,system-ui,sans-serif;padding:2rem;color:#111;'>\
         <h1 style='margin:0 0 .5rem;'>You're in.</h1>\
         <p>Account linked. You can close this tab.</p></body>";
    let failure_body = "<!doctype html><meta charset=utf-8><title>mach</title>\
         <body style='font:14px -apple-system,system-ui,sans-serif;padding:2rem;color:#a00;'>\
         <h1 style='margin:0 0 .5rem;'>Login failed.</h1>\
         <p>Check the terminal for details.</p></body>";
    let body = if code.is_some() && state.is_some() && provider_error.is_none() {
        success_body
    } else {
        failure_body
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;

    if let Some(err) = provider_error {
        bail!("OAuth provider returned error: {err}");
    }
    let code = code.ok_or_else(|| anyhow!("redirect missing `code`"))?;
    let returned_state = state.ok_or_else(|| anyhow!("redirect missing `state`"))?;
    if returned_state != *csrf_token.secret() {
        bail!("CSRF state mismatch — possible session fixation attempt; aborting");
    }

    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(async_http_client)
        .await
        .map_err(|e| anyhow!("token exchange failed: {e}"))?;

    let refresh_token = token
        .refresh_token()
        .ok_or_else(|| {
            anyhow!(
                "Google did not return a refresh_token. \
                 Visit https://myaccount.google.com/permissions, revoke mach, and re-run login."
            )
        })?
        .secret()
        .clone();

    let access_token = token.access_token().secret().clone();
    let expires_at = expires_at_from(token.expires_in());

    let email = match fetch_email(&access_token).await {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "could not fetch account email; storing as 'unknown'");
            "unknown".into()
        }
    };

    Ok(StoredCredentials {
        email,
        refresh_token,
        access_token,
        expires_at,
    })
}

/// Trade a refresh token for a fresh access token. Called from the Gmail HTTP
/// client when it gets a 401 (or proactively before `expires_at`).
pub async fn refresh_access_token(
    config: &OAuthConfig,
    refresh_token: &str,
) -> Result<(String, DateTime<Utc>)> {
    let client = BasicClient::new(
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
        AuthUrl::new(AUTH_URL.into())?,
        Some(TokenUrl::new(TOKEN_URL.into())?),
    );

    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.into()))
        .request_async(async_http_client)
        .await
        .map_err(|e| anyhow!("refresh failed: {e}"))?;

    let access_token = token.access_token().secret().clone();
    let expires_at = expires_at_from(token.expires_in());
    Ok((access_token, expires_at))
}

async fn fetch_email(access_token: &str) -> Result<String> {
    let resp = reqwest::Client::new()
        .get(PROFILE_URL)
        .bearer_auth(access_token)
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;
    resp.get("emailAddress")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("getProfile response missing emailAddress"))
}

fn expires_at_from(d: Option<Duration>) -> DateTime<Utc> {
    let secs = d.map(|d| d.as_secs() as i64).unwrap_or(3600);
    Utc::now() + chrono::Duration::seconds(secs)
}
