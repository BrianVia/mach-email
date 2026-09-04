//! TOML keymap loader, merger, and resolver.
//!
//! Shape:
//! ```toml
//! [normal]
//! "j"   = "select_next"
//! "k"   = "select_prev"
//! "e"   = { kind = "archive", thread_ids = "$selection" }
//! "g i" = { kind = "open_label", label_id = "INBOX" }
//! "/"   = { kind = "search", query = "", limit = 50 }
//! ```
//!
//! - Top-level tables are *modes*: `normal`, `reading`, `composing`, `search`,
//!   `filter`. Each key inside is a *chord string* (single key or
//!   space-separated multi-key like `"g i"`).
//! - Values are either a bare action name (string) or a partial `Action`
//!   JSON object using `$macros`:
//!     - `"$selection"`        → current selection IDs (array)
//!     - `"$current_thread"`   → current thread id (string)
//!     - `"$current_message"`  → current message id (string)
//!     - `"$current_draft"`    → current draft id (string)
//! - Macros that resolve to arrays in array-shaped fields and strings in
//!   string-shaped fields. We treat the template substitution as
//!   string-replace on the JSON tree.
//!
//! Merge precedence: user file wins per binding; bindings absent from the
//! user file fall back to the default file.
//!
//! Validation: a malformed binding logs a `tracing::warn` and is dropped
//! (the rest of the keymap still loads).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::action::Action;

/// Application modes. The active mode decides which keymap section is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Normal,
    Reading,
    Composing,
    Search,
    Filter,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Reading => "reading",
            Mode::Composing => "composing",
            Mode::Search => "search",
            Mode::Filter => "filter",
        }
    }
}

/// A single key press, normalized. `"shift+j"`, `"ctrl+c"`, `"enter"`,
/// `"esc"`, `"space"`, `"tab"`, plain `"j"`, etc. Surface adapters convert
/// their native key event types into this string form before lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord(pub String);

