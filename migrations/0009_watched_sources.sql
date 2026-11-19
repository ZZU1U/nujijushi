CREATE TABLE watched_sources (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    url TEXT NOT NULL UNIQUE,
    initial_batch INTEGER NOT NULL CHECK (initial_batch BETWEEN 1 AND 100),
    checkpoint TEXT,
    last_job_id INTEGER REFERENCES jobs (id) ON DELETE SET NULL
);

CREATE TABLE watched_items (
    watch_id INTEGER NOT NULL REFERENCES watched_sources (id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    url TEXT NOT NULL,
    published_at TEXT NOT NULL,
    handled INTEGER NOT NULL DEFAULT 0,
    last_attempt_job_id INTEGER,
    PRIMARY KEY (watch_id, source_id)
);

CREATE INDEX idx_watched_items_pending ON watched_items
    (watch_id, last_attempt_job_id, published_at) WHERE handled = 0;

CREATE UNIQUE INDEX idx_jobs_watch_active ON jobs (payload)
    WHERE kind = 'watch_check' AND state IN ('pending', 'running');
