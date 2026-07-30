-- Multi-account groundwork. Every account-scoped table gains an
-- `account_id` column. Existing rows backfill to the user's only
-- account (`brian.a.via@gmail.com`) via the DEFAULT clause; later
-- inserts will pass their own account_id explicitly.
--
-- SQLite's STRICT mode accepts ALTER TABLE ADD COLUMN with a literal
-- DEFAULT — no table rewrite, O(1).
--
-- Index strategy: every "list threads in a label" / "list messages for
-- a thread" query is account-scoped, so the indexes lead with
-- `account_id` to keep them selective.

ALTER TABLE threads     ADD COLUMN account_id TEXT NOT NULL DEFAULT 'brian.a.via@gmail.com';
ALTER TABLE messages    ADD COLUMN account_id TEXT NOT NULL DEFAULT 'brian.a.via@gmail.com';
ALTER TABLE labels      ADD COLUMN account_id TEXT NOT NULL DEFAULT 'brian.a.via@gmail.com';
ALTER TABLE drafts      ADD COLUMN account_id TEXT NOT NULL DEFAULT 'brian.a.via@gmail.com';
ALTER TABLE send_later  ADD COLUMN account_id TEXT NOT NULL DEFAULT 'brian.a.via@gmail.com';
ALTER TABLE outbox      ADD COLUMN account_id TEXT NOT NULL DEFAULT 'brian.a.via@gmail.com';

CREATE INDEX idx_threads_account_last
  ON threads(account_id, last_message_at DESC);

CREATE INDEX idx_messages_account
  ON messages(account_id);

CREATE INDEX idx_outbox_account_pending
  ON outbox(account_id, state, id) WHERE state = 'pending';

-- Labels are already PRIMARY KEY(id) but id is Gmail-scoped; in
-- practice two accounts have overlapping system labels (INBOX, SENT,
-- etc.). Move primary key to (account_id, id) so each account has its
-- own row even for shared system labels.
--
-- SQLite doesn't allow ALTER PRIMARY KEY in place. Replicate via the
-- standard rebuild-and-swap dance.
CREATE TABLE labels_new (
  account_id   TEXT    NOT NULL DEFAULT 'brian.a.via@gmail.com',
  id           TEXT    NOT NULL,
  name         TEXT    NOT NULL,
  type         TEXT    NOT NULL,
  color        TEXT,
  unread_count INTEGER,
  total_count  INTEGER,
  PRIMARY KEY (account_id, id)
) STRICT;

INSERT INTO labels_new (account_id, id, name, type, color, unread_count, total_count)
  SELECT account_id, id, name, type, color, unread_count, total_count
  FROM labels;

DROP TABLE labels;
ALTER TABLE labels_new RENAME TO labels;
