//! TUI state machine + main event loop.
//!
//! Single-threaded model: a `tokio::select!` drains crossterm input events,
//! tick timers, and store-update notifications. Every mutation goes through
//! `Dispatcher::execute` so the TUI never touches state outside the
//! Action enum — same surface the CLI and MCP use.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use mach_core::ids::{AccountScope, LabelId, ThreadId};
use mach_core::store::{MailStore, Message, ThreadSummary};
use mach_core::{
    keymap::{KeyContext, Keymap, Mode, Resolution},
    Action, Dispatcher,
};
use mach_store::SqliteStore;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, warn};

use crate::config::load_keymap;
use crate::keys::key_event_to_chord;
use crate::render::draw;

/// Top-level app state. The `View` enum is the active screen; selection
/// and chord state live alongside.
pub struct App {
    pub keymap: Keymap,
    pub store: Arc<SqliteStore>,
    pub scope: AccountScope,
    pub dispatcher: Dispatcher,
    /// Body fetcher is optional — if creds are missing we run offline,
    /// serving whatever's already cached.
    pub body_fetchers: Arc<mach_gmail::GmailAccountPool>,
    search_events: Option<mpsc::UnboundedSender<SearchEvent>>,
    remote_search_task: Option<tokio::task::JoinHandle<()>>,

    pub view: View,
    pub status: StatusLine,
    pub chord_buffer: String,
    pub last_chord_continuations: Vec<String>,

    pub running: bool,
}

/// The active screen. Plus per-screen state.
pub enum View {
    Inbox(InboxView),
    Thread(ThreadView),
    Composer(ComposerView),
    Search(SearchView),
}

pub struct InboxView {
    pub label: LabelId,
    pub threads: Vec<ThreadSummary>,
    pub selected: usize,
    /// 0-indexed top row of the visible viewport; scrolls when selection
    /// goes off-screen.
    pub viewport_top: usize,
}

pub struct ThreadView {
    pub thread_id: ThreadId,
    pub summary: ThreadSummary,
    pub messages: Vec<Message>,
    pub scroll: u16,
    /// Selected message index (for `$current_message` resolution).
    pub selected_message: usize,
}

pub struct ComposerView {
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub field: ComposerField,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ComposerField {
    To,
    Cc,
    Subject,
    Body,
}

pub struct SearchView {
    pub query: String,
    pub results: Vec<ThreadSummary>,
    pub selected: usize,
    pub remote_searching: bool,
    pub remote_failures: usize,
    /// Background view to fall back to when search closes.
    pub background: Box<View>,
}

/// One-line status footer. Sync status + chord hint + binding hint.
pub struct StatusLine {
    pub account: String,
    pub sync: SyncState,
    pub hint: String,
}

#[allow(dead_code)] // AuthExpired is reserved for typed OAuth failure reporting.
pub enum SyncState {
    Ok,
    Syncing,
    Offline,
    AuthExpired,
}

enum PullEvent {
    Started,
    Finished(mach_gmail::PullReport),
}

struct SearchEvent {
    query: String,
    report: mach_gmail::RemoteSearchReport,
}

impl App {
    pub async fn new(store: Arc<SqliteStore>, scope: AccountScope) -> Result<Self> {
        let keymap = load_keymap()?;
        let dispatcher = Dispatcher::with_scope(store.clone(), scope.clone());

        // Try to construct a body fetcher; if OAuth creds aren't set up,
        // boot offline. The user can still browse cached mail.
        let body_fetchers =
            match mach_gmail::GmailAccountPool::from_stored_credentials(store.clone()) {
                Ok(pool) => Arc::new(pool),
                Err(e) => {
                    warn!(error = %e, "no body fetchers; running offline");
                    Arc::new(mach_gmail::GmailAccountPool::default())
                }
            };

        let inbox = load_inbox(&store, &scope, "INBOX").await?;
        let view = View::Inbox(inbox);

        let account = match &scope {
            AccountScope::One(account) => account.to_string(),
            AccountScope::All if !body_fetchers.is_empty() => {
                format!("All accounts ({})", body_fetchers.accounts().count())
            }
            AccountScope::All => "offline".into(),
        };
        let status = StatusLine {
            account,
            sync: if !body_fetchers.is_empty() {
                SyncState::Ok
            } else {
                SyncState::Offline
            },
            hint: "j/k:nav  e:archive  c:compose  /:search  q:quit".into(),
        };

        Ok(Self {
            keymap,
            store,
            scope,
            dispatcher,
            body_fetchers,
            search_events: None,
            remote_search_task: None,
            view,
            status,
            chord_buffer: String::new(),
            last_chord_continuations: Vec::new(),
            running: true,
        })
    }

