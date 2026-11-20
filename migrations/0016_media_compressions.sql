-- A compressed rendition overlays a source panel or an existing merged span.
CREATE TABLE media_compressions (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    first_ordinal INTEGER NOT NULL CHECK (first_ordinal >= 0),
    blob_hash TEXT NOT NULL REFERENCES blobs (hash),
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    PRIMARY KEY (post_id, first_ordinal)
);
CREATE INDEX idx_media_compressions_blob ON media_compressions (blob_hash);
CREATE INDEX idx_jobs_media_compression_post ON jobs (post_id, id DESC) WHERE kind = 'compress_media';
