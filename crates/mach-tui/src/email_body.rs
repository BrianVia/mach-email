//! Safe, width-aware conversion of stored email bodies into terminal text.
//!
//! Links (including images, shown as `🖼` link text) are styled visible text
//! with numbered references, and are tagged with an underline-color id that
//! [`crate::backend::HyperlinkBackend`] turns into OSC 8 hyperlinks for
//! terminals that support clickable links.

use std::{cell::RefCell, collections::HashMap};

use ego_tree::NodeRef;
use html2text::render::RichAnnotation;
use mach_core::store::Message;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use scraper::{Html, Node};

const ACCENT: Color = Color::Rgb(0x7c, 0x9c, 0xff);
const DIM: Color = Color::Rgb(0x88, 0x88, 0x88);
const MAX_INPUT_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_LINES: usize = 4_000;
const MAX_HYPERLINKS: usize = 255;

/// Owns the potentially expensive HTML-to-terminal conversion for a thread.
///
/// A message is parsed at most once for a given terminal width. The renderer
/// also owns the thread's hyperlink registry: every link gets a stable id
/// (encoded as an underline color) that the terminal backend maps back to the
/// URL for OSC 8 clickability.
#[derive(Default)]
pub struct EmailBodyRenderer {
    cache: RefCell<HashMap<String, CachedBody>>,
    hyperlinks: RefCell<Vec<String>>,
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

        let lines = self.render_message(message, width);
        self.cache.borrow_mut().insert(
            key,
            CachedBody {
                width,
                lines: lines.clone(),
            },
        );
        lines
    }

    /// URLs in id order (id = index + 1) for the backend's OSC 8 registry.
    pub fn hyperlinks(&self) -> Vec<String> {
        self.hyperlinks.borrow().clone()
    }

    fn hyperlink_id(&self, url: &str) -> Option<u8> {
        let mut registry = self.hyperlinks.borrow_mut();
        let id = registry
            .iter()
            .position(|known| known == url)
            .map(|position| position + 1)
            .or_else(|| {
                (registry.len() < MAX_HYPERLINKS).then(|| {
                    registry.push(url.to_string());
                    registry.len()
                })
            })?;
        u8::try_from(id).ok()
    }

    fn render_message(&self, message: &Message, width: u16) -> Vec<Line<'static>> {
        if let Some(html) = message
            .body_html
            .as_deref()
            .filter(|body| !body.trim().is_empty())
        {
            if let Some(lines) = self.render_html(html, width) {
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

    fn render_html(&self, html: &str, width: u16) -> Option<Vec<Line<'static>>> {
        let (input, input_truncated) = truncate_utf8(html, MAX_INPUT_BYTES);
        let normalized = normalize_html(input);
        let rendered = html2text::config::rich()
            .lines_from_read(normalized.as_bytes(), usize::from(width.max(20)))
            .ok()?;
        let output_truncated = rendered.len() > MAX_OUTPUT_LINES;
        let mut links = Vec::new();
        let mut lines: Vec<_> = rendered
            .into_iter()
            .take(MAX_OUTPUT_LINES)
            .map(|line| {
                let mut spans = Vec::new();
                for text in line.tagged_strings() {
                    let safe = strip_terminal_controls(&text.s);
                    if safe.is_empty() {
                        continue;
                    }
                    let mut style = annotation_style(&text.tag);
                    if let Some(url) = safe_link_target(&text.tag) {
                        if let Some(id) = self.hyperlink_id(&url) {
                            style = style.underline_color(Color::Rgb(0, 0, id));
                        }
                        let number = links.iter().position(|known| known == &url).map(|i| i + 1);
                        let first_reference = number.is_none() && links.len() < 50;
                        let number = number.or_else(|| {
                            first_reference.then(|| {
                                links.push(url);
                                links.len()
                            })
                        });
                        if first_reference {
                            spans.push(Span::styled(safe, style));
                            spans.push(Span::styled(
                                format!("[{}]", number.expect("new link has an index")),
                                Style::default().fg(DIM),
                            ));
                            continue;
                        }
                    }
                    spans.push(Span::styled(safe, style));
                }
                Line::from(spans)
            })
            .collect();
        if !links.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "Links",
                Style::default().fg(DIM).add_modifier(Modifier::BOLD),
            ));
            lines.extend(links.into_iter().enumerate().map(|(index, url)| {
                let mut url_style = Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::UNDERLINED);
                if let Some(id) = self.hyperlink_id(&url) {
                    url_style = url_style.underline_color(Color::Rgb(0, 0, id));
                }
                Line::from(vec![
                    Span::styled(format!("[{}] ", index + 1), Style::default().fg(DIM)),
                    Span::styled(url, url_style),
                ])
            }));
        }
        if input_truncated || output_truncated {
            lines.push(Line::styled(
                "… message truncated for terminal safety …",
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            ));
        }
        Some(collapse_blank_lines(lines))
    }
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
    collapse_blank_lines(lines)
}