    /// Build a `KeyContext` from current view state.
    pub fn key_context(&self) -> KeyContext {
        match &self.view {
            View::Inbox(v) => KeyContext {
                selection: v
                    .current_thread_id()
                    .map(|t| vec![t.as_str().to_string()])
                    .unwrap_or_default(),
                current_thread: v.current_thread_id().map(|t| t.as_str().to_string()),
                current_message: None,
                current_draft: None,
            },
            View::Thread(v) => KeyContext {
                selection: vec![v.thread_id.as_str().to_string()],
                current_thread: Some(v.thread_id.as_str().to_string()),
                current_message: v.current_message_id().map(|m| m.as_str().to_string()),
                current_draft: None,
            },
            View::Composer(_) => KeyContext::default(),
            View::Search(v) => KeyContext {
                selection: v
                    .current_thread_id()
                    .map(|t| vec![t.as_str().to_string()])
                    .unwrap_or_default(),
                current_thread: v.current_thread_id().map(|t| t.as_str().to_string()),
                ..Default::default()
            },
        }
    }

    pub fn current_mode(&self) -> Mode {
        match &self.view {
            View::Inbox(_) => Mode::Normal,
            View::Thread(_) => Mode::Reading,
            View::Composer(_) => Mode::Composing,
            View::Search(_) => Mode::Search,
        }
    }
}

impl InboxView {
    pub fn current_thread(&self) -> Option<&ThreadSummary> {
        self.threads.get(self.selected)
    }
    pub fn current_thread_id(&self) -> Option<&ThreadId> {
        self.current_thread().map(|t| &t.id)
    }
}

impl ThreadView {
    pub fn current_message(&self) -> Option<&Message> {
        self.messages.get(self.selected_message)
    }
    pub fn current_message_id(&self) -> Option<&mach_core::ids::MessageId> {
        self.current_message().map(|m| &m.id)
    }
}

impl SearchView {
    pub fn current_thread(&self) -> Option<&ThreadSummary> {
        self.results.get(self.selected)
    }
    pub fn current_thread_id(&self) -> Option<&ThreadId> {
        self.current_thread().map(|t| &t.id)
    }
}

async fn load_inbox(store: &SqliteStore, scope: &AccountScope, label: &str) -> Result<InboxView> {
    let lid = LabelId::new(label);
    let threads = store
        .list_threads_in_label(scope, &lid, 200)
        .await
        .context("listing inbox threads")?;
    Ok(InboxView {
        label: lid,
        threads,
        selected: 0,
        viewport_top: 0,
    })
}

/// Run the TUI to completion. Returns when the user quits.
pub async fn run(store: Arc<SqliteStore>, scope: AccountScope) -> Result<()> {
    let mut app = App::new(store, scope).await?;
    let (search_tx, mut search_rx) = mpsc::unbounded_channel();
    app.search_events = Some(search_tx);

    // Terminal setup.
    enable_raw_mode().context("enable_raw_mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).context("alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    let (pull_tx, mut pull_rx) = mpsc::unbounded_channel();
    let pull_task = tokio::spawn(periodic_pull(
        app.body_fetchers.clone(),
        app.scope.clone(),
        pull_tx,
    ));
    let result = main_loop(&mut app, &mut terminal, &mut pull_rx, &mut search_rx).await;
    pull_task.abort();
    if let Some(task) = app.remote_search_task.take() {
        task.abort();
    }

    // Always restore terminal state, even on error.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();

    result
}

async fn main_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    pull_events: &mut mpsc::UnboundedReceiver<PullEvent>,
    search_events: &mut mpsc::UnboundedReceiver<SearchEvent>,
) -> Result<()> {
    // Initial draw before we block on input.
    terminal.draw(|f| draw(f, app)).context("initial draw")?;

    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));

    while app.running {
        tokio::select! {
            ev = input.next() => {
                match ev {
                    Some(Ok(Event::Key(k))) if k.kind != KeyEventKind::Release => {
                        handle_key(app, k).await;
                    }
                    Some(Ok(_)) => {} // resize, mouse, paste — re-draw covers it
                    Some(Err(e)) => {
                        error!(error = %e, "input error");
                        break;
                    }
                    None => break,
                }
            }
            _ = tick.tick() => {
                // Heartbeat keeps status/chord feedback responsive.
            }
            Some(event) = pull_events.recv() => {
                handle_pull_event(app, event).await;
            }
            Some(event) = search_events.recv() => {
                handle_search_event(app, event);
            }
        }
        terminal.draw(|f| draw(f, app)).context("draw")?;
    }
    Ok(())
}