impl Chord {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runtime state needed to fill `$macros`.
#[derive(Debug, Clone, Default)]
pub struct KeyContext {
    pub selection: Vec<String>,
    pub current_thread: Option<String>,
    pub current_message: Option<String>,
    pub current_draft: Option<String>,
}

/// The output of consuming key events. The TUI/desktop drive this state
/// machine — each key call returns either:
/// - `Action(_)` — resolved, dispatch it
/// - `Prefix(_)` — keep listening for more keys (show chord overlay)
/// - `Unbound`   — no binding matched; reset to baseline
#[derive(Debug, Clone)]
pub enum Resolution {
    Action(Action),
    /// Presentation-only command interpreted by the TUI/desktop adapter.
    AdapterAction(String),
    Prefix(Vec<ChordContinuation>),
    Unbound,
}

#[derive(Debug, Clone)]
pub struct ChordContinuation {
    /// Next chord segment expected (e.g. "i" after "g").
    pub next: String,
    /// Stable action name the user would land on if they press it.
    pub action_name: String,
}

#[derive(Debug, Error)]
pub enum KeymapError {
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing macro `{0}` in context")]
    MissingMacro(String),
}

/// Stored template — either a string shorthand (`"select_next"`) or a
/// partial JSON Action with placeholders.
#[derive(Debug, Clone)]
enum Template {
    Bare(String),
    Json(serde_json::Value),
}

impl Template {
    /// Substitute `$macros` with values from `ctx`, then deserialize into
    /// an `Action`. Strings beginning with `$` are replaced wholesale:
    /// - `"$selection"` → JSON array of strings
    /// - `"$current_thread"` etc → JSON string
    fn resolve(&self, ctx: &KeyContext) -> Result<Action, KeymapError> {
        match self {
            Template::Bare(name) => {
                // Reconstruct the simplest Action JSON: just the tag.
                let json = serde_json::json!({ "kind": name });
                Ok(serde_json::from_value(json)?)
            }
            Template::Json(value) => {
                let mut v = value.clone();
                substitute(&mut v, ctx)?;
                Ok(serde_json::from_value(v)?)
            }
        }
    }
}

fn substitute(v: &mut serde_json::Value, ctx: &KeyContext) -> Result<(), KeymapError> {
    use serde_json::Value;
    match v {
        Value::String(s) if s.starts_with('$') => match s.as_str() {
            "$selection" => {
                *v = Value::Array(
                    ctx.selection
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                );
            }
            "$current_thread" => {
                let id = ctx
                    .current_thread
                    .clone()
                    .ok_or_else(|| KeymapError::MissingMacro("$current_thread".into()))?;
                *v = Value::String(id);
            }
            "$current_message" => {
                let id = ctx
                    .current_message
                    .clone()
                    .ok_or_else(|| KeymapError::MissingMacro("$current_message".into()))?;
                *v = Value::String(id);
            }
            "$current_draft" => {
                let id = ctx
                    .current_draft
                    .clone()
                    .ok_or_else(|| KeymapError::MissingMacro("$current_draft".into()))?;
                *v = Value::String(id);
            }
            other => return Err(KeymapError::MissingMacro(other.to_string())),
        },
        Value::Object(map) => {
            for (_, child) in map.iter_mut() {
                substitute(child, ctx)?;
            }
        }
        Value::Array(arr) => {
            for child in arr.iter_mut() {
                substitute(child, ctx)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// One mode's bindings. `chord_strings` is the user-typed key sequence
/// (space-separated for multi-key chords).
#[derive(Debug, Clone, Default)]
struct ModeBindings {
    /// chord_string (e.g. "g i") → template
    bindings: HashMap<String, Template>,
    /// Cached set of valid *prefixes* (e.g. "g" if "g i" or "g s" exists).
    /// Used to detect "user pressed `g`, wait for more keys."
    prefixes: HashSet<String>,
}

impl ModeBindings {
    fn rebuild_prefixes(&mut self) {
        self.prefixes.clear();
        for chord in self.bindings.keys() {
            let parts: Vec<&str> = chord.split_whitespace().collect();
            // Every strict prefix is a valid waiting state.
            for i in 1..parts.len() {
                self.prefixes.insert(parts[..i].join(" "));
            }
        }
    }

    fn upsert(&mut self, chord: &str, tpl: Template) {
        self.bindings.insert(chord.to_string(), tpl);
        self.rebuild_prefixes();
    }
}

/// The whole keymap. Built from the default file + optional user override.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    modes: HashMap<Mode, ModeBindings>,
}

impl Keymap {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse a single TOML string. Bad bindings are logged and skipped;
    /// the rest of the keymap still parses.
    pub fn from_toml(s: &str) -> Result<Self, KeymapError> {
        let raw: BTreeMap<String, BTreeMap<String, toml::Value>> = toml::from_str(s)?;
        let mut km = Self::default();
        for (mode_name, bindings) in raw {
            let mode = match mode_name.as_str() {
                "normal" => Mode::Normal,
                "reading" => Mode::Reading,
                "composing" => Mode::Composing,
                "search" => Mode::Search,
                "filter" => Mode::Filter,
                other => {
                    warn!(mode = other, "unknown keymap section; ignoring");
                    continue;
                }
            };
            let entry = km.modes.entry(mode).or_default();
            for (chord, raw_val) in bindings {
                match parse_template(&raw_val) {
                    Ok(tpl) => entry.upsert(&chord, tpl),
                    Err(e) => warn!(
                        mode = mode.as_str(),
                        chord = chord.as_str(),
                        error = %e,
                        "invalid binding; ignoring"
                    ),
                }
            }
        }
        Ok(km)
    }

    /// Overlay `user` on top of `self`. Per binding, user wins.
    pub fn merge(&mut self, user: Keymap) {
        for (mode, ub) in user.modes {
            let entry = self.modes.entry(mode).or_default();
            for (chord, tpl) in ub.bindings {
                entry.bindings.insert(chord, tpl);
            }
            entry.rebuild_prefixes();
        }
    }

    /// `chord_so_far` is the accumulated chord string (e.g. `"g"` after
    /// the user pressed `g`, or `"g i"` after they pressed both).
    pub fn resolve(&self, mode: Mode, chord_so_far: &str, ctx: &KeyContext) -> Resolution {
        let Some(bindings) = self.modes.get(&mode) else {
            return Resolution::Unbound;
        };

        if let Some(tpl) = bindings.bindings.get(chord_so_far) {
            let name = template_action_name(tpl);
            if matches!(
                name.as_str(),
                "inbox_split_important" | "inbox_split_other" | "inbox_split_newsletters"
            ) {
                return Resolution::AdapterAction(name);
            }
            return match tpl.resolve(ctx) {
                Ok(a) => Resolution::Action(a),
                Err(e) => {
                    warn!(error = %e, chord = chord_so_far, "binding failed to resolve");
                    Resolution::Unbound
                }
            };
        }

        if bindings.prefixes.contains(chord_so_far) {
            let mut conts = Vec::new();
            let prefix_with_space = format!("{chord_so_far} ");
            for (full, tpl) in &bindings.bindings {
                if let Some(rest) = full.strip_prefix(&prefix_with_space) {
                    // Use the immediate next segment, not the entire rest, so
                    // 3-key chords surface their middle step in the overlay.
                    let next_segment = rest.split_whitespace().next().unwrap_or("").to_string();
                    let action_name = template_action_name(tpl);
                    conts.push(ChordContinuation {
                        next: next_segment,
                        action_name,
                    });
                }
            }
            // Stable sort for deterministic UI.
            conts.sort_by(|a, b| a.next.cmp(&b.next));
            conts.dedup_by(|a, b| a.next == b.next);
            return Resolution::Prefix(conts);
        }

        Resolution::Unbound
    }

    /// Flatten to a list of `(mode, chord, action_name)` triples for the
    /// chord overlay / help screen. Sorted for stable rendering.
    pub fn bindings_for(&self, mode: Mode) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .modes
            .get(&mode)
            .map(|b| {
                b.bindings
                    .iter()
                    .map(|(c, t)| (c.clone(), template_action_name(t)))
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

fn parse_template(v: &toml::Value) -> Result<Template, KeymapError> {
    match v {
        toml::Value::String(s) => Ok(Template::Bare(s.clone())),
        toml::Value::Table(_) => {
            // Round-trip via JSON so the substitution + Action deserialization
            // share a single representation.
            let json = toml_to_json(v);
            Ok(Template::Json(json))
        }
        other => Err(KeymapError::Json(serde::de::Error::custom(format!(
            "binding must be a string or table, got {other:?}"
        )))),
    }
}

fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    use toml::Value as T;
    match v {
        T::String(s) => J::String(s.clone()),
        T::Integer(i) => J::Number((*i).into()),
        T::Float(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .unwrap_or(J::Null),
        T::Boolean(b) => J::Bool(*b),
        T::Datetime(d) => J::String(d.to_string()),
        T::Array(a) => J::Array(a.iter().map(toml_to_json).collect()),
        T::Table(t) => J::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

fn template_action_name(t: &Template) -> String {
    match t {
        Template::Bare(n) => n.clone(),
        Template::Json(v) => v
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("<?>")
            .to_string(),
    }
}

/// The Superhuman-flavored default keymap, embedded at compile time.
/// Lifted verbatim from
/// `https://assets.mail.superhuman.com/webapp/download/Superhuman%20Keyboard%20Shortcuts.pdf`.
pub const DEFAULT_KEYMAP_TOML: &str = include_str!("../../../config/default-keymap.toml");

impl Keymap {
    /// Build the canonical default keymap. Used if the user has no
    /// `~/.config/mach/keymap.toml`, or as the base before merging user
    /// overrides.
    pub fn default_keymap() -> Self {
        Self::from_toml(DEFAULT_KEYMAP_TOML).expect("baked-in keymap must parse")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{LabelId, ThreadId};

    #[test]
    fn parses_bare_action_shorthand() {
        let km = Keymap::from_toml(
            r#"
            [normal]
            "j" = "select_next"
        "#,
        )
        .unwrap();
        let r = km.resolve(Mode::Normal, "j", &KeyContext::default());
        assert!(matches!(r, Resolution::Action(Action::SelectNext)));
    }

    #[test]
    fn resolves_selection_macro() {
        let km = Keymap::from_toml(
            r#"
            [normal]
            "e" = { kind = "archive", thread_ids = "$selection" }
        "#,
        )
        .unwrap();
        let ctx = KeyContext {
            selection: vec!["t1".into(), "t2".into()],
            ..Default::default()
        };
        let r = km.resolve(Mode::Normal, "e", &ctx);
        match r {
            Resolution::Action(Action::Archive { thread_ids }) => {
                assert_eq!(thread_ids, vec![ThreadId::new("t1"), ThreadId::new("t2")]);
            }
            _ => panic!("expected Archive with 2 ids, got {r:?}"),
        }
    }

    #[test]
    fn resolves_current_message_macro() {
        let km = Keymap::from_toml(
            r#"
            [reading]
            "r" = { kind = "reply", message_id = "$current_message", all = false }
        "#,
        )
        .unwrap();
        let ctx = KeyContext {
            current_message: Some("m1".into()),
            ..Default::default()
        };
        let r = km.resolve(Mode::Reading, "r", &ctx);
        match r {
            Resolution::Action(Action::Reply { message_id, all }) => {
                assert_eq!(message_id.as_str(), "m1");
                assert!(!all);
            }
            _ => panic!("expected Reply"),
        }
    }

    #[test]
    fn chord_prefix_returns_continuation_overlay() {
        let km = Keymap::from_toml(
            r#"
            [normal]
            "g i" = { kind = "open_label", label_id = "INBOX" }
            "g s" = { kind = "open_label", label_id = "STARRED" }
            "g d" = { kind = "open_label", label_id = "DRAFT" }
        "#,
        )
        .unwrap();
        let r = km.resolve(Mode::Normal, "g", &KeyContext::default());
        match r {
            Resolution::Prefix(cs) => {
                let nexts: Vec<&str> = cs.iter().map(|c| c.next.as_str()).collect();
                assert_eq!(nexts, vec!["d", "i", "s"]); // sorted
                assert_eq!(cs[0].action_name, "open_label");
            }
            _ => panic!("expected prefix"),
        }
    }

    #[test]
    fn user_override_wins_over_default_per_binding() {
        let mut km = Keymap::from_toml(
            r#"
            [normal]
            "j" = "select_next"
            "k" = "select_prev"
        "#,
        )
        .unwrap();
        let user = Keymap::from_toml(
            r#"
            [normal]
            "j" = "select_prev"
        "#,
        )
        .unwrap();
        km.merge(user);
        assert!(matches!(
            km.resolve(Mode::Normal, "j", &KeyContext::default()),
            Resolution::Action(Action::SelectPrev)
        ));
        // The non-overridden binding still works.
        assert!(matches!(
            km.resolve(Mode::Normal, "k", &KeyContext::default()),
            Resolution::Action(Action::SelectPrev)
        ));
    }

    #[test]
    fn missing_macro_in_context_returns_unbound_not_panic() {
        let km = Keymap::from_toml(
            r#"
            [reading]
            "r" = { kind = "reply", message_id = "$current_message", all = false }
        "#,
        )
        .unwrap();
        // No current_message in ctx.
        let r = km.resolve(Mode::Reading, "r", &KeyContext::default());
        assert!(
            matches!(r, Resolution::Unbound),
            "expected Unbound, got {r:?}"
        );
    }

    #[test]
    fn unknown_chord_is_unbound() {
        let km = Keymap::from_toml(
            r#"[normal]
            "j" = "select_next"
        "#,
        )
        .unwrap();
        assert!(matches!(
            km.resolve(Mode::Normal, "xyz", &KeyContext::default()),
            Resolution::Unbound
        ));
    }

    #[test]
    fn default_keymap_parses_and_contains_archive() {
        let km = Keymap::default_keymap();
        let ctx = KeyContext {
            selection: vec!["t1".into()],
            ..Default::default()
        };
        let r = km.resolve(Mode::Normal, "e", &ctx);
        assert!(matches!(r, Resolution::Action(Action::Archive { .. })));
    }

    #[test]
    fn resolves_inbox_split_as_adapter_action() {
        let km = Keymap::from_toml(
            r#"
            [normal]
            "1" = "inbox_split_important"
        "#,
        )
        .unwrap();
        assert!(matches!(
            km.resolve(Mode::Normal, "1", &KeyContext::default()),
            Resolution::AdapterAction(name) if name == "inbox_split_important"
        ));
    }

    #[test]
    fn bindings_for_returns_sorted_pairs() {
        let km = Keymap::from_toml(
            r#"
            [normal]
            "j" = "select_next"
            "k" = "select_prev"
            "e" = { kind = "archive", thread_ids = "$selection" }
        "#,
        )
        .unwrap();
        let b = km.bindings_for(Mode::Normal);
        assert_eq!(
            b,
            vec![
                ("e".into(), "archive".into()),
                ("j".into(), "select_next".into()),
                ("k".into(), "select_prev".into()),
            ]
        );
    }

    // The label-id type round-trips through string substitution cleanly.
    #[test]
    fn open_label_resolves_when_label_id_is_string_literal() {
        let km = Keymap::from_toml(
            r#"
            [normal]
            "g i" = { kind = "open_label", label_id = "INBOX" }
        "#,
        )
        .unwrap();
        match km.resolve(Mode::Normal, "g i", &KeyContext::default()) {
            Resolution::Action(Action::OpenLabel { label_id }) => {
                assert_eq!(label_id, LabelId::new("INBOX"));
            }
            r => panic!("expected OpenLabel, got {r:?}"),
        }
    }
}
