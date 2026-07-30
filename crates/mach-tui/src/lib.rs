//! ratatui-based TUI for mach.
//!
//! Vim-flavored. Bindings live in `mach_core::keymap` and ship with
//! Superhuman defaults at `config/default-keymap.toml`. Override per-binding
//! at `~/.config/mach/keymap.toml`.

mod app;
mod config;
mod keys;
mod render;

pub use app::run;