async fn periodic_pull(
    accounts: Arc<mach_gmail::GmailAccountPool>,
    scope: AccountScope,
    events: mpsc::UnboundedSender<PullEvent>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if events.send(PullEvent::Started).is_err() {
            break;
        }
        let report = accounts.pull_updates(&scope).await;
        if events.send(PullEvent::Finished(report)).is_err() {
            break;
        }
    }
}

async fn handle_pull_event(app: &mut App, event: PullEvent) {
    match event {
        PullEvent::Started => app.status.sync = SyncState::Syncing,
        PullEvent::Finished(report) => {
            for (account, error) in &report.failures {
                warn!(account = %account, error, "background pull failed");
            }
            app.status.sync = if report.succeeded > 0 {
                SyncState::Ok
            } else {
                SyncState::Offline
            };
            if report.succeeded > 0 {
                refresh_visible_inbox(app).await;
            }
        }
    }
}

async fn refresh_visible_inbox(app: &mut App) {
    let View::Inbox(current) = &app.view else {
        return;
    };
    let label = current.label.clone();
    let selected = current
        .current_thread()
        .map(|thread| (thread.account_id.clone(), thread.id.clone()));
    match load_inbox(&app.store, &app.scope, label.as_str()).await {
        Ok(mut inbox) => {
            if let Some((account, id)) = selected {
                inbox.selected = inbox
                    .threads
                    .iter()
                    .position(|thread| thread.account_id == account && thread.id == id)
                    .unwrap_or(0);
            }
            app.view = View::Inbox(inbox);
        }
        Err(error) => warn!(%error, "refreshing inbox after background pull failed"),
    }
}

