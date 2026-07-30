//! ratatui rendering. Each `View` has its own draw function. Layout:
//!
//! ```text
//! ┌─ top bar ──────────────────────────────────┐
//! │ mach │ account │ context (label / subject) │
//! ├────────────────────────────────────────────┤
//! │  view body                                  │
//! │                                             │
//! ├────────────────────────────────────────────┤
//! │  status / chord overlay                     │
//! └────────────────────────────────────────────┘
//! ```

use chrono::{DateTime, Local, Utc};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, ComposerField, ComposerView, InboxView, SearchView, SyncState, ThreadView, View};

const ACCENT: Color = Color::Rgb(0x7c, 0x9c, 0xff);
const DIM: Color = Color::Rgb(0x88, 0x88, 0x88);
const UNREAD_DOT: Color = Color::Rgb(0xff, 0xb5, 0x6b);
const SELECTED_BG: Color = Color::Rgb(0x1e, 0x29, 0x40);
const STARRED: Color = Color::Rgb(0xff, 0xd1, 0x5c);

pub fn draw(f: &mut Frame, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // top bar
            Constraint::Min(1),    // body
            Constraint::Length(1), // status
        ])
        .split(f.area());

    draw_top_bar(f, app, layout[0]);
    match &app.view {
        View::Inbox(v) => draw_inbox(f, v, layout[1]),
        View::Thread(v) => draw_thread(f, v, layout[1]),
        View::Composer(v) => draw_composer(f, v, layout[1]),
        View::Search(v) => draw_search(f, v, layout[1]),
    }
    draw_status_bar(f, app, layout[2]);
}

fn draw_top_bar(f: &mut Frame, app: &App, area: Rect) {
    let context = match &app.view {
        View::Inbox(v) => format!(
            "  {} ({} threads)",
            label_display(v.label.as_str()),
            v.threads.len()
        ),
        View::Thread(v) => format!("  ← Inbox  •  {}", trunc(&v.summary.subject, 60)),
        View::Composer(_) => "  Compose".to_string(),
        View::Search(v) => format!("  / {}", v.query),
    };
    let bar = Line::from(vec![
        Span::styled(
            "  mach  ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("│ {} ", app.status.account), Style::default().fg(DIM)),
        Span::styled(context, Style::default().fg(Color::White)),
    ]);
    f.render_widget(
        Paragraph::new(bar).alignment(Alignment::Left),
        area,
    );
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let sync = match app.status.sync {
        SyncState::Ok => Span::styled("✓ Live", Style::default().fg(Color::Green)),
        SyncState::Syncing => Span::styled("⟳ Sync", Style::default().fg(Color::Yellow)),
        SyncState::Offline => Span::styled("○ Off", Style::default().fg(DIM)),
        SyncState::AuthExpired => Span::styled("⚠ Auth", Style::default().fg(Color::Red)),
    };

    let chord_hint = if !app.chord_buffer.is_empty() {
        format!(" │ chord: {} → ({})", app.chord_buffer, app.last_chord_continuations.join(", "))
    } else {
        format!(" │ {}", app.status.hint)
    };

    let line = Line::from(vec![
        Span::raw(" "),
        sync,
        Span::styled(chord_hint, Style::default().fg(DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_inbox(f: &mut Frame, v: &InboxView, area: Rect) {
    let inner = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(DIM))
        .inner(area);
    // Render border separately so the list itself has full width.
    f.render_widget(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(DIM)),
        area,
    );

    if v.threads.is_empty() {
        let msg = Paragraph::new("(empty)").style(Style::default().fg(DIM));
        f.render_widget(msg, inner);
        return;
    }

    let row_count = inner.height as usize;
    // Adjust viewport so selected is visible. (Computed read-only here; the
    // viewport_top in state should be kept in sync on selection moves — for v1
    // we just recompute every draw which is cheap.)
    let mut top = v.viewport_top.min(v.threads.len().saturating_sub(1));
    if v.selected < top {
        top = v.selected;
    } else if v.selected >= top + row_count {
        top = v.selected.saturating_sub(row_count.saturating_sub(1));
    }
    let end = (top + row_count).min(v.threads.len());

    let lines: Vec<Line> = v.threads[top..end]
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let idx = top + i;
            let selected = idx == v.selected;
            let mut style = Style::default();
            if selected {
                style = style.bg(SELECTED_BG);
            }
            let unread_marker = if t.unread {
                Span::styled("●", Style::default().fg(UNREAD_DOT))
            } else {
                Span::raw(" ")
            };
            let star_marker = if t.starred {
                Span::styled("★", Style::default().fg(STARRED))
            } else {
                Span::raw(" ")
            };
            let from = trunc(
                t.participants.first().map(|s| s.as_str()).unwrap_or("(no sender)"),
                28,
            );
            let subject_style = if t.unread {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let subj = trunc(&t.subject, 50);
            let snippet = trunc(&t.snippet, 60);
            let when = pretty_when(&t.last_message_at);

            Line::from(vec![
                Span::raw(" "),
                unread_marker,
                Span::raw(" "),
                star_marker,
                Span::raw(" "),
                Span::styled(format!("{:<28} ", from), Style::default().fg(ACCENT)),
                Span::styled(format!("{:<50} ", subj), subject_style),
                Span::styled(format!("{} ", snippet), Style::default().fg(DIM)),
                Span::styled(format!("{:>8}", when), Style::default().fg(DIM)),
            ])
            .style(style)
        })
        .collect();

    let par = Paragraph::new(lines);
    f.render_widget(par, inner);
}

fn draw_thread(f: &mut Frame, v: &ThreadView, area: Rect) {
    let inner = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(DIM))
        .inner(area);
    f.render_widget(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(DIM)),
        area,
    );

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        v.summary.subject.clone(),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::raw(""));

    for (i, m) in v.messages.iter().enumerate() {
        let selected = i == v.selected_message;
        let header_style = if selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ACCENT)
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, header_style),
            Span::styled(format!("From: {}", m.from), header_style),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("Date: {}", pretty_full_when(&m.internal_date)),
                Style::default().fg(DIM),
            ),
        ]));
        lines.push(Line::raw(""));

        let body = m
            .body_plain
            .as_deref()
            .unwrap_or("(body not fetched — open online to backfill)");
        for line in body.lines().take(if selected { 200 } else { 5 }) {
            lines.push(line_with_osc8_links(line));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "  ─────",
            Style::default().fg(DIM),
        ));
        lines.push(Line::raw(""));
    }

    let par = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((v.scroll, 0));
    f.render_widget(par, inner);
}

