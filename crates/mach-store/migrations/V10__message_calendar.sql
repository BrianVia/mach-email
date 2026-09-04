ALTER TABLE messages ADD COLUMN calendar_ics TEXT;
ALTER TABLE drafts ADD COLUMN calendar_reply_ics TEXT;

-- Rewalk already-cached MIME trees so existing invites get populated.
UPDATE messages SET fetched_full = 0;
