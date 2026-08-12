CREATE TABLE send_later_v5 (
  account_id TEXT    NOT NULL,
  id         TEXT    NOT NULL,
  draft_id   TEXT    NOT NULL,
  send_at    INTEGER NOT NULL,
  state      TEXT    NOT NULL DEFAULT 'scheduled',
  PRIMARY KEY (account_id, id)
) STRICT;

INSERT INTO send_later_v5 (account_id, id, draft_id, send_at, state)
SELECT account_id, id, draft_id, send_at, state FROM send_later;

DROP TABLE send_later;
ALTER TABLE send_later_v5 RENAME TO send_later;

CREATE INDEX idx_send_later_due ON send_later(account_id, send_at)
  WHERE state = 'scheduled';
