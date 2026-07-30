//! Credential storage on local disk.
//!
//! We deliberately avoid the OS keyring: on macOS, `keyring` 3.x silently
//! fails to persist across processes from unsigned CLI binaries (the new
//! data-protection backend needs an Apple-signed app identifier). Using a
//! file at the OS data dir with mode 0600 is what most CLI tools do
//! anyway — gh, doctl, gcloud all live here.

use std::fs;
use std::path::PathBuf;

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
}

impl StoredCredentials {
    pub fn is_access_expired(&self) -> bool {
        self.expires_at <= Utc::now() + chrono::Duration::seconds(30)
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

fn credentials_path() -> Result<PathBuf, CredsError> {
    let dirs = ProjectDirs::from("com", "via", "mach").ok_or(CredsError::NoDataDir)?;
    Ok(dirs.data_dir().join("credentials.json"))
}

pub fn save(creds: &StoredCredentials) -> Result<(), CredsError> {
    let path = credentials_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(creds)?;
    fs::write(&path, &json)?;
    set_mode_0600(&path)?;
    Ok(())
}

pub fn load() -> Result<Option<StoredCredentials>, CredsError> {
    let path = credentials_path()?;
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete() -> Result<(), CredsError> {
    let path = credentials_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
fn set_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}
