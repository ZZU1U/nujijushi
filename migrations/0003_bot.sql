-- nujijushi 0003: channel aliases + discovery, bot users, quotas, submitter tracking

ALTER TABLE channels ADD COLUMN alias TEXT;
ALTER TABLE channels ADD COLUMN discovered INTEGER NOT NULL DEFAULT 0;
UPDATE channels SET alias = 'ch' || id WHERE alias IS NULL;
CREATE UNIQUE INDEX idx_channels_alias ON channels (alias);

ALTER TABLE posts ADD COLUMN submitted_by INTEGER;
ALTER TABLE jobs ADD COLUMN submitted_by INTEGER;
CREATE INDEX idx_jobs_submitter ON jobs (submitted_by);

-- role: admin | guest | banned. `perms` is the hook for per-user grants later.
CREATE TABLE users (
    tg_id        INTEGER PRIMARY KEY,
    username     TEXT,
    first_name   TEXT,
    role         TEXT NOT NULL DEFAULT 'guest',
    perms        TEXT,
    note         TEXT,
    created_at   TEXT NOT NULL,
    last_seen_at TEXT
);

-- quota counters: one row per (user, day, action)
CREATE TABLE usage (
    tg_id INTEGER NOT NULL,
    day   TEXT NOT NULL,
    kind  TEXT NOT NULL,
    count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (tg_id, day, kind)
);
