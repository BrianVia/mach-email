# mach — Roadmap

Last updated: 2026-05-14.

Status legend: ✅ done · 🔄 in progress · ⏳ pending · ❌ dropped.

## Shipped (v0.1.0)

### Foundation
- ✅ **Workspace skeleton** — 7 crates + CLI stub.
- ✅ **Action enum + Dispatcher** — single mutation point across surfaces.
- ✅ **SQLite store** — STRICT tables, WAL, FTS5, refinery migrations.

### Gmail integration
- ✅ **OAuth installed-app flow** — PKCE, loopback redirect, file-based credentials (`~/Library/Application Support/com.via.mach/credentials.json`, mode 0600).
- ✅ **HTTP client** — gzip, rustls, proactive token refresh, 401 retry.
- ✅ **Bootstrap** — snapshots historyId, fetches labels + last-30-days threads (10x parallel).
- ✅ **Body backfill (Milestone P)** — lazy `format=full` fetch on first thread open, MIME walker handles padded/unpadded base64url, falls back to `html2text` for HTML-only messages.
- ✅ **Mutating endpoints** — `threads.modify`, `threads.trash`, `messages.send`.
- ✅ **Outbox drain worker** — replays optimistic local mutations to Gmail.
- ✅ **Incremental sync** — `users.history.list` with messageAdded/Deleted/labelAdded/labelRemoved events.
- ✅ **Gap recovery** — falls back to a 7-day re-bootstrap on 404 cursor-too-old, reconciles by `messageId` upsert.

### Milestones (against real Gmail)
- ✅ **A** — Auth round-trips. `mach auth login/status/logout` work.
- ✅ **B** — Bootstrap populates cache: 1039 threads / 1228 msgs / 40 labels in ~22s, 0 failures.
- ✅ **C** — Real reads. FTS5 search, `open_thread` with full bodies, hyphenated terms fixed via phrase-quoting.
- ✅ **P** — Body backfill: first open ~280ms (multi-msg), second open ~14ms (cache hit), FTS5 trigger reindexes on body UPDATE.
- ✅ **D** — `mach-tui` dogfoodable inbox: ratatui app, inbox/thread/composer/search views, Superhuman keymap, chord prefixes, user keymap.toml override, ~35 workspace tests including keymap golden files and key-event normalization.
- ✅ **E** — `mach-desktop` Tauri 2.x + SolidJS + Tailwind v4 + Motion One; same Action JSON dispatched via `invoke`, same keymap TOML shared, spring-eased selection, FLIP'd archive, backdrop-blur composer, chord overlay. Builds: 104KB JS / 16KB CSS.

### Multi-surface
- ✅ **CLI** — `mach do '{...}'` dispatches Action JSON.
- ✅ **TUI** — `mach` launches ratatui app.
- ✅ **Desktop** — `mach-desktop` launches Tauri app.
- ✅ **MCP** — `mach mcp` speaks JSON-RPC over stdio; one `mach` tool with the full Action schema. Tested handshake + tools/list + tools/call.

### Polish
- ✅ **Undo/redo** — 20-deep in-memory stack on `Dispatcher`. Inverse computed before mutation; new mutation clears redo. Tests: round-trip + empty-stack + redo-invalidation.
- ✅ **Snooze sweeper** — `mach sync` scans `MACH/Snoozed/<rfc3339>` labels, un-snoozes when due.
- ✅ **Send-later timer** — `mach sync` fires drafts whose `send_later.send_at` is past.
- ✅ **`mach doctor`** — env health (paths, cursor, creds, account, outbox); `--simulate-gap` wipes cursor for gap-recovery regression.

## Out of scope for v0.1 (open ideas)

These weren't requested as tasks but are obvious next steps. Reorganized as
they emerge from feedback. They aren't blocking the v0.1 ship.

### Sync robustness
- ⏳ **Echo suppression**: tag history events whose net effect matches a recent (<60s) own `op_id` and drop them. Prevents UI flicker after archive.
- ⏳ **Auto re-auth on `invalid_grant`**: detect refresh failure → blow away `credentials.json` → auto-pop browser for re-login. Surface a "refresh token expires in <N>h" status pill 18h before due.
- ⏳ **Periodic sync in TUI/Desktop**: background tokio task that fires `mach sync` equivalent every 60s; right now the user runs `mach sync` manually.
- ⏳ **List-Unsubscribe**: parse the header → `unsubscribe` action.

### Composer wiring
- ⏳ **Compose → SendDraft pipeline**: TUI/Desktop composer state isn't yet serialized into a Draft record. Send-later infra exists but no UI fires it.
- ⏳ **Reply/Forward pre-population**: actions exist but bodies/headers aren't auto-prefixed.
- ⏳ **Snippets** (`;`): templated message fragments.
- ⏳ **HTML→Markdown round-trip for replies**.

### UX
- ⏳ **Multi-account**.
- ⏳ **Pub/Sub watch** for push notifications (vs polling).
- ⏳ **Calendar / contacts** integration.
- ⏳ **Theme system**: ship 2-3 themes, not just one light + dark.
- ⏳ **Mouse support** in TUI.
- ⏳ **Mute** (`Shift+M`).
- ⏳ **Hot-reload keymap.toml on file change** via `notify`.
- ⏳ **System notifications** on new mail (Desktop).
- ⏳ **Drag-and-drop attachments**.
- ⏳ **Tauri Mobile** build (iOS/Android).

### Search
- ⏳ **Gmail-operator awareness** (`is:unread`, `from:foo`, `has:attachment`) — currently we quote everything as a literal phrase.
- ⏳ **Fallback to remote `users.messages.list?q=` for older mail** beyond the 30-day cache window.

### Bug fixes discovered along the way
- ✅ **keyring 3.x silently drops writes** on unsigned macOS binaries — switched to file storage at `credentials.json` (mode 0600). Documented in `crates/mach-gmail/src/credentials.rs`.
- ✅ **Gmail body base64 has padding sometimes** — original `URL_SAFE_NO_PAD` rejected it. Switched to `Indifferent` padding mode.
- ✅ **FTS5 interpreted `-` as NOT operator** on hyphenated user queries — wrapped queries in `"…"` for literal-phrase matching.
- ✅ **`open_thread` returned empty data** — was a TUI navigation action only; now populates `data: { thread, messages }` for CLI/MCP.
- ✅ **Doctest treated ASCII layout diagram as Rust code** — added ` ```text ` fence.
- ✅ **Duplicate `r` binding in default keymap** — `refresh` moved to `ctrl+r`.

## Decisions locked

- Desktop frontend: **SolidJS** (vs React; the VDOM cost matters at 16ms).
- Default keymap: **Superhuman bindings** lifted verbatim; user can override per-binding.
- Storage: file-backed `credentials.json` at OS data dir, mode 0600. (Keyring 3.x silently fails on unsigned macOS binaries.)
- Same SQLite DB shared across all surfaces (WAL handles concurrent processes).
- One unified `Action` enum drives TUI / CLI / Desktop / MCP. Adding a feature = one variant + one dispatcher arm + one keymap binding.