fn draw_composer(f: &mut Frame, v: &ComposerView, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // To
            Constraint::Length(1), // Cc
            Constraint::Length(1), // Subject
            Constraint::Length(1), // separator
            Constraint::Min(1),    // Body
        ])
        .split(inner);

    fn field<'a>(label: &'a str, value: &'a str, active: bool) -> Paragraph<'a> {
        let label_style = if active {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let value_style = Style::default().fg(Color::White);
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} ", label), label_style),
            Span::styled(value.to_string(), value_style),
            if active {
                Span::styled("▏", Style::default().fg(ACCENT))
            } else {
                Span::raw("")
            },
        ]))
    }

    f.render_widget(
        field("To:     ", &v.to, v.field == ComposerField::To),
        chunks[0],
    );
    f.render_widget(
        field("Cc:     ", &v.cc, v.field == ComposerField::Cc),
        chunks[1],
    );
    f.render_widget(
        field("Subject:", &v.subject, v.field == ComposerField::Subject),
        chunks[2],
    );
    f.render_widget(
        Paragraph::new(Line::styled(" ─────", Style::default().fg(DIM))),
        chunks[3],
    );
    f.render_widget(
        Paragraph::new(v.body.clone())
            .wrap(Wrap { trim: false })
            .style(if v.field == ComposerField::Body {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(DIM)
            }),
        chunks[4],
    );
}

