-- An edited post keeps its source gallery for re-import and Undo.
CREATE TABLE media_merge_sources (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    blob_hash TEXT NOT NULL REFERENCES blobs (hash),
    mime TEXT,
    width INTEGER,
    height INTEGER,
    is_video INTEGER NOT NULL,
    poster_hash TEXT,
    sensitive INTEGER NOT NULL,
    meta_json TEXT,
    PRIMARY KEY (post_id, ordinal)
);
CREATE INDEX idx_media_merge_source_hash ON media_merge_sources (blob_hash);
CREATE INDEX idx_media_merge_source_poster ON media_merge_sources (poster_hash)
WHERE poster_hash IS NOT NULL;

-- Non-overlapping ranges in source-gallery order, with both endpoints included.
CREATE TABLE media_merges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    first_ordinal INTEGER NOT NULL CHECK (first_ordinal >= 0),
    last_ordinal INTEGER NOT NULL CHECK (last_ordinal > first_ordinal),
    blob_hash TEXT NOT NULL REFERENCES blobs (hash),
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    UNIQUE (post_id, first_ordinal)
);
CREATE INDEX idx_media_merges_blob ON media_merges (blob_hash);
CREATE INDEX idx_jobs_media_merge_post ON jobs (post_id, id DESC) WHERE kind = 'merge_media';
