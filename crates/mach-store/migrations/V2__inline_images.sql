-- Inline images: image/* MIME parts referenced from the message's HTML via
-- `cid:`. We don't store the bytes here (they live on disk after the first
-- fetch); we just remember the (content_id → gmail attachment_id) mapping so
-- the desktop frontend can ask for the data later.
--
-- Stored as JSON on `messages` rather than a separate table — a message
-- typically has 0-5 inline images, so the locality wins over the join.

ALTER TABLE messages ADD COLUMN inline_images_json TEXT;
