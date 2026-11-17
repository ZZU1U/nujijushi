-- nujijushi 0004: mirroring posts to external storage sinks

CREATE TABLE mirrors (
    post_id     INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    sink        TEXT NOT NULL,
    -- pending | done | failed
    state       TEXT NOT NULL DEFAULT 'pending',
    external_id TEXT,
    bytes       INTEGER NOT NULL DEFAULT 0,
    attempts    INTEGER NOT NULL DEFAULT 0,
    error       TEXT,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (post_id, sink)
);

CREATE INDEX idx_mirrors_state ON mirrors (sink, state);
