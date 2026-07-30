-- Threads: primary unit of inbox display. Denormalized fields (participants,
-- unread, label_ids) are populated from messages on every write so the inbox
-- list paints with zero joins.
CREATE TABLE threads (
  id                TEXT    PRIMARY KEY,
  history_id        INTEGER NOT NULL,
  snippet           TEXT    NOT NULL DEFAULT '',
  subject           TEXT    NOT NULL DEFAULT '',
  participants_json TEXT    NOT NULL DEFAULT '[]',
  last_message_at   INTEGER NOT NULL,
  message_count     INTEGER NOT NULL,
  unread            INTEGER NOT NULL,
  starred           INTEGER NOT NULL,
  label_ids_json    TEXT    NOT NULL DEFAULT '[]',
  updated_at        INTEGER NOT NULL
) STRICT;
CREATE INDEX idx_threads_last_message ON threads(last_message_at DESC);
CREATE INDEX idx_threads_unread ON threads(unread, last_message_at DESC) WHERE unread = 1;

-- Messages: per-message detail. body_plain/body_html are NULL until the
-- thread is opened; fetched_full flips to 1 once a full fetch lands.
CREATE TABLE messages (
  id              TEXT    PRIMARY KEY,
  thread_id       TEXT    NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  history_id      INTEGER NOT NULL,
  internal_date   INTEGER NOT NULL,
  from_addr       TEXT    NOT NULL DEFAULT '',
  to_addrs        TEXT    NOT NULL DEFAULT '[]',
  cc_addrs        TEXT,
  bcc_addrs       TEXT,
  subject         TEXT    NOT NULL DEFAULT '',
  snippet         TEXT    NOT NULL DEFAULT '',
  raw_size        INTEGER,
  body_plain      TEXT,
  body_html       TEXT,
  headers_json    TEXT,
  label_ids_json  TEXT    NOT NULL DEFAULT '[]',
  fetched_full    INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX idx_messages_thread ON messages(thread_id, internal_date);

-- Full-text search over messages. External-content table — body kept once.
CREATE VIRTUAL TABLE messages_fts USING fts5(
  subject, from_addr, to_addrs, body_plain,
  content='messages',
  content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, subject, from_addr, to_addrs, body_plain)
  VALUES (new.rowid, new.subject, new.from_addr, new.to_addrs, new.body_plain);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_addr, to_addrs, body_plain)
  VALUES ('delete', old.rowid, old.subject, old.from_addr, old.to_addrs, old.body_plain);
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_addr, to_addrs, body_plain)
  VALUES ('delete', old.rowid, old.subject, old.from_addr, old.to_addrs, old.body_plain);
  INSERT INTO messages_fts(rowid, subject, from_addr, to_addrs, body_plain)
  VALUES (new.rowid, new.subject, new.from_addr, new.to_addrs, new.body_plain);
END;

CREATE TABLE labels (
  id           TEXT    PRIMARY KEY,
  name         TEXT    NOT NULL,
  type         TEXT    NOT NULL,
  color        TEXT,
  unread_count INTEGER,
  total_count  INTEGER
) STRICT;

CREATE TABLE attachments (
  id          TEXT PRIMARY KEY,
  message_id  TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  filename    TEXT,
  mime_type   TEXT,
  size        INTEGER,
  cached_path TEXT
) STRICT;
CREATE INDEX idx_attachments_msg ON attachments(message_id);

CREATE TABLE drafts (
  id                     TEXT    PRIMARY KEY,
  gmail_draft_id         TEXT,
  thread_id              TEXT,
  in_reply_to_message_id TEXT,
  to_addrs               TEXT    NOT NULL DEFAULT '[]',
  cc_addrs               TEXT    NOT NULL DEFAULT '[]',
  bcc_addrs              TEXT    NOT NULL DEFAULT '[]',
  subject                TEXT    NOT NULL DEFAULT '',
  body_md                TEXT    NOT NULL DEFAULT '',
  updated_at             INTEGER NOT NULL,
  state                  TEXT    NOT NULL DEFAULT 'draft'
) STRICT;

CREATE TABLE send_later (
  id       TEXT    PRIMARY KEY,
  draft_id TEXT    NOT NULL REFERENCES drafts(id),
  send_at  INTEGER NOT NULL,
  state    TEXT    NOT NULL DEFAULT 'scheduled'
) STRICT;
CREATE INDEX idx_send_later_due ON send_later(send_at) WHERE state = 'scheduled';

-- Outbox: durable record of pending remote-bound mutations. The sync engine
-- drains these in FIFO order. `op_id` is the deterministic dedup key used to
-- suppress history events that echo our own writes back.
CREATE TABLE outbox (
  id           INTEGER PRIMARY KEY,
  op_id        TEXT    NOT NULL,
  op_kind      TEXT    NOT NULL,
  payload_json TEXT    NOT NULL,
  created_at   INTEGER NOT NULL,
  attempts     INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT,
  state        TEXT    NOT NULL DEFAULT 'pending'
) STRICT;
CREATE INDEX idx_outbox_pending ON outbox(state, id) WHERE state = 'pending';
CREATE INDEX idx_outbox_op_id ON outbox(op_id);

CREATE TABLE sync_state (
  account_id           TEXT    PRIMARY KEY,
  history_id           INTEGER NOT NULL,
  last_full_sync_at    INTEGER,
  last_incremental_at  INTEGER
) STRICT;

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT
) STRICT;
