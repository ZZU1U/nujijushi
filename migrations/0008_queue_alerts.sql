ALTER TABLE channels ADD COLUMN empty_alert_handled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE queue ADD COLUMN unconfirmed_alert_handled INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_queue_unhandled_alert ON queue (id)
WHERE state = 'unknown' AND unconfirmed_alert_handled = 0;

CREATE TRIGGER queue_alert_rearm_on_insert
AFTER INSERT ON queue WHEN NEW.state = 'queued'
BEGIN
    UPDATE channels SET empty_alert_handled = 0 WHERE id = NEW.channel_id AND empty_alert_handled = 1;
END;

CREATE TRIGGER queue_alert_rearm_on_requeue
AFTER UPDATE OF state ON queue WHEN OLD.state <> 'queued' AND NEW.state = 'queued'
BEGIN
    UPDATE channels SET empty_alert_handled = 0 WHERE id = NEW.channel_id AND empty_alert_handled = 1;
END;

CREATE TRIGGER queue_alert_rearm_on_unconfirmed
AFTER UPDATE OF state ON queue WHEN OLD.state <> 'unknown' AND NEW.state = 'unknown'
BEGIN
    UPDATE queue SET unconfirmed_alert_handled = 0 WHERE id = NEW.id AND unconfirmed_alert_handled = 1;
END;
