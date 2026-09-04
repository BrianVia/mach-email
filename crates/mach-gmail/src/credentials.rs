//! Credential storage on local disk.
//!
//! We deliberately avoid the OS keyring: on macOS, `keyring` 3.x silently
//! fails to persist across processes from unsigned CLI binaries (the new
//! data-protection backend needs an Apple-signed app identifier). Using a
//! file at the OS data dir with mode 0600 is what most CLI tools do
//! anyway — gh, doctl, gcloud all live here.

use std::fs;
use std::path::PathBuf;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredentials {
    pub email: String,
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub needs_reauth: Option<String>,
}

impl StoredCredentials {
    pub fn is_access_expired(&self) -> bool {
        self.expires_at <= Utc::now() + chrono::Duration::seconds(30)
    }

    pub fn needs_reauth(&self) -> bool {
        self.needs_reauth.is_some()
    }
}

#[derive(Debug, Error)]
pub enum CredsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("could not resolve OS application-data directory")]
    NoDataDir,
}

pub(crate) fn data_dir() -> Result<PathBuf, CredsError> {
    let dirs = ProjectDirs::from("com", "via", "mach").ok_or(CredsError::NoDataDir)?;
    Ok(dirs.data_dir().to_path_buf())
}

fn legacy_credentials_path() -> Result<PathBuf, CredsError> {
    Ok(data_dir()?.join("credentials.json"))
}

fn credentials_path(account: &str) -> Result<PathBuf, CredsError> {
    let filename = format!("{}.json", URL_SAFE_NO_PAD.encode(account.as_bytes()));
    Ok(data_dir()?.join("accounts").join(filename))
}

pub fn save(creds: &StoredCredentials) -> Result<(), CredsError> {
    let path = credentials_path(&creds.email)?;
    save_to(&path, creds)
}

fn save_to(path: &std::path::Path, creds: &StoredCredentials) -> Result<(), CredsError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(creds)?;
    fs::write(path, &json)?;
    set_mode_0600(path)?;
    Ok(())
}

pub fn load(account: &str) -> Result<Option<StoredCredentials>, CredsError> {
    migrate_legacy()?;
    let path = credentials_path(account)?;
    load_from(&path)
}

fn load_from(path: &std::path::Path) -> Result<Option<StoredCredentials>, CredsError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn load_all() -> Result<Vec<StoredCredentials>, CredsError> {
    migrate_legacy()?;
    let dir = data_dir()?.join("accounts");
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut accounts: Vec<StoredCredentials> = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        accounts.push(serde_json::from_slice(&fs::read(path)?)?);
    }
    accounts.sort_by(|left, right| left.email.cmp(&right.email));
    Ok(accounts)
}

pub fn delete(account: &str) -> Result<(), CredsError> {
    migrate_legacy()?;
    let path = credentials_path(account)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn delete_all() -> Result<(), CredsError> {
    migrate_legacy()?;
    let path = data_dir()?.join("accounts");
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn migrate_legacy() -> Result<(), CredsError> {
    let legacy = legacy_credentials_path()?;
    let bytes = match fs::read(&legacy) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let credentials: StoredCredentials = serde_json::from_slice(&bytes)?;
    let destination = credentials_path(&credentials.email)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    if !destination.exists() {
        fs::rename(&legacy, &destination)?;
        set_mode_0600(&destination)?;
    } else {
        fs::remove_file(legacy)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub(crate) fn set_mode_0600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    fn credentials(needs_reauth: Option<&str>) -> StoredCredentials {
        StoredCredentials {
            email: "me@example.com".into(),
            refresh_token: "refresh".into(),
            access_token: "access".into(),
            expires_at: Utc::now(),
            needs_reauth: needs_reauth.map(str::to_string),
        }
    }

    #[test]
    fn reauth_state_save_load_round_trips_and_defaults_for_old_files() {
        let dir = env::temp_dir().join(format!("mach-credentials-{}", uuid::Uuid::new_v4()));
        for expected in [None, Some("invalid_grant")] {
            let path = dir.join(format!("{}.json", expected.unwrap_or("none")));
            save_to(&path, &credentials(expected)).unwrap();
            let loaded = load_from(&path).unwrap().unwrap();
            assert_eq!(loaded.needs_reauth.as_deref(), expected);
        }

        let mut old = serde_json::to_value(credentials(None)).unwrap();
        old.as_object_mut().unwrap().remove("needs_reauth");
        let loaded: StoredCredentials = serde_json::from_value(old).unwrap();
        assert_eq!(loaded.needs_reauth, None);
        fs::remove_dir_all(dir).unwrap();
    }
}
