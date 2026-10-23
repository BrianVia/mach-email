//! Cross-process advisory lock so two frontends (desktop, native app, CLI,
//! TUI) never sync or drain the same account's outbox at the same time.
//!
//! One lock file per account, not one global file: flock conflicts between
//! separate opens even inside one process, so a single file would make the
//! desktop's parallel per-account ticks skip each other.

use std::fs::{File, TryLockError};
use std::path::Path;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use mach_core::ids::AccountId;
use tracing::{debug, warn};

/// Held for one sync tick or outbox drain; dropping it releases the lock.
pub struct SyncLock {
    _file: Option<File>,
}

/// `None` means another process (or another tick in this one) holds the
/// account's lock and the caller should skip this round.
pub fn try_sync_lock(account: &AccountId) -> Option<SyncLock> {
    let Ok(dir) = crate::credentials::data_dir() else {
        warn!(%account, "no data dir for the sync lock; syncing unlocked");
        return Some(SyncLock { _file: None });
    };
    let name = format!("sync-{}.lock", URL_SAFE_NO_PAD.encode(account.as_str()));
    let lock = try_lock_at(&dir.join(name));
    if lock.is_none() {
        debug!(%account, "sync lock held elsewhere; skipping");
    }
    lock
}

fn try_lock_at(path: &Path) -> Option<SyncLock> {
    let file = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(path)
        });
    let file = match file {
        Ok(file) => file,
        Err(error) => {
            // Failing closed would stop sync entirely on a broken data dir.
            warn!(path = %path.display(), %error, "opening sync lock failed; syncing unlocked");
            return Some(SyncLock { _file: None });
        }
    };
    match file.try_lock() {
        Ok(()) => Some(SyncLock { _file: Some(file) }),
        Err(TryLockError::WouldBlock) => None,
        Err(TryLockError::Error(error)) => {
            warn!(path = %path.display(), %error, "locking sync lock failed; syncing unlocked");
            Some(SyncLock { _file: None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_fails_while_first_is_held() {
        let path = std::env::temp_dir().join(format!("mach-sync-lock-{}.lock", std::process::id()));
        let first = try_lock_at(&path).expect("first lock");
        assert!(first._file.is_some(), "first lock holds a real flock");
        assert!(try_lock_at(&path).is_none(), "second lock must fail while first is held");
        drop(first);
        assert!(try_lock_at(&path).is_some(), "lock is free again after release");
        let _ = std::fs::remove_file(path);
    }
}
