//! Loads the keymap: defaults + optional user override at
//! `~/.config/mach/keymap.toml`. Errors at load time are *non-fatal* —
//! we log and fall back to the shipped defaults so the TUI always boots.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use mach_core::Keymap;
use tracing::{info, warn};

/// Try to read `~/.config/mach/keymap.toml` (or platform equivalent).
/// Returns `None` if the file doesn't exist.
pub fn user_keymap_path() -> Option<PathBuf> {
    let dirs = ProjectDirs::from("com", "via", "mach")?;
    Some(dirs.config_dir().join("keymap.toml"))
}

/// Build the active keymap: defaults merged with user overrides if present.
pub fn load_keymap() -> Result<Keymap> {
    let mut km = Keymap::default_keymap();
    if let Some(path) = user_keymap_path() {
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("reading user keymap at {}", path.display()))?;
            match Keymap::from_toml(&raw) {
                Ok(user) => {
                    info!(path = %path.display(), "merging user keymap");
                    km.merge(user);
                }
                Err(e) => {
                    warn!(error = %e, path = %path.display(),
                          "user keymap failed to parse; using defaults");
                }
            }
        }
    }
    Ok(km)
}
