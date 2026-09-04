use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Google OAuth client credentials. Personal builds bring their own GCP
/// project — the unverified-app cap (100 users, 7-day refresh tokens) makes
/// shipping a single shared client ID unsuitable for a personal tool.
///
/// [`OAuthConfig::load`] first checks `MACH_GOOGLE_CLIENT_ID` and
/// `MACH_GOOGLE_CLIENT_SECRET`, then falls back to `google_client.json` in the
/// OS application-data directory. Create the credentials at
/// https://console.cloud.google.com/apis/credentials → "OAuth client ID" →
/// "Desktop app".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthConfigSource {
    Env,
    File,
}

pub fn pubsub_topic() -> Option<String> {
    nonempty_env("MACH_PUBSUB_TOPIC")
}

pub fn pubsub_subscription() -> Option<String> {
    nonempty_env("MACH_PUBSUB_SUBSCRIPTION")
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not resolve OS application-data directory")]
    NoDataDir,
    #[error("could not read or write OAuth client config at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid OAuth client config at {path}: {source}")]
    Serde {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "OAuth client credentials are not configured: set MACH_GOOGLE_CLIENT_ID and \
         MACH_GOOGLE_CLIENT_SECRET, or create {path}; running `mach auth login` once \
         with the env vars set persists it"
    )]
    Missing { path: PathBuf },
}

impl OAuthConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_with_source().map(|(config, _)| config)
    }

    pub fn load_with_source() -> Result<(Self, OAuthConfigSource), ConfigError> {
        if let Some(config) = from_env(|name| env::var(name).ok()) {
            return Ok((config, OAuthConfigSource::Env));
        }
        let path = config_path()?;
        load_file(&path)
    }

    pub fn persist(&self) -> Result<(), ConfigError> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(|source| ConfigError::Serde {
            path: path.clone(),
            source,
        })?;
        fs::write(&path, json).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        crate::credentials::set_mode_0600(&path).map_err(|source| ConfigError::Io { path, source })
    }
}

fn config_path() -> Result<PathBuf, ConfigError> {
    crate::credentials::data_dir()
        .map(|dir| dir.join("google_client.json"))
        .map_err(|_| ConfigError::NoDataDir)
}

#[cfg(test)]
fn load_from(
    path: &Path,
    get_env: impl Fn(&str) -> Option<String>,
) -> Result<(OAuthConfig, OAuthConfigSource), ConfigError> {
    if let Some(config) = from_env(get_env) {
        return Ok((config, OAuthConfigSource::Env));
    }
    load_file(path)
}

fn from_env(get_env: impl Fn(&str) -> Option<String>) -> Option<OAuthConfig> {
    let (Some(client_id), Some(client_secret)) = (
        get_env("MACH_GOOGLE_CLIENT_ID"),
        get_env("MACH_GOOGLE_CLIENT_SECRET"),
    ) else {
        return None;
    };
    Some(OAuthConfig {
        client_id,
        client_secret,
    })
}

fn load_file(path: &Path) -> Result<(OAuthConfig, OAuthConfigSource), ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::Missing {
                path: path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let config = serde_json::from_slice(&bytes).map_err(|source| ConfigError::Serde {
        path: path.to_path_buf(),
        source,
    })?;
    Ok((config, OAuthConfigSource::File))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("mach-gmail-config-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_file_only_config() {
        let dir = TestDir::new();
        let path = dir.0.join("google_client.json");
        fs::write(
            &path,
            br#"{"client_id":"file-id","client_secret":"file-secret"}"#,
        )
        .unwrap();

        let (config, source) = load_from(&path, |_| None).unwrap();

        assert_eq!(
            config,
            OAuthConfig {
                client_id: "file-id".into(),
                client_secret: "file-secret".into(),
            }
        );
        assert_eq!(source, OAuthConfigSource::File);
    }

    #[test]
    fn env_config_takes_precedence_over_file() {
        let dir = TestDir::new();
        let path = dir.0.join("google_client.json");
        fs::write(
            &path,
            br#"{"client_id":"file-id","client_secret":"file-secret"}"#,
        )
        .unwrap();
        let env = HashMap::from([
            ("MACH_GOOGLE_CLIENT_ID", "env-id".to_string()),
            ("MACH_GOOGLE_CLIENT_SECRET", "env-secret".to_string()),
        ]);

        let (config, source) = load_from(&path, |name| env.get(name).cloned()).unwrap();

        assert_eq!(
            config,
            OAuthConfig {
                client_id: "env-id".into(),
                client_secret: "env-secret".into(),
            }
        );
        assert_eq!(source, OAuthConfigSource::Env);
    }
}
