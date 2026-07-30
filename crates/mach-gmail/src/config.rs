use std::env;

use thiserror::Error;

/// Google OAuth client credentials. Personal builds bring their own GCP
/// project — the unverified-app cap (100 users, 7-day refresh tokens) makes
/// shipping a single shared client ID unsuitable for a personal tool.
///
/// Set `MACH_GOOGLE_CLIENT_ID` and `MACH_GOOGLE_CLIENT_SECRET` in your shell.
/// Create the credentials at https://console.cloud.google.com/apis/credentials
/// → "OAuth client ID" → "Desktop app".
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing env var {0} — see `mach auth login --help` for setup steps")]
    Missing(&'static str),
}

impl OAuthConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let client_id = env::var("MACH_GOOGLE_CLIENT_ID")
            .map_err(|_| ConfigError::Missing("MACH_GOOGLE_CLIENT_ID"))?;
        let client_secret = env::var("MACH_GOOGLE_CLIENT_SECRET")
            .map_err(|_| ConfigError::Missing("MACH_GOOGLE_CLIENT_SECRET"))?;
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}
