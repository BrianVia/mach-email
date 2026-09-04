use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub accounts: BTreeMap<String, String>,
    #[serde(default)]
    pub signatures: BTreeMap<String, String>,
    #[serde(default)]
    pub snippets: BTreeMap<String, String>,
}

pub fn expand_snippet(
    text: &str,
    cursor: usize,
    snippets: &BTreeMap<String, String>,
) -> Option<(String, usize)> {
    let before = text.get(..cursor)?;
    let start = before
        .char_indices()
        .rev()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let name = before.get(start..)?.strip_prefix(';')?;
    let replacement = snippets.get(name)?;
    let mut expanded = String::with_capacity(text.len() - (cursor - start) + replacement.len());
    expanded.push_str(&text[..start]);
    expanded.push_str(replacement);
    expanded.push_str(&text[cursor..]);
    Some((expanded, start + replacement.len()))
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

    pub fn account_label<'a>(&'a self, email: &'a str) -> &'a str {
        self.accounts.get(email).map_or(email, String::as_str)
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
            "[accounts]\n\"me@work.com\" = \"Work\"\n\n[signatures]\ndefault = \"—\\nBrian\"\n\"me@work.com\" = \"Brian Via\\nWork\"\n\n[snippets]\nthanks = \"Thanks so much,\\nBrian\"\n",
        )
        .unwrap();

        let config = UserConfig::load(&path).unwrap();
        std::fs::remove_file(path).unwrap();

        assert_eq!(config.signature_for("me@work.com"), Some("Brian Via\nWork"));
        assert_eq!(config.signature_for("x@example.com"), Some("—\nBrian"));
        assert_eq!(config.snippets["thanks"], "Thanks so much,\nBrian");
        assert_eq!(config.account_label("me@work.com"), "Work");
        assert_eq!(config.account_label("x@example.com"), "x@example.com");
    }

    #[test]
    fn missing_file_yields_default() {
        let path = std::env::temp_dir().join(format!("mach-missing-{}.toml", uuid::Uuid::new_v4()));
        let config = UserConfig::load(&path).unwrap();
        assert!(config.accounts.is_empty());
        assert!(config.signatures.is_empty());
        assert!(config.snippets.is_empty());
    }

    #[test]
    fn expands_only_the_matching_token_at_cursor() {
        let snippets = [("thanks".into(), "Thanks so much,\nBrian".into())].into();
        let text = ";thanks earlier\nReply: ;thanks later";
        let cursor = text.find(" later").unwrap();

        assert_eq!(
            expand_snippet(text, cursor, &snippets),
            Some((
                ";thanks earlier\nReply: Thanks so much,\nBrian later".into(),
                text.find(";thanks later").unwrap() + "Thanks so much,\nBrian".len()
            ))
        );
        assert_eq!(expand_snippet(";missing", 8, &snippets), None);
    }
}