/// Rewrite marketing HTML into a shape `html2text` renders cleanly: layout
/// tables become flowing text, noise (styles, hidden nodes, spacer images) is
/// dropped, and content images become `🖼` links to their source URL. Data
/// tables (headers/borders) are kept so receipts stay column-aligned.
fn normalize_html(html: &str) -> String {
    let document = Html::parse_fragment(html);
    let mut out = String::with_capacity(html.len());
    for child in document.root_element().children() {
        serialize_node(&child, &mut out);
    }
    out
}

fn serialize_node(node: &NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(text) => out.push_str(&text.text),
        Node::Element(element) => match element.name() {
            "style" | "script" | "head" | "title" | "meta" | "link" | "template" | "noscript"
            | "iframe" | "svg" => {}
            "img" => serialize_image(element, out),
            "table" if !is_data_table(node) => serialize_table_cells(node, out),
            name => {
                if is_hidden(element) {
                    return;
                }
                out.push('<');
                out.push_str(name);
                if name == "a" {
                    if let Some(href) = element.attr("href") {
                        out.push_str(" href=\"");
                        out.push_str(&href.replace('&', "&amp;").replace('"', "&quot;"));
                        out.push('"');
                    }
                }
                out.push('>');
                for child in node.children() {
                    serialize_node(&child, out);
                }
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
        },
        _ => {}
    }
}

/// Images render as links to their source: `🖼 alt` anchored at the src URL.
/// Inline `cid:` images have no terminal-reachable URL, so they stay markers.
fn serialize_image(element: &scraper::node::Element, out: &mut String) {
    if is_spacer_image(element) {
        return;
    }
    let src = element.attr("src").map(str::trim).unwrap_or("");
    let remote = src.starts_with("http://") || src.starts_with("https://");
    let inline = src.starts_with("cid:");
    if !remote && !inline {
        out.push_str(" 🖼 ");
        return;
    }
    let alt = element
        .attr("alt")
        .map(str::trim)
        .filter(|alt| !alt.is_empty())
        .unwrap_or("image");
    if remote {
        out.push_str("<a href=\"");
        out.push_str(&src.replace('&', "&amp;").replace('"', "&quot;"));
        out.push_str("\">");
    }
    out.push_str("🖼 ");
    out.push_str(alt);
    if remote {
        out.push_str("</a>");
    }
}

fn serialize_table_cells(node: &NodeRef<'_, Node>, out: &mut String) {
    for child in node.children() {
        match child.value().as_element() {
            Some(element) if element.name() == "td" || element.name() == "th" => {
                for grandchild in child.children() {
                    serialize_node(&grandchild, out);
                }
                out.push_str("<br>");
            }
            Some(element) if element.name() == "table" => {
                if is_data_table(&child) {
                    serialize_node(&child, out);
                } else {
                    serialize_table_cells(&child, out);
                }
            }
            _ => serialize_table_cells(&child, out),
        }
    }
}

fn is_data_table(node: &NodeRef<'_, Node>) -> bool {
    let Some(element) = node.value().as_element() else {
        return false;
    };
    if element
        .attr("border")
        .is_some_and(|border| border.trim() != "0")
    {
        return true;
    }
    if matches!(element.attr("role"), Some("grid" | "table")) {
        return true;
    }
    node.descendants().any(|descendant| {
        descendant
            .value()
            .as_element()
            .is_some_and(|el| el.name() == "th")
    })
}

fn is_spacer_image(element: &scraper::node::Element) -> bool {
    if element.attr("role") == Some("presentation") {
        return true;
    }
    let dimension = |name: &str| {
        element
            .attr(name)
            .and_then(|value| value.trim().parse::<u32>().ok())
    };
    if dimension("width").is_some_and(|value| value <= 10)
        || dimension("height").is_some_and(|value| value <= 10)
    {
        return true;
    }
    if element.attr("alt").is_some_and(str::is_empty) {
        return true;
    }
    element
        .attr("src")
        .is_some_and(|src| src.contains("spacer") || src.contains("pixel"))
}

fn is_hidden(element: &scraper::node::Element) -> bool {
    if element.attr("hidden").is_some() || element.attr("aria-hidden") == Some("true") {
        return true;
    }
    element.attr("style").is_some_and(|style| {
        let compact = style.replace(' ', "").to_ascii_lowercase();
        compact.contains("display:none") || compact.contains("visibility:hidden")
    })
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

fn collapse_blank_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let mut collapsed: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        if is_blank(&line) && collapsed.last().map_or(true, is_blank) {
            continue;
        }
        collapsed.push(line);
    }
    while collapsed.last().is_some_and(is_blank) {
        collapsed.pop();
    }
    collapsed
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

