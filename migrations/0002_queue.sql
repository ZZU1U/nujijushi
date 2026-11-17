-- nujijushi 0002: channels, queue, jobs, send audit log
CREATE TABLE channels (
    id                  INTEGER PRIMARY KEY,
    title               TEXT NOT NULL,
    tg_id               TEXT NOT NULL UNIQUE,
    enabled             INTEGER NOT NULL DEFAULT 1,
    paused              INTEGER NOT NULL DEFAULT 0,
    interval_minutes    INTEGER NOT NULL DEFAULT 180,
    posts_per_slot      INTEGER NOT NULL DEFAULT 1,
    active_from         TEXT NOT NULL DEFAULT '09:00',
    active_to           TEXT NOT NULL DEFAULT '23:00',
    tz                  TEXT NOT NULL DEFAULT 'UTC',
    daily_cap           INTEGER NOT NULL DEFAULT 0,
    jitter_secs         INTEGER NOT NULL DEFAULT 0,
    artist_cooldown     INTEGER NOT NULL DEFAULT 3,
    nsfw_policy         TEXT NOT NULL DEFAULT 'spoil',
    min_media           INTEGER NOT NULL DEFAULT 0,
    blocked_tags        TEXT NOT NULL DEFAULT '',
    required_tags       TEXT NOT NULL DEFAULT '',
    parse_mode          TEXT NOT NULL DEFAULT 'html',
    template_name       TEXT NOT NULL DEFAULT 'default',
    template            TEXT,
    attribution         TEXT NOT NULL DEFAULT 'footer',
    tag_style           TEXT NOT NULL DEFAULT 'hashtags',
    max_tags            INTEGER NOT NULL DEFAULT 10,
    silent              INTEGER NOT NULL DEFAULT 0,
    protect             INTEGER NOT NULL DEFAULT 0,
    include_source_tag  INTEGER NOT NULL DEFAULT 1,
    last_sent_at        TEXT,
    next_due_at         TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL
);

-- state: queued | sending | sent | failed | skipped
CREATE TABLE queue (
    id              INTEGER PRIMARY KEY,
    post_id         INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    channel_id      INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    state           TEXT NOT NULL DEFAULT 'queued',
    priority        INTEGER NOT NULL DEFAULT 0,
    pinned          INTEGER NOT NULL DEFAULT 0,
    lease_token     TEXT,
    lease_at        TEXT,
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    sent_at         TEXT,
    tg_message_ids  TEXT,
    added_at        TEXT NOT NULL,
    UNIQUE (post_id, channel_id)
);

CREATE INDEX idx_queue_channel ON queue (channel_id, state, pinned DESC, priority DESC);
CREATE INDEX idx_queue_post ON queue (post_id);

CREATE TABLE jobs (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    payload    TEXT,
    state      TEXT NOT NULL DEFAULT 'pending',
    progress   INTEGER NOT NULL DEFAULT 0,
    total      INTEGER NOT NULL DEFAULT 0,
    message    TEXT,
    post_id    INTEGER,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_jobs_state ON jobs (state, created_at DESC);

CREATE TABLE sends (
    id             INTEGER PRIMARY KEY,
    channel_id     INTEGER NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    queue_id       INTEGER,
    post_id        INTEGER,
    sent_at        TEXT NOT NULL,
    ok             INTEGER NOT NULL,
    tg_message_ids TEXT,
    caption        TEXT,
    error          TEXT
);

CREATE INDEX idx_sends_channel ON sends (channel_id, sent_at DESC);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
