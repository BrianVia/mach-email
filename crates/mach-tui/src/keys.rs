//! Normalize crossterm `KeyEvent`s into the chord-string form the keymap
//! expects: plain characters as-is, named keys as lowercase words,
//! modifiers as `mod+key` (alphabetical order: `ctrl+shift+x`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Convert a crossterm key event into the chord string used by `Keymap`.
///
/// Returns `None` for events we don't want to feed the keymap — like
/// the Release variant or modifier-only presses.
pub fn key_event_to_chord(ev: &KeyEvent) -> Option<String> {
    use crossterm::event::KeyEventKind;
    if ev.kind == KeyEventKind::Release {
        return None;
    }

    let mut parts: Vec<&'static str> = Vec::new();
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl");
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt");
    }
    // Shift gets folded into Char(_) by crossterm (J for shift+j), but for
    // letter chars we re-add the explicit `shift+` prefix below so
    // `shift+u` and `u` are distinguishable in the keymap.
    let key = match ev.code {
        KeyCode::Char(c) => {
            // Crossterm sends uppercase for shift+letter. Normalize to
            // lowercase + explicit shift modifier.
            if c.is_ascii_uppercase() {
                parts.push("shift");
                format!("{}", c.to_ascii_lowercase())
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => {
            parts.push("shift");
            "tab".into()
        }
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Insert => "insert".into(),
        // Bare modifier presses, system keys, etc.
        _ => return None,
    };

    if parts.is_empty() {
        Some(key)
    } else {
        Some(format!("{}+{}", parts.join("+"), key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn plain_char_passes_through() {
        assert_eq!(
            key_event_to_chord(&ev(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some("j".into())
        );
    }

    #[test]
    fn uppercase_letter_becomes_shift_plus_lowercase() {
        assert_eq!(
            key_event_to_chord(&ev(KeyCode::Char('J'), KeyModifiers::SHIFT)),
            Some("shift+j".into())
        );
    }

    #[test]
    fn ctrl_combo_is_prefixed() {
        assert_eq!(
            key_event_to_chord(&ev(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some("ctrl+s".into())
        );
    }

    #[test]
    fn named_keys_use_lowercase_words() {
        assert_eq!(
            key_event_to_chord(&ev(KeyCode::Enter, KeyModifiers::NONE)),
            Some("enter".into())
        );
        assert_eq!(
            key_event_to_chord(&ev(KeyCode::Esc, KeyModifiers::NONE)),
            Some("esc".into())
        );
    }

    #[test]
    fn release_events_drop() {
        let mut e = ev(KeyCode::Char('j'), KeyModifiers::NONE);
        e.kind = KeyEventKind::Release;
        assert_eq!(key_event_to_chord(&e), None);
    }
}
