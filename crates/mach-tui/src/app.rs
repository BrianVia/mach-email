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
use mach_core::ids::{AccountScope, DraftId, LabelId, ThreadId};
use mach_core::store::{Draft, MailStore, Message, OutboxSummary, ThreadSummary};
use mach_core::{
    keymap::{KeyContext, Keymap, Mode, Resolution},
    split_of, Action, ActionOutcome, Dispatcher, DraftPatch, Split,
};
use mach_store::SqliteStore;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, warn};

use crate::backend::HyperlinkRegistry;
use crate::config::load_keymap;
use crate::email_body::EmailBodyRenderer;
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
    pub hyperlinks: Arc<HyperlinkRegistry>,
    search_events: Option<mpsc::UnboundedSender<SearchEvent>>,
    remote_search_task: Option<tokio::task::JoinHandle<()>>,

    pub view: View,
    pub status: StatusLine,
    pub chord_buffer: String,
    pub last_chord_continuations: Vec<String>,
    pub inbox_split: Split,

    pub running: bool,
}

/// The active screen. Plus per-screen state.
pub enum View {
    Inbox(InboxView),
    Thread(Box<ThreadView>),
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
    pub body_renderer: EmailBodyRenderer,
    pub scroll: u16,
    /// Selected message index (for `$current_message` resolution).
    pub selected_message: usize,
}

