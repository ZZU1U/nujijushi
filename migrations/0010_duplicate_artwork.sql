CREATE TABLE image_fingerprints (
    blob_hash TEXT PRIMARY KEY REFERENCES blobs (hash) ON DELETE CASCADE,
    hash INTEGER,
    aspect REAL,
    band0 INTEGER GENERATED ALWAYS AS (hash & 65535) VIRTUAL,
    band1 INTEGER GENERATED ALWAYS AS ((hash >> 16) & 65535) VIRTUAL,
    band2 INTEGER GENERATED ALWAYS AS ((hash >> 32) & 65535) VIRTUAL,
    band3 INTEGER GENERATED ALWAYS AS ((hash >> 48) & 65535) VIRTUAL
);

CREATE INDEX idx_fingerprint_band0 ON image_fingerprints (band0);
CREATE INDEX idx_fingerprint_band1 ON image_fingerprints (band1);
CREATE INDEX idx_fingerprint_band2 ON image_fingerprints (band2);
CREATE INDEX idx_fingerprint_band3 ON image_fingerprints (band3);

ALTER TABLE media ADD COLUMN duplicate_indexed INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_media_duplicate_pending ON media (id)
    WHERE duplicate_indexed = 0 AND is_video = 0;

CREATE TABLE duplicate_matches (
    left_media_id INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    right_media_id INTEGER NOT NULL REFERENCES media (id) ON DELETE CASCADE,
    left_post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    right_post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    distance INTEGER NOT NULL,
    PRIMARY KEY (left_media_id, right_media_id),
    CHECK (left_post_id < right_post_id)
);

CREATE INDEX idx_duplicate_matches_right_media ON duplicate_matches (right_media_id);
CREATE INDEX idx_duplicate_matches_posts ON duplicate_matches (left_post_id, right_post_id, distance);
CREATE INDEX idx_duplicate_matches_right_post ON duplicate_matches (right_post_id);

CREATE TABLE duplicate_dismissals (
    left_post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    right_post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    PRIMARY KEY (left_post_id, right_post_id),
    CHECK (left_post_id < right_post_id)
);
