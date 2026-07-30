//! Safe, width-aware conversion of stored email bodies into terminal-native text.

use std::{cell::RefCell, collections::HashMap};

use html2text::render::text_renderer::RichAnnotation;
use mach_core::store::Message;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const ACCENT: Color = Color::Rgb(0x7c, 0x9c, 0xff);
const DIM: Color = Color::Rgb(0x88, 0x88, 0x88);
const MAX_INPUT_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_LINES: usize = 4_000;

/// Owns the potentially expensive HTML-to-terminal conversion for a thread.
///
/// A message is parsed at most once for a given terminal width. The renderer
/// contains no network-capable HTML engine: images remain textual markers and
/// links are styled visible text, never terminal escape sequences.
#[derive(Default)]
pub struct EmailBodyRenderer {
    cache: RefCell<HashMap<String, CachedBody>>,
}

struct CachedBody {
    width: u16,
    lines: Vec<Line<'static>>,
}

impl EmailBodyRenderer {
    pub fn lines(&self, message: &Message, width: u16) -> Vec<Line<'static>> {
        let key = message.id.to_string();
        if let Some(cached) = self.cache.borrow().get(&key) {
            if cached.width == width {
                return cached.lines.clone();
            }
        }

        let lines = render_message(message, width);
        self.cache.borrow_mut().insert(
            key,
            CachedBody {
                width,
                lines: lines.clone(),
            },
        );
        lines
    }
}

fn render_message(message: &Message, width: u16) -> Vec<Line<'static>> {
    if let Some(html) = message
        .body_html
        .as_deref()
        .filter(|body| !body.trim().is_empty())
    {
        if let Some(lines) = render_html(html, width) {
            return lines;
        }
    }

    let plain = message
        .body_plain
        .as_deref()
        .filter(|body| !body.trim().is_empty())
        .unwrap_or("(body not fetched — open online to backfill)");
    render_plain(plain)
}

fn render_html(html: &str, width: u16) -> Option<Vec<Line<'static>>> {
    let (input, input_truncated) = truncate_utf8(html, MAX_INPUT_BYTES);
    let rendered = html2text::config::rich()
        .lines_from_read(input.as_bytes(), usize::from(width.max(20)))
        .ok()?;
    let output_truncated = rendered.len() > MAX_OUTPUT_LINES;
    let mut lines: Vec<_> = rendered
        .into_iter()
        .take(MAX_OUTPUT_LINES)
        .map(|line| {
            let spans = line
                .tagged_strings()
                .filter_map(|text| {
                    let safe = strip_terminal_controls(&text.s);
                    (!safe.is_empty()).then(|| Span::styled(safe, annotation_style(&text.tag)))
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect();
    if input_truncated || output_truncated {
        lines.push(Line::styled(
            "… message truncated for terminal safety …",
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        ));
    }
    Some(lines)
}

fn render_plain(plain: &str) -> Vec<Line<'static>> {
    let (input, input_truncated) = truncate_utf8(plain, MAX_INPUT_BYTES);
    let mut lines: Vec<_> = input
        .lines()
        .take(MAX_OUTPUT_LINES)
        .map(|line| Line::raw(strip_terminal_controls(line)))
        .collect();
    if input_truncated || input.lines().count() > MAX_OUTPUT_LINES {
        lines.push(Line::styled(
            "… message truncated for terminal safety …",
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        ));
    }
    lines
}

fn annotation_style(annotations: &[RichAnnotation]) -> Style {
    let mut style = Style::default();
    for annotation in annotations {
        style = match annotation {
            RichAnnotation::Link(url) if safe_link(url) => {
                style.fg(ACCENT).add_modifier(Modifier::UNDERLINED)
            }
            RichAnnotation::Image(_) => style.fg(DIM).add_modifier(Modifier::ITALIC),
            RichAnnotation::Emphasis => style.add_modifier(Modifier::ITALIC),
            RichAnnotation::Strong => style.add_modifier(Modifier::BOLD),
            RichAnnotation::Strikeout => style.add_modifier(Modifier::CROSSED_OUT),
            RichAnnotation::Code | RichAnnotation::Preformat(_) => style.fg(Color::Yellow),
            _ => style,
        };
    }
    style
}

fn safe_link(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
}

fn strip_terminal_controls(input: &str) -> String {
    let mut safe = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            // A literal tab has terminal-dependent width while ratatui treats
            // it as zero-width, desynchronizing all subsequent cursor writes.
            '\t' => safe.push_str("    "),
            character if !character.is_control() => safe.push(character),
            _ => {}
        }
    }
    safe
}

fn truncate_utf8(input: &str, max_bytes: usize) -> (&str, bool) {
    if input.len() <= max_bytes {
        return (input, false);
    }
    let mut end = max_bytes;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    (&input[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mach_core::ids::{AccountId, MessageId, ThreadId};

    fn message(html: Option<&str>, plain: Option<&str>) -> Message {
        Message {
            account_id: AccountId::new("test@example.com"),
            id: MessageId::new("message"),
            thread_id: ThreadId::new("thread"),
            from: "sender@example.com".into(),
            to: vec![],
            cc: vec![],
            subject: "Subject".into(),
            snippet: String::new(),
            internal_date: Utc::now(),
            body_plain: plain.map(str::to_owned),
            body_html: html.map(str::to_owned),
            label_ids: vec![],
            fetched_full: true,
            inline_images: vec![],
        }
    }

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn prefers_html_and_preserves_semantic_formatting() {
        let lines = render_message(
            &message(
                Some("<h1>Hello</h1><p><strong>bold</strong> <em>soft</em></p>"),
                Some("wrong fallback"),
            ),
            80,
        );
        let output = text(&lines);
        assert!(output.contains("Hello"));
        assert!(output.contains("bold"));
        assert!(!output.contains("wrong fallback"));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.style.add_modifier.contains(Modifier::BOLD)
                || span.style.add_modifier.contains(Modifier::ITALIC)
        }));
    }

    #[test]
    fn strips_terminal_escape_and_control_characters() {
        let lines = render_message(
            &message(Some("<p>safe\u{1b}]8;;evil\u{7}text\taligned</p>"), None),
            80,
        );
        let output = text(&lines);
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\u{7}'));
        assert!(!output.contains('\t'));
        assert!(output.contains("safe"));
        assert_eq!(strip_terminal_controls("left\tright"), "left    right");
    }

    #[test]
    fn renders_malformed_html_without_panicking() {
        let lines = render_message(&message(Some("<table><tr><td>invoice"), None), 24);
        assert!(text(&lines).contains("invoice"));
    }

    #[test]
    fn wraps_html_to_the_requested_width() {
        let lines = render_message(
            &message(
                Some("<p>This is a deliberately long sentence that must wrap.</p>"),
                None,
            ),
            20,
        );
        assert!(lines.len() > 1);
    }
}
