use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub signatures: BTreeMap<String, String>,
}

#[derive(Debug, Error)]
pub enum UserConfigError {
    #[error("reading user config at {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("parsing user config at {path}: {source}")]
    Toml {
        path: String,
        source: toml::de::Error,
    },
}

impl UserConfig {
    pub fn load(path: &Path) -> Result<Self, UserConfigError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(source) => {
                return Err(UserConfigError::Io {
                    path: path.display().to_string(),
                    source,
                })
            }
        };
        toml::from_str(&raw).map_err(|source| UserConfigError::Toml {
            path: path.display().to_string(),
            source,
        })
    }

    pub fn signature_for(&self, account: &str) -> Option<&str> {
        self.signatures
            .get(account)
            .or_else(|| self.signatures.get("default"))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_signatures_and_falls_back_to_default() {
        let path = std::env::temp_dir().join(format!("mach-config-{}.toml", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "[signatures]\ndefault = \"—\\nBrian\"\n\"me@work.com\" = \"Brian Via\\nWork\"\n",
        )
        .unwrap();

        let config = UserConfig::load(&path).unwrap();
        std::fs::remove_file(path).unwrap();

        assert_eq!(config.signature_for("me@work.com"), Some("Brian Via\nWork"));
        assert_eq!(config.signature_for("x@example.com"), Some("—\nBrian"));
    }

    #[test]
    fn missing_file_yields_default() {
        let path = std::env::temp_dir().join(format!("mach-missing-{}.toml", uuid::Uuid::new_v4()));
        let config = UserConfig::load(&path).unwrap();
        assert!(config.signatures.is_empty());
    }
}
