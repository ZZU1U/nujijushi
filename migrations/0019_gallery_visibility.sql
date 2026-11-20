CREATE TABLE media_merge_members (
    merge_id INTEGER NOT NULL REFERENCES media_merges (id) ON DELETE CASCADE,
    source_ordinal INTEGER NOT NULL CHECK (source_ordinal >= 0),
    PRIMARY KEY (merge_id, source_ordinal)
);

INSERT INTO media_merge_members (merge_id, source_ordinal)
SELECT m.id, s.ordinal
FROM media_merges m JOIN media_merge_sources s ON s.post_id = m.post_id
    AND s.ordinal BETWEEN m.first_ordinal AND m.last_ordinal;

CREATE TABLE media_hidden_sources (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    blob_hash TEXT NOT NULL,
    occurrence INTEGER NOT NULL CHECK (occurrence >= 0),
    PRIMARY KEY (post_id, blob_hash, occurrence)
);
