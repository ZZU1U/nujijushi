ALTER TABLE sends ADD COLUMN delivery_target TEXT;
ALTER TABLE sends ADD COLUMN delivery_chat_id INTEGER;
ALTER TABLE sends ADD COLUMN delivery_username TEXT;

-- A matching live queue row can recover the destination for older deliveries.
UPDATE sends AS s
SET delivery_target = q.delivery_target,
    delivery_chat_id = q.delivery_chat_id,
    delivery_username = q.delivery_username
FROM queue q
WHERE s.ok = 1 AND q.id = s.queue_id AND q.channel_id = s.channel_id AND q.post_id = s.post_id
  AND q.state = 'sent' AND q.tg_message_ids = s.tg_message_ids AND q.sent_at <= s.sent_at;

CREATE INDEX idx_sends_post_delivered ON sends (post_id, channel_id, sent_at DESC)
WHERE ok = 1;