fn handle_search_event(app: &mut App, event: SearchEvent) {
    let View::Search(search) = &mut app.view else {
        return;
    };
    if search.query != event.query {
        return;
    }
    let selected = search
        .current_thread()
        .map(|thread| (thread.account_id.clone(), thread.id.clone()));
    search.results.extend(event.report.results);
    search.results.sort_by(|left, right| {
        right
            .last_message_at
            .cmp(&left.last_message_at)
            .then_with(|| left.account_id.as_str().cmp(right.account_id.as_str()))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    search
        .results
        .dedup_by(|left, right| left.account_id == right.account_id && left.id == right.id);
    search.results.truncate(50);
    search.selected = selected
        .and_then(|(account, id)| {
            search
                .results
                .iter()
                .position(|thread| thread.account_id == account && thread.id == id)
        })
        .unwrap_or(0);
    search.remote_searching = false;
    search.remote_failures = event.report.failures.len();
    for (account, error) in event.report.failures {
        warn!(account = %account, error, "remote search failed");
    }
}

async fn handle_key(app: &mut App, k: crossterm::event::KeyEvent) {
    // Composer text input is special: most keys go to the field, not the
    // keymap. We only consult the keymap for specific control bindings.
    if let View::Composer(_) = &app.view {
        if composer_swallow_key(app, &k) {
            return;
        }
    }
    // Search text input similarly: chars feed the query, special keys
    // (Esc/Enter/Up/Down) drive selection.
    if let View::Search(_) = &app.view {
        if search_swallow_key(app, &k).await {
            return;
        }
    }

    let Some(chord_atom) = key_event_to_chord(&k) else {
        return;
    };

    let new_chord = if app.chord_buffer.is_empty() {
        chord_atom
    } else {
        format!("{} {}", app.chord_buffer, chord_atom)
    };

    let ctx = app.key_context();
    match app.keymap.resolve(app.current_mode(), &new_chord, &ctx) {
        Resolution::Action(action) => {
            app.chord_buffer.clear();
            app.last_chord_continuations.clear();
            execute_action(app, action).await;
        }
        Resolution::Prefix(conts) => {
            app.chord_buffer = new_chord;
            app.last_chord_continuations = conts
                .iter()
                .map(|c| format!("{} → {}", c.next, c.action_name))
                .collect();
        }
        Resolution::Unbound => {
            // Try fallback: maybe this is a single-key action that's
            // displacing a chord (e.g. user pressed `g` then a key that
            // isn't a continuation — treat the new key as fresh).
            if !app.chord_buffer.is_empty() {
                app.chord_buffer.clear();
                app.last_chord_continuations.clear();
                let chord = key_event_to_chord(&k).unwrap_or_default();
                if let Resolution::Action(action) =
                    app.keymap.resolve(app.current_mode(), &chord, &ctx)
                {
                    execute_action(app, action).await;
                }
            }
        }
    }
}

fn composer_swallow_key(app: &mut App, k: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let View::Composer(c) = &mut app.view else {
        return false;
    };
    // Ctrl-Enter and Esc + Ctrl-S go to the keymap.
    if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('s')) {
        return false;
    }
    if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Enter) {
        return false;
    }
    if matches!(k.code, KeyCode::Esc) {
        return false;
    }
    match k.code {
        KeyCode::Tab => {
            c.field = match c.field {
                ComposerField::To => ComposerField::Cc,
                ComposerField::Cc => ComposerField::Subject,
                ComposerField::Subject => ComposerField::Body,
                ComposerField::Body => ComposerField::To,
            };
            true
        }
        KeyCode::BackTab => {
            c.field = match c.field {
                ComposerField::To => ComposerField::Body,
                ComposerField::Cc => ComposerField::To,
                ComposerField::Subject => ComposerField::Cc,
                ComposerField::Body => ComposerField::Subject,
            };
            true
        }
        KeyCode::Backspace => {
            let target = match c.field {
                ComposerField::To => &mut c.to,
                ComposerField::Cc => &mut c.cc,
                ComposerField::Subject => &mut c.subject,
                ComposerField::Body => &mut c.body,
            };
            target.pop();
            true
        }
        KeyCode::Enter => {
            // Only the body gets newlines; other fields ignore enter.
            if c.field == ComposerField::Body {
                c.body.push('\n');
            }
            true
        }
        KeyCode::Char(ch) => {
            let target = match c.field {
                ComposerField::To => &mut c.to,
                ComposerField::Cc => &mut c.cc,
                ComposerField::Subject => &mut c.subject,
                ComposerField::Body => &mut c.body,
            };
            target.push(ch);
            true
        }
        _ => false,
    }
}

