# mach

`mach` is a local-first, keyboard-driven Gmail client inspired by Superhuman.
It provides a terminal UI, JSON CLI, Tauri desktop application, and MCP
server over one Rust core.

The application is currently best treated as a personal read-and-triage
client. Unified multi-account inbox browsing, per-account filtering, search,
archive, trash, read/unread, starring,
labels, snooze, lazy body fetching, undo/redo, manual Gmail sync, and
account-correct mutation routing are wired. Compose, reply, forward, draft
persistence, send-later, and automatic background sync are not yet complete.

## Architecture

- `mach-core`: shared actions, dispatcher, and storage interfaces
- `mach-store`: SQLite cache, FTS5 search, migrations, and durable outbox
- `mach-gmail`: OAuth, Gmail HTTP client, MIME extraction, sync, and outbox drain
- `mach-tui`: Ratatui interface
- `mach-cli`: `mach` command and composition root
- `mach-mcp`: JSON-RPC MCP server over stdio
- `mach-desktop`: Tauri 2 backend and SolidJS frontend

Mail data is cached in the platform application-data directory. Mutations
update SQLite optimistically and atomically enqueue a durable Gmail operation.
Run `mach sync` to drain those operations and pull Gmail history.

## Prerequisites

- Stable Rust with `rustfmt` and `clippy`
- Bun 1.x for the desktop frontend
- A Google Cloud desktop OAuth client with the Gmail API enabled

Tauri requires native WebKit dependencies on Linux. On Debian or Ubuntu:

```sh
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

See the [official Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)
for other platforms.

## Setup

```sh
cp .env.example .env
# Fill in MACH_GOOGLE_CLIENT_ID and MACH_GOOGLE_CLIENT_SECRET.

cargo run -p mach-cli -- auth login
cargo run -p mach-cli
```

Repeat `mach auth login` for each Gmail address. Logins are additive and a new
account is bootstrapped automatically. A later `mach sync` also bootstraps any
account missing its initial cursor. The default TUI merges every account into
one time-sorted inbox and shows the owning address and date on each row:

```sh
mach                         # unified inbox
mach --account me@work.com   # one account
mach sync                    # sync every account
mach sync --account me@work.com
mach sync --bootstrap        # explicitly rebuild every account
mach auth status
mach auth logout --account me@work.com
```

Useful non-interactive commands:

```sh
cargo run -p mach-cli -- doctor
cargo run -p mach-cli -- sync
cargo run -p mach-cli -- do '{"kind":"search","query":"invoice","limit":10}'
cargo run -p mach-cli -- mcp
```

Desktop development:

```sh
cd crates/mach-desktop/ui
bun install --frozen-lockfile
cd ..
cargo tauri dev
```

The desktop build hook runs the frontend build automatically.

## Validation

```sh
cargo fmt --all -- --check
cargo clippy --workspace --exclude mach-desktop --all-targets -- -D warnings
cargo test --workspace --exclude mach-desktop --all-targets

cd crates/mach-desktop/ui
bun install --frozen-lockfile
bun test
bun run build
cd ../../..
cargo check -p mach-desktop
```

Tests that contact Gmail are intentionally not run automatically. Before using
sync changes against a primary mailbox, exercise them against a test account
and preserve a copy of the local SQLite database.

## Data and privacy

OAuth credentials are stored as one file per account under `accounts/` in the
platform application-data directory with mode `0600` on Unix. A historical
single `credentials.json` is migrated automatically. Email bodies and metadata
are cached locally in SQLite. The desktop renderer sanitizes HTML, but currently
loads remote images; opening a message can therefore notify tracking servers.

## License

MIT. See [LICENSE](LICENSE).
