# Native macOS app

Goal: replace the Tauri desktop app on macOS with a native SwiftUI app that
keeps the Rust engine (`mach-core`, `mach-gmail`, `mach-store`) and reaches
feature parity with the Tauri app, then retire `crates/mach-desktop`.

Done means: `Mach.app` built from this repo is installed in `/Applications`
on the Mac, every row of the parity checklist below is checked on the real
app through its debug server, and the Tauri app is removed.

## Architecture

```
macos/                     SwiftPM package for the app (no .xcodeproj)
  Package.swift
  Sources/Mach/            SwiftUI app
  scripts/build-app.sh     builds Rust xcframework + swift build + assembles Mach.app
crates/mach-ffi/           Rust cdylib/staticlib exported to Swift with UniFFI
```

- **Bridge contract.** `mach-ffi` exposes one method per Tauri command in
  `crates/mach-desktop/src/commands.rs`, same name and argument names, returning
  the same JSON the Tauri command returns, as a `String`. Swift decodes it with
  `Codable` models that mirror `crates/mach-desktop/ui/src/lib/ipc.ts`. Events
  (`mail-synced`, `sync-status`) arrive through a UniFFI callback interface
  `on_event(name, payload_json)`. Attachments come back as bytes plus a MIME type.
  The engine owns its tokio runtime, the 15s per-account sync loop, and the
  outbox drain, exactly as `crates/mach-desktop/src/main.rs` does today.
- **Email HTML.** Rendered in a `WKWebView` with JavaScript disabled. `cid:` and
  attachment URLs go through a `WKURLSchemeHandler`. Remote images load by
  default; the per-sender block setting from the Tauri app is kept.
- **Keyboard.** The keymap stays `config/default-keymap.toml` plus the user's
  override, resolved by `mach_core::keymap` through the bridge, so chords and
  contexts behave identically.
- **Debug server.** The app listens on `127.0.0.1:$MACH_DEBUG_PORT` (default
  7366) with `/state`, `/key`, `/action`, and `/snapshot` (PNG of the app's own
  window, no screen-recording permission needed). This is how agents verify UI
  over SSH, since `screencapture` cannot run from an SSH session.
- **Bundle.** `Mach.app`, bundle id `com.via.mach.native` during development.
  It shares the Rust data directory (`~/Library/Application Support/com.via.mach`).

## Parity checklist

Inbox and navigation
- [ ] Sidebar system labels, collapsible user labels with unread counts, Activity
- [ ] Thread list with sender, subject, snippet, time, unread and starred state, account badge
- [ ] Inbox splits (Important / Other / Updates / Newsletters), keys 1 to 4
- [ ] Account menu in the header: All accounts or one account, with a clear button
- [ ] Live status pill, outbox pending / failed pill with retry
- [ ] Sign-in expired banner that re-signs in with `login_hint`; add account
- [ ] Load older (`shift+l`), scroll position, selection kept across refresh

Reader
- [ ] HTML and plain bodies, quoted text, links open in the browser
- [ ] Remote images on by default, per-sender block setting
- [ ] Image attachment thumbnails, full-screen viewer with arrows / Esc / Save
- [ ] Non-image attachments save to Downloads with a notice that reveals the file
- [ ] Unsubscribe (one-click POST and mailto), calendar invite accept / tentative / decline
- [ ] Copy thread as Markdown (`ctrl+shift+c`)

Actions and keyboard
- [ ] Every binding in `config/default-keymap.toml`, including `g` chords and the chord hint overlay
- [ ] Archive, trash, star, read / unread, mute, labels, snooze, undo / redo, Activity undo
- [ ] Command palette (`cmd+k`) with every command the Tauri palette lists
- [ ] Search view
- [ ] `cmd+r` syncs every account and reports the result

Compose
- [ ] New, reply, reply-all, forward with prefill and signatures
- [ ] Markdown body, snippets (`;name` expansion), attachments
- [ ] Save (`cmd+s`), autosave on close, send (`cmd+enter`), send later presets, Scheduled view
- [ ] Drafts sync to Gmail (engine already does this)

System
- [ ] Notifications for new mail through `UNUserNotificationCenter`; clicking one opens its thread
- [ ] Opening the window never jumps to a thread on its own
- [ ] Dark and light mode
- [ ] Menu bar menus with the main commands

## Standing orders for every agent

1. Never send email, archive, trash, mute, unsubscribe, or answer invites on real
   mail while testing. Drafts you create and then delete, and star / unstar or
   read / unread toggles that you restore, are allowed.
2. Never touch `/Applications/mach.app` (the Tauri app the owner uses daily) or
   kill `mach-desktop`. Install dev builds only to `~/Applications/Mach-dev/`.
3. Each agent works in its own branch `macos/<unit>` off `origin/main` and its own
   Mac checkout `~/Development/Personal/mach-native/<unit>`. Fetch over HTTPS
   (`git fetch https://github.com/BrianVia/mach-email <branch>`); push from Linux.
4. Build and test on `ssh dfinitiv-macbook-pro`. Prefix with
   `export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:/opt/homebrew/bin:$PATH"`.
5. Verify UI through the app's debug server and `/snapshot`, not by assuming.
   A clean compile is not done.
6. Comments only for a non-obvious why. No new dependencies without a reason in
   the report.
7. Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
8. Use your own Linux worktree (`git worktree add ../mach-<unit>-wt -b macos/<unit> origin/main`),
   never the shared checkout at `~/Development/Personal/mach-email`.
9. Remote shell steps that replace an installed app run under `set -euo pipefail`,
   never chain with `;`, and check the new bundle exists before deleting the old one.
   On 2026-10-23 a failed checkout followed by `;` deleted the owner's daily app.