fn draw_search(f: &mut Frame, v: &SearchView, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(" / ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(v.query.clone(), Style::default().fg(Color::White)),
        Span::styled("▏", Style::default().fg(ACCENT)),
    ]));
    f.render_widget(prompt, chunks[0]);

    if v.results.is_empty() {
        let msg = if v.query.is_empty() {
            "Type to search…"
        } else {
            "No matches"
        };
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(DIM)),
            chunks[1],
        );
        return;
    }

    let lines: Vec<Line> = v
        .results
        .iter()
        .take(chunks[1].height as usize)
        .enumerate()
        .map(|(i, t)| {
            let selected = i == v.selected;
            let style = if selected {
                Style::default().bg(SELECTED_BG)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(if selected { " ▸ " } else { "   " }),
                Span::styled(
                    format!("{:<28} ", trunc(t.participants.first().map(|s| s.as_str()).unwrap_or(""), 28)),
                    Style::default().fg(ACCENT),
                ),
                Span::styled(
                    format!("{:<50} ", trunc(&t.subject, 50)),
                    Style::default().fg(Color::White),
                ),
                Span::styled(trunc(&t.snippet, 60), Style::default().fg(DIM)),
            ])
            .style(style)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), chunks[1]);
}

/// Build a ratatui `Line` from a raw string, wrapping detected URLs in
/// OSC 8 hyperlink escape sequences. Modern terminals (iTerm2, Kitty,
/// Ghostty, WezTerm, recent Terminal.app) make those clickable even when
/// the visible text wraps across multiple cells.
///
/// Escape format (per Hyperlink spec): `\x1b]8;;URL\x1b\\TEXT\x1b]8;;\x1b\\`.
/// ratatui passes raw bytes from `Span::raw` to the terminal verbatim.
fn line_with_osc8_links(s: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut last = 0;
    let bytes = s.as_bytes();
    while last < bytes.len() {
        // Find the next "http://" or "https://" occurrence at a word boundary.
        let rest = &s[last..];
        let pos_http = rest.find("http://");
        let pos_https = rest.find("https://");
        let next = match (pos_http, pos_https) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some(rel) = next else {
            spans.push(Span::raw(rest.to_string()));
            break;
        };
        let url_start = last + rel;
        // Word boundary check — refuse to match mid-word like "ahttp".
        if url_start > 0 {
            let prev_char = s[..url_start].chars().last();
            if let Some(c) = prev_char {
                if c.is_alphanumeric() && c != '/' {
                    // false positive — emit up through this point and continue.
                    spans.push(Span::raw(s[last..url_start + 4].to_string()));
                    last = url_start + 4;
                    continue;
                }
            }
        }
        // Push text before the URL.
        if url_start > last {
            spans.push(Span::raw(s[last..url_start].to_string()));
        }
        // Find URL end — first whitespace or terminating char.
        let url_end = s[url_start..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | ')' | ']' | '}')
            })
            .map(|n| url_start + n)
            .unwrap_or(s.len());
        let url = &s[url_start..url_end];
        // OSC 8 wrap: ESC ] 8 ; ; URL ESC \ TEXT ESC ] 8 ; ; ESC \
        let wrapped = format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\");
        spans.push(Span::styled(
            wrapped,
            Style::default().fg(ACCENT).add_modifier(Modifier::UNDERLINED),
        ));
        last = url_end;
    }
    Line::from(spans)
}

fn trunc(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn label_display(id: &str) -> String {
    match id {
        "INBOX" => "Inbox".into(),
        "STARRED" => "Starred".into(),
        "SENT" => "Sent".into(),
        "DRAFT" => "Drafts".into(),
        "TRASH" => "Trash".into(),
        "SPAM" => "Spam".into(),
        "DONE" => "Done".into(),
        other => other.into(),
    }
}

fn pretty_when(dt: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let dt_local: DateTime<Local> = DateTime::from(*dt);
    let diff = now.signed_duration_since(*dt);
    if diff.num_hours() < 24 {
        dt_local.format("%-I:%M %p").to_string()
    } else if diff.num_days() < 7 {
        dt_local.format("%a").to_string()
    } else if diff.num_days() < 365 {
        dt_local.format("%b %-d").to_string()
    } else {
        dt_local.format("%Y").to_string()
    }
}

fn pretty_full_when(dt: &DateTime<Utc>) -> String {
    let dt_local: DateTime<Local> = DateTime::from(*dt);
    dt_local.format("%a %b %-d, %Y %-I:%M %p").to_string()
}