async fn search_swallow_key(app: &mut App, k: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyCode;
    let View::Search(s) = &mut app.view else {
        return false;
    };
    match k.code {
        KeyCode::Esc => {
            if let Some(task) = app.remote_search_task.take() {
                task.abort();
            }
            // Close search; restore background view.
            let bg = std::mem::replace(
                &mut *s.background,
                View::Inbox(InboxView {
                    label: LabelId::new("INBOX"),
                    threads: Vec::new(),
                    selected: 0,
                    viewport_top: 0,
                }),
            );
            app.view = bg;
            true
        }
        KeyCode::Enter => {
            if let Some(t) = s.current_thread().cloned() {
                if let Some(task) = app.remote_search_task.take() {
                    task.abort();
                }
                let id = t.id.clone();
                drop(t);
                open_thread(app, id).await;
            }
            true
        }
        KeyCode::Up => {
            if s.selected > 0 {
                s.selected -= 1;
            }
            true
        }
        KeyCode::Down => {
            if s.selected + 1 < s.results.len() {
                s.selected += 1;
            }
            true
        }
        KeyCode::Backspace => {
            s.query.pop();
            run_search(app).await;
            true
        }
        KeyCode::Char(ch) => {
            s.query.push(ch);
            run_search(app).await;
            true
        }
        _ => false,
    }
}

async fn run_search(app: &mut App) {
    let query = if let View::Search(s) = &app.view {
        s.query.clone()
    } else {
        return;
    };
    if query.trim().is_empty() {
        if let Some(task) = app.remote_search_task.take() {
            task.abort();
        }
        if let View::Search(s) = &mut app.view {
            s.results.clear();
            s.selected = 0;
            s.remote_searching = false;
            s.remote_failures = 0;
        }
        return;
    }
    match app.store.search_threads(&app.scope, &query, 50).await {
        Ok(hits) => {
            if let View::Search(s) = &mut app.view {
                s.results = hits;
                s.selected = 0;
                s.remote_searching = !app.body_fetchers.is_empty();
                s.remote_failures = 0;
            }
        }
        Err(e) => warn!(error = %e, "search failed"),
    }
    if let Some(task) = app.remote_search_task.take() {
        task.abort();
    }
    if app.body_fetchers.is_empty() {
        return;
    }
    let Some(events) = app.search_events.clone() else {
        return;
    };
    let accounts = app.body_fetchers.clone();
    let scope = app.scope.clone();
    let task_query = query.clone();
    app.remote_search_task = Some(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let report = accounts.search_remote(&scope, &task_query, 50).await;
        let _ = events.send(SearchEvent {
            query: task_query,
            report,
        });
    }));
}

