ALTER TABLE outbox ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;

DROP INDEX idx_outbox_account_pending;
CREATE INDEX idx_outbox_account_pending
  ON outbox(account_id, state, next_attempt_at, id) WHERE state = 'pending';
