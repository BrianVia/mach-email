-- Finish the V3 multi-account groundwork. Gmail IDs are scoped to an account,
-- so every Gmail-owned identity uses (account_id, id) as its durable key.
-- Existing V3 rows are marked __legacy__; the runtime claims them for the
-- sole stored credential before adding another account.

DROP TRIGGER messages_ai;
DROP TRIGGER messages_ad;
DROP TRIGGER messages_au;
DROP TABLE messages_fts;

CREATE TABLE threads_v4 (
  account_id       TEXT    NOT NULL,
  id               TEXT    NOT NULL,
  history_id       INTEGER NOT NULL,
  snippet          TEXT    NOT NULL DEFAULT '',
  subject          TEXT    NOT NULL DEFAULT '',
  participants_json TEXT   NOT NULL DEFAULT '[]',
  last_message_at  INTEGER NOT NULL,
  message_count    INTEGER NOT NULL,
  unread           INTEGER NOT NULL,
  starred          INTEGER NOT NULL,
  label_ids_json   TEXT    NOT NULL DEFAULT '[]',
  updated_at       INTEGER NOT NULL,
  PRIMARY KEY (account_id, id)
) STRICT;

INSERT INTO threads_v4
SELECT CASE WHEN account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE account_id END,
       id, history_id, snippet, subject, participants_json, last_message_at,
       message_count, unread, starred, label_ids_json, updated_at
FROM threads;

CREATE TABLE messages_v4 (
  account_id        TEXT    NOT NULL,
  id                TEXT    NOT NULL,
  thread_id         TEXT    NOT NULL,
  history_id        INTEGER NOT NULL,
  internal_date     INTEGER NOT NULL,
  from_addr         TEXT    NOT NULL DEFAULT '',
  to_addrs          TEXT    NOT NULL DEFAULT '[]',
  cc_addrs          TEXT,
  bcc_addrs          TEXT,
  subject           TEXT    NOT NULL DEFAULT '',
  snippet           TEXT    NOT NULL DEFAULT '',
  raw_size          INTEGER,
  body_plain        TEXT,
  body_html         TEXT,
  headers_json      TEXT,
  label_ids_json    TEXT    NOT NULL DEFAULT '[]',
  fetched_full      INTEGER NOT NULL DEFAULT 0,
  inline_images_json TEXT,
  PRIMARY KEY (account_id, id),
  FOREIGN KEY (account_id, thread_id)
    REFERENCES threads_v4(account_id, id) ON DELETE CASCADE ON UPDATE CASCADE
) STRICT;

INSERT INTO messages_v4
SELECT CASE WHEN account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE account_id END,
       id, thread_id, history_id, internal_date, from_addr, to_addrs, cc_addrs,
       bcc_addrs, subject, snippet, raw_size, body_plain, body_html, headers_json,
       label_ids_json, fetched_full, inline_images_json
FROM messages;

CREATE TABLE attachments_v4 (
  account_id TEXT NOT NULL,
  id          TEXT NOT NULL,
  message_id  TEXT NOT NULL,
  filename    TEXT,
  mime_type   TEXT,
  size        INTEGER,
  cached_path TEXT,
  PRIMARY KEY (account_id, id),
  FOREIGN KEY (account_id, message_id)
    REFERENCES messages_v4(account_id, id) ON DELETE CASCADE ON UPDATE CASCADE
) STRICT;

INSERT INTO attachments_v4
SELECT CASE WHEN m.account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE m.account_id END,
       a.id, a.message_id, a.filename, a.mime_type, a.size, a.cached_path
FROM attachments a
JOIN messages m ON m.id = a.message_id;

CREATE TABLE labels_v4 (
  account_id   TEXT    NOT NULL,
  id           TEXT    NOT NULL,
  name         TEXT    NOT NULL,
  type         TEXT    NOT NULL,
  color        TEXT,
  unread_count INTEGER,
  total_count  INTEGER,
  PRIMARY KEY (account_id, id)
) STRICT;

INSERT INTO labels_v4
SELECT CASE WHEN account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE account_id END,
       id, name, type, color, unread_count, total_count
FROM labels;

