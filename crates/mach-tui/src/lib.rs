//! ratatui-based TUI for mach.
//!
//! Vim-flavored. Bindings live in `mach_core::keymap` and ship with
//! Superhuman defaults at `config/default-keymap.toml`. Override per-binding
//! at `~/.config/mach/keymap.toml`.

mod app;
mod backend;
mod config;
mod email_body;
mod keys;
mod render;

pub use app::{run, run_with_user_config};
