-- Preserve first-import provenance even when a later import adds attribution.
ALTER TABLE posts ADD COLUMN operator_import INTEGER CHECK (operator_import IN (0, 1));

-- Capture the delivery address before the Telegram call, including claims that
-- need manual confirmation after the channel configuration changes.
ALTER TABLE queue ADD COLUMN delivery_target TEXT;
ALTER TABLE queue ADD COLUMN delivery_chat_id INTEGER;
ALTER TABLE queue ADD COLUMN delivery_username TEXT;

CREATE TABLE reputation_subjects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    reviewable INTEGER NOT NULL CHECK (reviewable IN (0, 1)),
    verdict_points INTEGER,
    rediscovered INTEGER NOT NULL DEFAULT 0 CHECK (rediscovered IN (0, 1))
);

CREATE TABLE reputation_posts (
    post_id INTEGER PRIMARY KEY REFERENCES posts(id) ON DELETE CASCADE,
    subject_id INTEGER NOT NULL REFERENCES reputation_subjects(id)
);
CREATE INDEX reputation_posts_subject ON reputation_posts(subject_id, post_id);

CREATE TABLE reputation_keys (
    kind TEXT NOT NULL,
    identity TEXT NOT NULL,
    subject_id INTEGER NOT NULL REFERENCES reputation_subjects(id),
    PRIMARY KEY (kind, identity)
);

CREATE TABLE reputation_suggestions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_id INTEGER NOT NULL REFERENCES reputation_subjects(id),
    user_id INTEGER NOT NULL,
    review_member INTEGER NOT NULL CHECK (review_member IN (0, 1)),
    UNIQUE(subject_id, user_id)
);

CREATE TABLE reputation_publications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    subject_id INTEGER NOT NULL REFERENCES reputation_subjects(id),
    target TEXT NOT NULL,
    chat_id INTEGER,
    username TEXT,
    message_id INTEGER
);
CREATE INDEX reputation_publications_subject ON reputation_publications(subject_id, id DESC);

CREATE TABLE reputation_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL,
    delta INTEGER NOT NULL,
    balance INTEGER NOT NULL,
    reason TEXT NOT NULL,
    message_url TEXT,
    notification_claimed INTEGER NOT NULL DEFAULT 0 CHECK (notification_claimed IN (0, 1))
);
CREATE INDEX reputation_events_balance ON reputation_events(user_id, id DESC);
CREATE INDEX reputation_events_pending ON reputation_events(id) WHERE notification_claimed = 0;