pub struct ComposerView {
    pub draft_id: DraftId,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    pub field: ComposerField,
    previous_view: Box<View>,
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
    pub needs_reauth: Vec<String>,
    pub outbox: OutboxSummary,
}

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
        let hyperlinks = Arc::new(HyperlinkRegistry::default());
        let needs_reauth = mach_gmail::credentials::load_all()?
            .into_iter()
            .filter(|credentials| {
                credentials.needs_reauth()
                    && scope
                        .account()
                        .map_or(true, |account| account.as_str() == credentials.email)
            })
            .map(|credentials| credentials.email)
            .collect::<Vec<_>>();

        let account = match &scope {
            AccountScope::One(account) => account.to_string(),
            AccountScope::All if !body_fetchers.is_empty() => {
                format!("All accounts ({})", body_fetchers.accounts().count())
            }
            AccountScope::All => "offline".into(),
        };
        let outbox = store.outbox_summary(&scope).await?;
        let status = StatusLine {
            account,
            sync: if !needs_reauth.is_empty() && body_fetchers.is_empty() {
                SyncState::AuthExpired
            } else if !body_fetchers.is_empty() {
                SyncState::Ok
            } else {
                SyncState::Offline
            },
            hint: "j/k:nav  e:archive  c:compose  /:search  q:quit".into(),
            needs_reauth,
            outbox,
        };

        Ok(Self {
            keymap,
            store,
            scope,
            dispatcher,
            body_fetchers,
            hyperlinks,
            search_events: None,
            remote_search_task: None,
            view,
            status,
            chord_buffer: String::new(),
            last_chord_continuations: Vec::new(),
            inbox_split: Split::Important,
            running: true,
        })
    }

    /// Build a `KeyContext` from current view state.
    pub fn key_context(&self) -> KeyContext {
        match &self.view {
            View::Inbox(v) => KeyContext {
                selection: v
                    .current_thread_id(self.inbox_split)
                    .map(|t| vec![t.as_str().to_string()])
                    .unwrap_or_default(),
                current_thread: v
                    .current_thread_id(self.inbox_split)
                    .map(|t| t.as_str().to_string()),
                current_message: None,
                current_draft: None,
            },
            View::Thread(v) => KeyContext {
                selection: vec![v.thread_id.as_str().to_string()],
                current_thread: Some(v.thread_id.as_str().to_string()),
                current_message: v.current_message_id().map(|m| m.as_str().to_string()),
                current_draft: None,
            },
            View::Composer(v) => KeyContext {
                selection: Vec::new(),
                current_thread: None,
                current_message: None,
                current_draft: Some(v.draft_id.as_str().to_string()),
            },
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
    pub fn visible_threads(&self, split: Split) -> Vec<&ThreadSummary> {
        if self.label.as_str() == "INBOX" {
            self.threads
                .iter()
                .filter(|thread| split_of(&thread.label_ids) == split)
                .collect()
        } else {
            self.threads.iter().collect()
        }
    }

    pub fn current_thread(&self, split: Split) -> Option<&ThreadSummary> {
        self.visible_threads(split).get(self.selected).copied()
    }
    pub fn current_thread_id(&self, split: Split) -> Option<&ThreadId> {
        self.current_thread(split).map(|t| &t.id)
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
    let backend = crate::backend::HyperlinkBackend::new(stdout, app.hyperlinks.clone());
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
    terminal: &mut Terminal<crate::backend::HyperlinkBackend<io::Stdout>>,
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
            for account in report.needs_reauth {
                let email = account.to_string();
                if !app.status.needs_reauth.contains(&email) {
                    app.status.needs_reauth.push(email);
                }
            }
            app.status.sync = if report.succeeded > 0 {
                SyncState::Ok
            } else if !app.status.needs_reauth.is_empty() {
                SyncState::AuthExpired
            } else {
                SyncState::Offline
            };
            if report.succeeded > 0 {
                refresh_visible_inbox(app).await;
            }
            match app.store.outbox_summary(&app.scope).await {
                Ok(summary) => app.status.outbox = summary,
                Err(error) => warn!(%error, "refreshing outbox summary failed"),
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
        .current_thread(app.inbox_split)
        .map(|thread| (thread.account_id.clone(), thread.id.clone()));
    match load_inbox(&app.store, &app.scope, label.as_str()).await {
        Ok(mut inbox) => {
            if let Some((account, id)) = selected {
                inbox.selected = inbox
                    .visible_threads(app.inbox_split)
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
    if thread_scroll_key(app, &k) {
        return;
    }
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
        Resolution::AdapterAction(action) => {
            app.chord_buffer.clear();
            app.last_chord_continuations.clear();
            execute_adapter_action(app, &action);
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
                match app.keymap.resolve(app.current_mode(), &chord, &ctx) {
                    Resolution::Action(action) => execute_action(app, action).await,
                    Resolution::AdapterAction(action) => execute_adapter_action(app, &action),
                    Resolution::Prefix(_) | Resolution::Unbound => {}
                }
            }
        }
    }
}

fn execute_adapter_action(app: &mut App, action: &str) {
    let split = match action {
        "inbox_split_important" => Split::Important,
        "inbox_split_other" => Split::Other,
        "inbox_split_newsletters" => Split::Newsletters,
        _ => return,
    };
    let View::Inbox(inbox) = &mut app.view else {
        return;
    };
    if inbox.label.as_str() == "INBOX" {
        app.inbox_split = split;
        inbox.selected = inbox
            .selected
            .min(inbox.visible_threads(split).len().saturating_sub(1));
        inbox.viewport_top = inbox.viewport_top.min(inbox.selected);
    }
}

fn thread_scroll_key(app: &mut App, key: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyCode;
    let View::Thread(thread) = &mut app.view else {
        return false;
    };
    match key.code {
        KeyCode::PageDown => {
            thread.scroll = thread.scroll.saturating_add(20);
            true
        }
        KeyCode::PageUp => {
            thread.scroll = thread.scroll.saturating_sub(20);
            true
        }
        KeyCode::Home => {
            thread.scroll = 0;
            true
        }
        KeyCode::End => {
            thread.scroll = u16::MAX;
            true
        }
        _ => false,
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
            if matches!(app.view, View::Composer(_)) {
                save_composer(app).await;
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
        Action::ComposeNew | Action::Reply { .. } | Action::Forward { .. } => {
            open_composer(app, action.clone()).await;
            return;
        }
        Action::SaveDraft { .. } if matches!(app.view, View::Composer(_)) => {
            save_composer(app).await;
            return;
        }
        Action::SendDraft { .. } if matches!(app.view, View::Composer(_)) => {
            send_composer(app).await;
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
                    v.selected = v
                        .selected
                        .min(v.visible_threads(app.inbox_split).len().saturating_sub(1));
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

async fn open_composer(app: &mut App, action: Action) {
    match app.dispatcher.execute(action).await {
        Ok(outcome) => match draft_from_outcome(outcome) {
            Ok(draft) => {
                let previous_view = std::mem::replace(&mut app.view, empty_inbox_view());
                app.view = View::Composer(ComposerView::from_draft(draft, previous_view));
            }
            Err(error) => warn!(%error, "dispatch returned an invalid draft"),
        },
        Err(error) => warn!(%error, "dispatch failed"),
    }
}

async fn save_composer(app: &mut App) -> bool {
    let Some(action) = composer_save_action(&app.view) else {
        return false;
    };
    match app.dispatcher.execute(action).await {
        Ok(outcome) => {
            debug!(?outcome, "draft saved");
            true
        }
        Err(error) => {
            warn!(%error, "dispatch failed");
            false
        }
    }
}

async fn send_composer(app: &mut App) {
    if !save_composer(app).await {
        return;
    }
    let View::Composer(composer) = &app.view else {
        return;
    };
    let action = Action::SendDraft {
        draft_id: composer.draft_id.clone(),
    };
    match app.dispatcher.execute(action).await {
        Ok(outcome) => {
            debug!(?outcome, "draft queued to send");
            let View::Composer(composer) = std::mem::replace(&mut app.view, empty_inbox_view())
            else {
                return;
            };
            app.view = *composer.previous_view;
        }
        Err(error) => warn!(%error, "dispatch failed"),
    }
}

fn composer_save_action(view: &View) -> Option<Action> {
    let View::Composer(composer) = view else {
        return None;
    };
    Some(Action::SaveDraft {
        draft_id: composer.draft_id.clone(),
        patch: DraftPatch {
            to: Some(split_recipients(&composer.to)),
            cc: Some(split_recipients(&composer.cc)),
            subject: Some(composer.subject.clone()),
            body_md: Some(composer.body.clone()),
            ..DraftPatch::default()
        },
    })
}

fn split_recipients(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|recipient| !recipient.is_empty())
        .map(str::to_string)
        .collect()
}

fn draft_from_outcome(outcome: ActionOutcome) -> Result<Draft> {
    let data = outcome.data.context("missing action outcome data")?;
    let draft = data
        .get("draft")
        .context("missing draft in action outcome")?
        .clone();
    serde_json::from_value(draft).context("decoding draft from action outcome")
}

fn empty_inbox_view() -> View {
    View::Inbox(InboxView {
        label: LabelId::new("INBOX"),
        threads: Vec::new(),
        selected: 0,
        viewport_top: 0,
    })
}

impl ComposerView {
    fn from_draft(draft: Draft, previous_view: View) -> Self {
        Self {
            draft_id: draft.id,
            to: draft.to.join(", "),
            cc: draft.cc.join(", "),
            subject: draft.subject,
            body: draft.body_md,
            field: ComposerField::To,
            previous_view: Box::new(previous_view),
        }
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
    app.view = View::Thread(Box::new(ThreadView {
        thread_id: id,
        summary,
        messages,
        body_renderer: EmailBodyRenderer::default(),
        scroll: 0,
        selected_message: 0,
    }));
}

fn advance_selection(app: &mut App, delta: i32) {
    match &mut app.view {
        View::Inbox(v) => {
            let len = v.visible_threads(app.inbox_split).len();
            if len == 0 {
                return;
            }
            let next = (v.selected as i32 + delta).clamp(0, len as i32 - 1) as usize;
            v.selected = next;
        }
        View::Thread(v) => {
            if !v.messages.is_empty() {
                let next = (v.selected_message as i32 + delta).clamp(0, v.messages.len() as i32 - 1)
                    as usize;
                v.selected_message = next;
                v.scroll = 0;
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use mach_core::ids::AccountId;

    use super::*;

    fn draft() -> Draft {
        Draft {
            account_id: AccountId::new("me@example.com"),
            id: DraftId::new("draft-1"),
            gmail_draft_id: None,
            thread_id: Some(ThreadId::new("thread-1")),
            in_reply_to_message_id: Some(mach_core::ids::MessageId::new("message-1")),
            to: vec!["Alice <alice@example.com>".into(), "bob@example.com".into()],
            cc: vec!["carol@example.com".into()],
            bcc: Vec::new(),
            subject: "Re: Plans".into(),
            body_md: "Sounds good".into(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn draft_outcome_populates_composer_buffer() {
        let draft = draft();
        let outcome = ActionOutcome {
            action_name: "reply".into(),
            op_id: None,
            changed_threads: Vec::new(),
            changed_drafts: vec![draft.id.clone()],
            data: Some(serde_json::json!({ "draft": draft })),
            message: String::new(),
        };

        let composer =
            ComposerView::from_draft(draft_from_outcome(outcome).unwrap(), empty_inbox_view());

        assert_eq!(composer.draft_id.as_str(), "draft-1");
        assert_eq!(composer.to, "Alice <alice@example.com>, bob@example.com");
        assert_eq!(composer.cc, "carol@example.com");
        assert_eq!(composer.subject, "Re: Plans");
        assert_eq!(composer.body, "Sounds good");
    }

    #[test]
    fn composer_save_uses_the_entire_editing_buffer() {
        let mut composer = ComposerView::from_draft(draft(), empty_inbox_view());
        composer.to = " alice@example.com, bob@example.com, ".into();
        composer.cc = " , carol@example.com ".into();
        composer.subject = "Updated".into();
        composer.body = "New body".into();

        let Action::SaveDraft { draft_id, patch } =
            composer_save_action(&View::Composer(composer)).unwrap()
        else {
            panic!("expected save draft action");
        };

        assert_eq!(draft_id.as_str(), "draft-1");
        assert_eq!(patch.to.unwrap(), ["alice@example.com", "bob@example.com"]);
        assert_eq!(patch.cc.unwrap(), ["carol@example.com"]);
        assert_eq!(patch.subject.as_deref(), Some("Updated"));
        assert_eq!(patch.body_md.as_deref(), Some("New body"));
        assert!(patch.in_reply_to_message_id.is_none());
        assert!(patch.thread_id.is_none());
    }

    #[test]
    fn malformed_draft_outcome_is_rejected() {
        let outcome = ActionOutcome::empty("compose_new");
        assert!(draft_from_outcome(outcome).is_err());
    }
}