fn safe_link_target(annotations: &[RichAnnotation]) -> Option<String> {
    annotations.iter().find_map(|annotation| {
        let RichAnnotation::Link(url) = annotation else {
            return None;
        };
        if !safe_link(url) {
            return None;
        }
        let safe = strip_terminal_controls(url.trim());
        let (bounded, _) = truncate_utf8(&safe, 2_048);
        Some(bounded.to_owned())
    })
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

    fn render(message: &Message, width: u16) -> Vec<Line<'static>> {
        EmailBodyRenderer::default().render_message(message, width)
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
        let lines = render(
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
        let lines = render(
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
        let lines = render(&message(Some("<table><tr><td>invoice"), None), 24);
        assert!(text(&lines).contains("invoice"));
    }

    #[test]
    fn wraps_html_to_the_requested_width() {
        let lines = render(
            &message(
                Some("<p>This is a deliberately long sentence that must wrap.</p>"),
                None,
            ),
            20,
        );
        assert!(lines.len() > 1);
    }

    #[test]
    fn exposes_link_targets_without_terminal_escape_sequences() {
        let lines = render(
            &message(
                Some(r#"<p><a href="https://example.com/inbox">Open your Inbox</a></p>"#),
                None,
            ),
            80,
        );
        let output = text(&lines);
        assert!(output.contains("Open your Inbox"));
        assert!(output.contains("[1]"));
        assert!(output.contains("https://example.com/inbox"));
        assert!(!output.contains('\u{1b}'));
    }

    #[test]
    fn tags_links_with_hyperlink_ids_for_osc8() {
        let renderer = EmailBodyRenderer::default();
        let lines = renderer.render_message(
            &message(
                Some(r#"<p><a href="https://example.com/inbox">Open your Inbox</a></p>"#),
                None,
            ),
            80,
        );
        let tagged = lines.iter().flat_map(|line| line.spans.iter()).any(|span| {
            span.content.contains("Open your Inbox")
                && span.style.underline_color == Some(Color::Rgb(0, 0, 1))
        });
        assert!(tagged);
        assert_eq!(renderer.hyperlinks(), vec!["https://example.com/inbox"]);
    }

    #[test]
    fn flattens_presentational_tables_without_box_drawing() {
        let lines = render(
            &message(
                Some(
                    r#"<table role="presentation" cellpadding="0"><tr><td>Earn 2% back</td></tr><tr><td><a href="https://example.com">Unlock</a></td></tr></table>"#,
                ),
                None,
            ),
            80,
        );
        let output = text(&lines);
        assert!(output.contains("Earn 2% back"));
        assert!(output.contains("Unlock"));
        assert!(!output.contains('│'));
        assert!(!output.contains('─'));
        assert!(!output.contains('┌'));
    }

    #[test]
    fn keeps_data_tables_column_aligned() {
        let lines = render(
            &message(
                Some(
                    "<table border=\"1\"><tr><th>Item</th><th>Price</th></tr><tr><td>Widget</td><td>$9</td></tr></table>",
                ),
                None,
            ),
            80,
        );
        let output = text(&lines);
        assert!(output.contains("Widget"));
        assert!(output.contains('│') || output.contains('─'));
    }

    #[test]
    fn images_become_links_to_their_source() {
        let renderer = EmailBodyRenderer::default();
        let lines = renderer.render_message(
            &message(
                Some(
                    r#"<p><img src="pixel.gif" width="1" height="1"><img alt="New arrivals" src="https://cdn.example.com/hero.png" width="600" height="200"></p>"#,
                ),
                None,
            ),
            80,
        );
        let output = text(&lines);
        assert!(!output.contains("pixel"));
        assert!(output.contains("🖼 New arrivals"));
        assert!(output.contains("https://cdn.example.com/hero.png"));
        assert_eq!(
            renderer.hyperlinks(),
            vec!["https://cdn.example.com/hero.png"]
        );
    }

    #[test]
    fn inline_cid_images_stay_markers() {
        let lines = render(
            &message(
                Some(r#"<img alt="logo" src="cid:part1" width="100" height="40">"#),
                None,
            ),
            80,
        );
        let output = text(&lines);
        assert!(output.contains(" logo"));
        assert!(!output.contains("cid:"));
    }

    #[test]
    fn collapses_blank_line_runs_and_hidden_content() {
        let lines = render(
            &message(
                Some(
                    r#"<p>a</p><div style="display:none">secret</div><div></div><div></div><p>b</p>"#,
                ),
                None,
            ),
            80,
        );
        let output = text(&lines);
        assert!(!output.contains("secret"));
        assert!(!output.contains("\n\n\n"));
    }
}