async fn execute_action(app: &mut App, action: Action) {
    debug!(?action, "execute_action");
    match &action {
        Action::Quit => {
            app.running = false;
            return;
        }
        Action::BackToList => {
            // From thread/composer/search → back to inbox label list.
            if let View::Inbox(_) = app.view {
                // Already on inbox; nothing to do.
                return;
            }
            if let Ok(inbox) = load_inbox(&app.store, &app.scope, "INBOX").await {
                app.view = View::Inbox(inbox);
            }
            return;
        }
        Action::SelectNext => {
            advance_selection(app, 1);
            return;
        }
        Action::SelectPrev => {
            advance_selection(app, -1);
            return;
        }
        Action::ComposeNew => {
            app.view = View::Composer(ComposerView {
                to: String::new(),
                cc: String::new(),
                subject: String::new(),
                body: String::new(),
                field: ComposerField::To,
            });
            return;
        }
        Action::OpenThread { id } => {
            open_thread(app, id.clone()).await;
            return;
        }
        Action::OpenLabel { label_id } => {
            match load_inbox(&app.store, &app.scope, label_id.as_str()).await {
                Ok(inbox) => app.view = View::Inbox(inbox),
                Err(e) => warn!(error = %e, "open_label failed"),
            }
            return;
        }
        Action::Search { .. } => {
            let bg = std::mem::replace(
                &mut app.view,
                View::Inbox(InboxView {
                    label: LabelId::new("INBOX"),
                    threads: Vec::new(),
                    selected: 0,
                    viewport_top: 0,
                }),
            );
            app.view = View::Search(SearchView {
                query: String::new(),
                results: Vec::new(),
                selected: 0,
                remote_searching: false,
                remote_failures: 0,
                background: Box::new(bg),
            });
            return;
        }
        _ => {}
    }

    // All other actions go through the dispatcher (mutations, etc.).
    let is_archive_or_trash = matches!(&action, Action::Archive { .. } | Action::Trash { .. });
    let is_in_reading_mode = matches!(&app.view, View::Thread(_));

    match app.dispatcher.execute(action.clone()).await {
        Ok(outcome) => {
            debug!(?outcome, "dispatched");

            // From inbox view: drop the affected row in place (Superhuman feel —
            // the list visibly closes the gap).
            if is_archive_or_trash {
                if let View::Inbox(v) = &mut app.view {
                    v.threads
                        .retain(|t| !outcome.changed_threads.iter().any(|c| c == &t.id));
                    if v.selected >= v.threads.len() && !v.threads.is_empty() {
                        v.selected = v.threads.len() - 1;
                    }
                }
            }

            // From thread reader: archive/trash should kick you back to the
            // inbox + advance to the next thread. That's the muscle-memory
            // expectation from Superhuman, Gmail, Mail.app — you're done with
            // this thread, hand me the next one.
            if is_archive_or_trash && is_in_reading_mode {
                let removed_id = outcome.changed_threads.first().cloned();
                if let Ok(mut inbox) = load_inbox(&app.store, &app.scope, "INBOX").await {
                    // Pick the thread that was *next* in the old list (so the
                    // selection points to what would have shown up underneath
                    // the cursor). Falls back to the head of the list.
                    if let Some(removed) = removed_id {
                        let preferred = inbox
                            .threads
                            .iter()
                            .position(|t| t.id == removed)
                            .unwrap_or(0);
                        inbox.selected = preferred.min(inbox.threads.len().saturating_sub(1));
                    }
                    app.view = View::Inbox(inbox);
                }
            }
        }
        Err(e) => warn!(error = %e, "dispatch failed"),
    }
}

async fn open_thread(app: &mut App, id: ThreadId) {
    let Ok(Some(summary)) = app.store.get_thread(&app.scope, &id).await else {
        warn!(thread_id = %id, "thread not found");
        return;
    };
    // Background body fetch if creds are available. We block here for the
    // sake of simplicity; for v1.5 we'll move this to a background task.
    if let Some(fetcher) = app.body_fetchers.get(&summary.account_id) {
        if let Err(e) = fetcher.fetch_if_needed(&id).await {
            warn!(error = %e, "body backfill failed");
        }
    }
    let messages = app
        .store
        .list_messages_in_thread(&AccountScope::One(summary.account_id.clone()), &id)
        .await
        .unwrap_or_default();
    app.view = View::Thread(ThreadView {
        thread_id: id,
        summary,
        messages,
        scroll: 0,
        selected_message: 0,
    });
}

fn advance_selection(app: &mut App, delta: i32) {
    match &mut app.view {
        View::Inbox(v) => {
            if v.threads.is_empty() {
                return;
            }
            let next = (v.selected as i32 + delta).clamp(0, v.threads.len() as i32 - 1) as usize;
            v.selected = next;
        }
        View::Thread(v) => {
            if !v.messages.is_empty() {
                let next = (v.selected_message as i32 + delta).clamp(0, v.messages.len() as i32 - 1)
                    as usize;
                v.selected_message = next;
            }
        }
        View::Search(v) => {
            if !v.results.is_empty() {
                let next =
                    (v.selected as i32 + delta).clamp(0, v.results.len() as i32 - 1) as usize;
                v.selected = next;
            }
        }
        _ => {}
    }
}
