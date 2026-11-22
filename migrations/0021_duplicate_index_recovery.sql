CREATE TABLE duplicate_fingerprint_failures (
    blob_hash TEXT PRIMARY KEY REFERENCES blobs (hash) ON DELETE CASCADE,
    attempts INTEGER NOT NULL CHECK (attempts >= 1),
    retry_at INTEGER NOT NULL
);

-- Earlier releases used NULL for both skipped animation and failed reads.
-- Recheck those images once with the new error handling.
UPDATE media SET duplicate_indexed = 0
WHERE is_video = 0 AND blob_hash IN (SELECT blob_hash FROM image_fingerprints WHERE hash IS NULL);
DELETE FROM image_fingerprints WHERE hash IS NULL;

-- Exact matches now come from archive references, including unindexed media.
DELETE FROM duplicate_matches WHERE distance = 0;
DROP INDEX idx_media_blob;
CREATE INDEX idx_media_blob ON media (blob_hash, post_id, ordinal);
