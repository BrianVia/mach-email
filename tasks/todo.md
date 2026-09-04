# mach — Roadmap

Last updated: 2026-09-04.

Status legend: ✅ done · ⏳ pending · ❌ dropped.

Work is tracked as GitHub issues at https://github.com/BrianVia/mach-email/issues.
This file is the short map; the issues hold the detail.

## Shipped

### Foundation
- ✅ Workspace of 7 crates: core, store, gmail, tui, cli, mcp, desktop.
- ✅ One `Action` enum + `Dispatcher` as the single mutation point for every surface.
- ✅ SQLite (STRICT, WAL, FTS5), refinery migrations V1–V10.
- ✅ Multi-account: per-account credentials, cursors, outbox; unified and single-account scopes.

### Sync and reliability
- ✅ OAuth PKCE login; `invalid_grant` detected, account flagged `needs_reauth`, surfaced in CLI/TUI/desktop, skipped by sync loops (#6).
- ✅ Bootstrap (30 days) + incremental history sync + gap recovery, one shared thread-hydration path.
- ✅ Load older mail per label: desktop "Load older…", TUI `shift+l`, `mach sync --older` (#10).
- ✅ Durable outbox with backoff (1m/5m/30m/2h/12h), dead-letter after 5, `mach outbox list|retry`, "N unsynced / N failed" pill (#2).
- ✅ Echo suppression of our own label changes within 60s (#5).
- ✅ Optional Gmail Pub/Sub push sync with watch renewal; 60s poll kept as fallback (#17).
- ✅ Background sync in TUI and desktop every 60s.

### Triage
- ✅ Archive, trash, read/unread, star, labels, snooze, mute (#12).
- ✅ Undo/redo including trash and snooze via compound history entries (#1).
- ✅ Activity log (`mach log`, desktop Activity view, TUI `g v`) with undo of any completed op (#21).
- ✅ Split inbox: Important / Other / Newsletters, keys 1/2/3 (#13).
- ✅ Sidebar: Inbox, Starred, Sent, Drafts, Scheduled, Done, Snoozed, Muted, Trash, Spam, All Mail, user labels (#9).
- ✅ Unsubscribe from `List-Unsubscribe` (one-click POST, https, mailto) with confirm (#12).
- ✅ Search operators: `from: to: subject: label: is: has: newer_than: older_than:` (#4).

### Reading
- ✅ Sanitized HTML; remote images blocked by default with per-message show and per-sender allowlist (#3).
- ✅ Attachments listed per message; download to ~/Downloads from desktop and TUI (#8).
- ✅ Calendar invites parsed from ICS; accept / tentative / decline sends a METHOD:REPLY (#20).
- ✅ Copy thread as Markdown; links open in the system browser.

### Compose
- ✅ Compose, reply, reply-all, forward with quoting and RFC threading headers.
- ✅ To / Cc / Bcc, per-account signatures and snippets from `config.toml` (#7, #14).
- ✅ File attachments on send (#8).
- ✅ Send later with presets and a Scheduled folder with cancel (#16).
- ✅ Draft persistence and real send through the outbox.

### Surfaces
- ✅ TUI (ratatui), desktop (Tauri 2 + Svelte 5), CLI (`mach do`), MCP server.
- ✅ MCP high-level tools: `inbox_overview`, `read_thread`, `find_threads`, `draft_reply`, `daily_digest` (#18).
- ✅ Desktop notifications on new mail (#15).
- ✅ Cmd+K palette, chord keymap with user overrides.

## Open

- ⏳ #19 Local AI triage at sync time (needs a runtime decision and a review UI).
- ⏳ #22 Tauri Mobile build (touch-first shell, signing, background fetch).
- ⏳ #23 IMAP backend behind the `MailStore` seam.

## Decisions locked

- Default keymap follows Superhuman; users override per binding in `~/.config/mach/keymap.toml`.
- Credentials: one file per account under the OS app-data `accounts/` dir, mode 0600.
- One SQLite DB shared by all surfaces (WAL).
- Virtual labels (DONE, SNOOZED, MUTED, ALL, SCHEDULED) are derived in the store, never stored.
- Adapters (CLI, TUI, desktop, MCP) never rebuild domain orchestration; they call the dispatcher or one `mach-gmail` entry point.
