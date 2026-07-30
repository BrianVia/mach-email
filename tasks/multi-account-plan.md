# Multi-account support — plan

> Implemented on 2026-07-30. The shipped design uses `AccountScope` for
> unified/single-account reads, composite SQLite keys, per-account credential
> files and Gmail clients, and account-bound cursors/outbox workers. This file
> remains as historical design context; details below may differ from the
> final implementation.

Goal: support **N Gmail accounts in one mach install** with both a
per-account toggle and an optional Unified pseudo-account view. Default
UX is account-toggle; Unified is a switchable second mode.

Total estimate: **~16h focused work** across 8 phases. Phase 1 (schema)
is the only piece with real migration risk; everything else is mechanical.

## UX decisions (locked)

- **Account picker**: a chip in the top-left of the title bar that opens
  a dropdown. Each item is `<avatar> <email> · <N unread>`. Plus
  `+ Add account` and `Unified` rows.
- **Why not a hamburger**: hamburgers are a mobile metaphor. Desktop has
  pixels to spare. Keep the chip; if we ever need workspace-level
  settings (sign out, preferences), they get their *own* affordance.
- **Compose-from**: auto-pick the account that owns the thread for
  reply/forward (this account_id is on the thread row); explicit picker
  for `compose_new`. Unified view forces explicit picker — no implicit
  default.
- **Search scope**: active account by default, all accounts on a
  modifier (e.g. `Ctrl+/`). v1 ships active-only; cross-account search
  is a polish item.
- **Limit**: 5 accounts in v1. Beyond that we'd need filter/grouping
  affordances we haven't designed yet.

## Architecture changes

### Storage shape

Per-row `account_id TEXT NOT NULL` on every account-scoped table:
`threads`, `messages`, `labels`, `outbox`, `drafts`, `send_later`,
`attachments` (via cascade from message). `sync_state.account_id`
already exists; we just stop hard-coding `'default'`.

Why per-row rather than per-database-file:
- Cross-account search wants one SQLite, one FTS5 index.
- Unified view is a `SELECT WHERE account_id IN (…)`.
- Indexes on `(account_id, last_message_at DESC)` keep the inbox query
  cheap.

### Credentials

`~/Library/Application Support/com.via.mach/accounts/<email>.json` —
one file per account, mode 0600. On boot, scan the dir.

One-time migration: if the legacy `credentials.json` exists, move it to
`accounts/<email>.json` using the email from the file itself.

### Gmail client + body fetcher

`GmailClientPool` indexed by email:
```rust
pub struct GmailClientPool {
    clients: HashMap<String, Arc<GmailClient>>,
}
impl GmailClientPool {
    fn client_for(&self, email: &str) -> Option<Arc<GmailClient>>;
}
```

`BodyFetcher` becomes `BodyFetcher::for_account(pool, account, store)`.

### Sync engine

Bootstrap / incremental / outbox drain become per-account. Run all
accounts in parallel via `tokio::spawn`. Each account has its own
history cursor (already true in the schema). Failures on one account
don't kill sync for the others.

### Dispatcher

Thread `account_id` from the stored thread row through every mutation.
For `compose_new`, the user picks; for everything else, the action's
target thread tells us. Outbox ops carry `account_id` so the worker
knows which client to use.

The Action enum stays unchanged; account routing happens at dispatch
time by reading from the store. *Exception*: `ComposeNew` gains an
optional `account: Option<String>` field with the user-picked
identity.

### UI changes

- **Top-left account chip** (replaces the empty 60px spacer next to the
  traffic lights). Click → dropdown.
- **Dropdown contents**: avatar + email + unread count per account;
  `Unified`; `+ Add account`.
- **Inbox row**: when in Unified, show a colored 4px tab on the left
  edge keyed by account (same hue as the account's avatar).
- **Reply/forward**: composer auto-selects the from-account based on the
  thread.
- **New compose from Unified**: composer opens with an account picker
  in the To row.

### CLI / MCP

- `mach auth login [--account <email>]` — first time adds, subsequent
  switches.
- `mach do --account <email> '{…}'` — defaults to primary if only one
  account exists.
- `mach sync --account <email>` — defaults to all accounts in parallel.
- `mach mcp` — exposes one tool per account *and* a unified tool, or
  one tool with an `account` argument. Probably the latter (less
  cognitive load for the agent).

## Phases (in order)

| # | What | Risk | Est. |
|---|---|---|---|
| 1 | **Schema V3** — `account_id` columns + indexes + backfill | M (migration on live DB) | 3-4h |
| 2 | **Credentials per-account** — `accounts/<email>.json` + legacy migration | S | 1h |
| 3 | **GmailClientPool + BodyFetcher::for_account** | M | 3-4h |
| 4 | **Sync engine multi-account** — parallel per-account bootstrap/incremental/outbox-drain | M | 2h |
| 5 | **Dispatcher account-awareness** — read account_id from thread, route outbox ops | M | 2-3h |
| 6 | **Account chip UI + dropdown** | S | 2h |
| 7 | **Unified pseudo-account view** — `SELECT` union, row affordance, compose blocked unless picked | M | 2h |
| 8 | **CLI / MCP `--account` plumbing** | S | 1h |

## Risk register

- **Phase 1**: live DB has 1230 messages + 1039 threads. ALTER TABLE
  ADD COLUMN with `DEFAULT 'brian.a.via@gmail.com'` is safe (no
  table rewrite required in SQLite ≥ 3.35), but FTS5 triggers and
  partial indexes need to be re-examined. Test with `mach doctor`
  before/after.
- **Phase 3**: token refresh races if two accounts hit `invalid_grant`
  at once. Each `GmailClient` already has a `Mutex<StoredCredentials>`,
  so per-account refresh stays serialized; the pool just keeps separate
  Arc<GmailClient> instances.
- **Phase 5**: Action JSON is account-implicit today. Adding
  `account: Option<String>` to `ComposeNew` and inferring from thread
  elsewhere is backward-compatible — old clients still work with the
  single default account.
- **Phase 7**: Unified view's compose flow needs UX testing — picking
  a from-account inside the composer is unusual. We may end up
  defaulting to "the primary account" with a small dropdown next to
  the To field.

## Out of scope for v1

- Account groups / folders.
- Cross-account snooze rules.
- Per-account themes.
- More than 5 accounts.
- Calendar / Drive integration (different scope entirely).