CREATE TABLE drafts_v4 (
  account_id             TEXT    NOT NULL,
  id                     TEXT    NOT NULL,
  gmail_draft_id         TEXT,
  thread_id              TEXT,
  in_reply_to_message_id TEXT,
  to_addrs               TEXT    NOT NULL DEFAULT '[]',
  cc_addrs               TEXT    NOT NULL DEFAULT '[]',
  bcc_addrs              TEXT    NOT NULL DEFAULT '[]',
  subject                TEXT    NOT NULL DEFAULT '',
  body_md                TEXT    NOT NULL DEFAULT '',
  updated_at             INTEGER NOT NULL,
  state                  TEXT    NOT NULL DEFAULT 'draft',
  PRIMARY KEY (account_id, id),
  FOREIGN KEY (account_id, thread_id)
    REFERENCES threads_v4(account_id, id) ON DELETE SET NULL ON UPDATE CASCADE,
  FOREIGN KEY (account_id, in_reply_to_message_id)
    REFERENCES messages_v4(account_id, id) ON DELETE SET NULL ON UPDATE CASCADE
) STRICT;

INSERT INTO drafts_v4
SELECT CASE WHEN account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE account_id END,
       id, gmail_draft_id, thread_id, in_reply_to_message_id, to_addrs, cc_addrs,
       bcc_addrs, subject, body_md, updated_at, state
FROM drafts;

CREATE TABLE send_later_v4 (
  account_id TEXT    NOT NULL,
  id         TEXT    NOT NULL,
  draft_id   TEXT    NOT NULL,
  send_at    INTEGER NOT NULL,
  state      TEXT    NOT NULL DEFAULT 'scheduled',
  PRIMARY KEY (account_id, id),
  FOREIGN KEY (account_id, draft_id)
    REFERENCES drafts_v4(account_id, id) ON DELETE CASCADE ON UPDATE CASCADE
) STRICT;

INSERT INTO send_later_v4
SELECT CASE WHEN account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE account_id END,
       id, draft_id, send_at, state
FROM send_later;

CREATE TABLE outbox_v4 (
  id           INTEGER PRIMARY KEY,
  account_id   TEXT    NOT NULL,
  op_id        TEXT    NOT NULL,
  op_kind      TEXT    NOT NULL,
  payload_json TEXT    NOT NULL,
  created_at   INTEGER NOT NULL,
  attempts     INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT,
  state        TEXT    NOT NULL DEFAULT 'pending'
) STRICT;

INSERT INTO outbox_v4
SELECT id,
       CASE WHEN account_id = 'brian.a.via@gmail.com' THEN '__legacy__' ELSE account_id END,
       op_id, op_kind, payload_json, created_at, attempts, last_error, state
FROM outbox;

CREATE TABLE sync_state_v4 (
  account_id          TEXT    PRIMARY KEY,
  history_id          INTEGER NOT NULL,
  last_full_sync_at   INTEGER,
  last_incremental_at INTEGER
) STRICT;

INSERT INTO sync_state_v4
SELECT CASE
         WHEN account_id IN ('default', 'brian.a.via@gmail.com') THEN '__legacy__'
         ELSE account_id
       END,
       MAX(history_id), MAX(last_full_sync_at), MAX(last_incremental_at)
FROM sync_state
GROUP BY CASE
           WHEN account_id IN ('default', 'brian.a.via@gmail.com') THEN '__legacy__'
           ELSE account_id
         END;

DROP TABLE attachments;
DROP TABLE messages;
DROP TABLE threads;
DROP TABLE labels;
DROP TABLE send_later;
DROP TABLE drafts;
DROP TABLE outbox;
DROP TABLE sync_state;

ALTER TABLE threads_v4 RENAME TO threads;
ALTER TABLE messages_v4 RENAME TO messages;
ALTER TABLE attachments_v4 RENAME TO attachments;
ALTER TABLE labels_v4 RENAME TO labels;
ALTER TABLE drafts_v4 RENAME TO drafts;
ALTER TABLE send_later_v4 RENAME TO send_later;
ALTER TABLE outbox_v4 RENAME TO outbox;
ALTER TABLE sync_state_v4 RENAME TO sync_state;

CREATE INDEX idx_threads_last_message ON threads(last_message_at DESC);
CREATE INDEX idx_threads_account_last ON threads(account_id, last_message_at DESC);
CREATE INDEX idx_threads_unread ON threads(unread, last_message_at DESC) WHERE unread = 1;
CREATE INDEX idx_messages_thread ON messages(account_id, thread_id, internal_date);
CREATE INDEX idx_messages_account ON messages(account_id);
CREATE INDEX idx_attachments_msg ON attachments(account_id, message_id);
CREATE INDEX idx_send_later_due ON send_later(account_id, send_at)
  WHERE state = 'scheduled';
CREATE INDEX idx_outbox_pending ON outbox(state, id) WHERE state = 'pending';
CREATE INDEX idx_outbox_account_pending ON outbox(account_id, state, id)
  WHERE state = 'pending';
CREATE INDEX idx_outbox_op_id ON outbox(op_id);

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

INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');
