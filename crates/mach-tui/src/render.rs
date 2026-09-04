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
use mach_core::store::ThreadSummary;
use mach_core::{split_of, Split};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};
use std::borrow::Borrow;

use crate::app::{
    App, ComposerField, ComposerView, InboxView, SearchView, SyncState, ThreadView, View,
};

const ACCENT: Color = Color::Rgb(0x7c, 0x9c, 0xff);
const DIM: Color = Color::Rgb(0x88, 0x88, 0x88);
const UNREAD_DOT: Color = Color::Rgb(0xff, 0xb5, 0x6b);
const SELECTED_BG: Color = Color::Rgb(0x1e, 0x29, 0x40);
const STARRED: Color = Color::Rgb(0xff, 0xd1, 0x5c);

pub fn draw(f: &mut Frame, app: &App) {
    // Publish this frame's link table so the backend can translate
    // underline-color ids into OSC 8 hyperlinks while drawing.
    match &app.view {
        View::Thread(v) => app.hyperlinks.publish(v.body_renderer.hyperlinks()),
        _ => app.hyperlinks.publish(Vec::new()),
    }
    // Every frame owns every terminal cell. This also repairs stale cells
    // after a resize or a previously width-unsafe message body.
    f.render_widget(Clear, f.area());
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
        View::Inbox(v) => draw_inbox(f, v, app.inbox_split, layout[1]),
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
            v.visible_threads(app.inbox_split).len()
        ),
        View::Thread(v) => format!("  ← Inbox  •  {}", trunc(&v.summary.subject, 60)),
        View::Composer(v) => format!("  Compose  •  draft {}", v.draft_id),
        View::Search(v) => format!("  / {}", v.query),
    };
    let bar = Line::from(vec![
        Span::styled(
            "  mach  ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("│ {} ", app.status.account),
            Style::default().fg(DIM),
        ),
        Span::styled(context, Style::default().fg(Color::White)),
    ]);
    f.render_widget(Paragraph::new(bar).alignment(Alignment::Left), area);
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let sync = match app.status.sync {
        SyncState::Ok => Span::styled("✓ Live", Style::default().fg(Color::Green)),
        SyncState::Syncing => Span::styled("⟳ Sync", Style::default().fg(Color::Yellow)),
        SyncState::Offline => Span::styled("○ Off", Style::default().fg(DIM)),
        SyncState::AuthExpired => Span::styled("⚠ Auth", Style::default().fg(Color::Red)),
    };

    let chord_hint = if !app.chord_buffer.is_empty() {
        format!(
            " │ chord: {} → ({})",
            app.chord_buffer,
            app.last_chord_continuations.join(", ")
        )
    } else {
        format!(" │ {}", app.status.hint)
    };
    let reauth = app
        .status
        .needs_reauth
        .first()
        .map(|email| format!(" · re-auth needed: {email}"))
        .unwrap_or_default();
    let (outbox, outbox_color) = if app.status.outbox.failed > 0 {
        (
            format!(" · {} failed", app.status.outbox.failed),
            Color::Red,
        )
    } else if app.status.outbox.pending > 0 {
        (
            format!(" · {} unsynced", app.status.outbox.pending),
            Color::Yellow,
        )
    } else {
        (String::new(), DIM)
    };

    let line = Line::from(vec![
        Span::raw(" "),
        sync,
        Span::styled(chord_hint, Style::default().fg(DIM)),
        Span::styled(reauth, Style::default().fg(Color::Red)),
        Span::styled(outbox, Style::default().fg(outbox_color)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_inbox(f: &mut Frame, v: &InboxView, split: Split, area: Rect) {
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

    let threads = v.visible_threads(split);
    let list_area = if v.label.as_str() == "INBOX" {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);
        let count = |candidate| {
            v.threads
                .iter()
                .filter(|thread| split_of(&thread.label_ids) == candidate && thread.unread)
                .count()
        };
        let tab = |label, candidate| {
            Span::styled(
                format!(" {label} {} ", count(candidate)),
                if split == candidate {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(DIM)
                },
            )
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                tab("1 Important", Split::Important),
                tab("2 Other", Split::Other),
                tab("3 Newsletters", Split::Newsletters),
            ])),
            chunks[0],
        );
        chunks[1]
    } else {
        inner
    };

    if threads.is_empty() {
        let msg = Paragraph::new("(empty)").style(Style::default().fg(DIM));
        f.render_widget(msg, list_area);
        return;
    }

    draw_mailbox_table(f, &threads, v.selected, v.viewport_top, list_area);
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
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
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

        let body_lines = v.body_renderer.lines(m, inner.width.saturating_sub(2));
        lines.extend(
            body_lines
                .into_iter()
                .take(if selected { usize::MAX } else { 5 }),
        );
        lines.push(Line::raw(""));
        lines.push(Line::styled("  ─────", Style::default().fg(DIM)));
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
            Constraint::Length(1), // Bcc
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
            Span::styled(format!(" {label} "), label_style),
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
        field("Bcc:    ", &v.bcc, v.field == ComposerField::Bcc),
        chunks[2],
    );
    f.render_widget(
        field("Subject:", &v.subject, v.field == ComposerField::Subject),
        chunks[3],
    );
    f.render_widget(
        Paragraph::new(Line::styled(" ─────", Style::default().fg(DIM))),
        chunks[4],
    );
    f.render_widget(
        Paragraph::new(v.body.clone())
            .wrap(Wrap { trim: false })
            .style(if v.field == ComposerField::Body {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(DIM)
            }),
        chunks[5],
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
        Span::styled(
            " / ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(v.query.clone(), Style::default().fg(Color::White)),
        Span::styled("▏", Style::default().fg(ACCENT)),
        Span::styled(
            if v.remote_searching {
                format!("  {} local · searching Gmail…", v.results.len())
            } else if v.remote_failures > 0 {
                format!(
                    "  {} results · {} account search failed",
                    v.results.len(),
                    v.remote_failures
                )
            } else {
                format!("  {} results", v.results.len())
            },
            Style::default().fg(DIM),
        ),
    ]));
    f.render_widget(prompt, chunks[0]);

    if v.results.is_empty() {
        let msg = if v.remote_searching {
            "Searching Gmail…"
        } else if v.query.is_empty() {
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

    draw_mailbox_table(f, &v.results, v.selected, 0, chunks[1]);
}

/// Render the inbox-shaped projection as an actual table. The table owns
/// clipping and column sizing, so a narrow pane can never turn one message
/// into multiple visual rows or displace the fixed header.
fn draw_mailbox_table<T: Borrow<ThreadSummary>>(
    f: &mut Frame,
    threads: &[T],
    selected: usize,
    viewport_top: usize,
    area: Rect,
) {
    let row_count = area.height.saturating_sub(1) as usize;
    if row_count == 0 || threads.is_empty() {
        return;
    }
    let mut top = viewport_top.min(threads.len().saturating_sub(1));
    if selected < top {
        top = selected;
    } else if selected >= top + row_count {
        top = selected.saturating_sub(row_count.saturating_sub(1));
    }
    let end = (top + row_count).min(threads.len());

    let rows = threads[top..end]
        .iter()
        .enumerate()
        .map(|(offset, thread)| {
            let thread = thread.borrow();
            let is_selected = top + offset == selected;
            let marker = Line::from(vec![
                if thread.unread {
                    Span::styled("●", Style::default().fg(UNREAD_DOT))
                } else {
                    Span::raw(" ")
                },
                Span::raw(" "),
                if thread.starred {
                    Span::styled("★", Style::default().fg(STARRED))
                } else {
                    Span::raw(" ")
                },
            ]);
            let subject_style = if thread.unread {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Row::new(vec![
                Cell::from(marker),
                Cell::from(thread.account_id.to_string()).style(Style::default().fg(DIM)),
                Cell::from(pretty_when(&thread.last_message_at)).style(Style::default().fg(DIM)),
                Cell::from(
                    thread
                        .participants
                        .first()
                        .map(String::as_str)
                        .unwrap_or("(no sender)")
                        .to_owned(),
                )
                .style(Style::default().fg(ACCENT)),
                Cell::from(thread.subject.clone()).style(subject_style),
                Cell::from(thread.snippet.clone()).style(Style::default().fg(DIM)),
            ])
            .style(if is_selected {
                Style::default().bg(SELECTED_BG)
            } else {
                Style::default()
            })
        });
    let header = Row::new(["", "Account", "Date", "From", "Subject", "Preview"])
        .style(Style::default().fg(DIM));
    let table = Table::new(rows, mailbox_widths(area.width))
        .header(header)
        .column_spacing(1);
    f.render_widget(table, area);
}

fn mailbox_widths(width: u16) -> [Constraint; 6] {
    if width >= 150 {
        [
            Constraint::Length(3),
            Constraint::Length(22),
            Constraint::Length(8),
            Constraint::Length(28),
            Constraint::Length(50),
            Constraint::Fill(1),
        ]
    } else if width >= 110 {
        [
            Constraint::Length(3),
            Constraint::Length(20),
            Constraint::Length(8),
            Constraint::Length(24),
            Constraint::Fill(2),
            Constraint::Fill(1),
        ]
    } else if width >= 80 {
        [
            Constraint::Length(3),
            Constraint::Length(16),
            Constraint::Length(8),
            Constraint::Length(20),
            Constraint::Fill(1),
            Constraint::Length(0),
        ]
    } else {
        [
            Constraint::Length(3),
            Constraint::Length(0),
            Constraint::Length(8),
            Constraint::Length(18),
            Constraint::Fill(1),
            Constraint::Length(0),
        ]
    }
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
        "SNOOZED" => "Snoozed".into(),
        "ALL" => "All Mail".into(),
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
