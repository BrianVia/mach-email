//! Domain types, the `Action` enum, and the `Dispatcher`.
//!
//! `mach-core` is the only crate that owns mutation logic. The TUI keymap,
//! CLI subcommands, and MCP tools all produce `Action` values and call
//! `Dispatcher::execute` — never bypass the dispatcher.

pub mod action;
pub mod compose;
pub mod dispatcher;
pub mod error;
pub mod event;
pub mod ids;
pub mod keymap;
pub mod mock;
pub mod search_query;
pub mod state;
pub mod store;
pub mod unsubscribe;
pub mod user_config;

pub use action::{Action, ActionOutcome, DraftPatch, OpId};
pub use dispatcher::Dispatcher;
pub use error::{CoreError, CoreResult};
pub use event::StateEvent;
pub use ids::{DraftId, LabelId, MessageId, ThreadId};
pub use keymap::{Chord, KeyContext, Keymap, KeymapError, Mode};
pub use search_query::SearchQuery;
pub use state::AppState;
pub use store::{
    InlineImageRow, MailRemote, MailStore, MessageHeaders, OutboxOp, OutboxOpKind, OutboxSummary,
};
pub use unsubscribe::{unsubscribe_targets, UnsubscribeTarget};
pub use user_config::UserConfig;
