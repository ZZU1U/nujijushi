ALTER TABLE watched_sources ADD COLUMN pixiv_baselined INTEGER NOT NULL DEFAULT 0
    CHECK (pixiv_baselined IN (0, 1));

ALTER TABLE watched_items ADD COLUMN pixiv_missing_scans INTEGER NOT NULL DEFAULT 0
    CHECK (pixiv_missing_scans BETWEEN 0 AND 2);

CREATE TABLE watched_pixiv_seen (
    watch_id INTEGER NOT NULL REFERENCES watched_sources (id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    PRIMARY KEY (watch_id, source_id)
) WITHOUT ROWID;
