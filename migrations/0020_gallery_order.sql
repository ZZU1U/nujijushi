CREATE TABLE media_gallery_order (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    blob_hash TEXT NOT NULL,
    occurrence INTEGER NOT NULL CHECK (occurrence >= 0),
    PRIMARY KEY (post_id, position),
    UNIQUE (post_id, blob_hash, occurrence)
);
