ALTER TABLE post_tags RENAME TO post_tags_legacy;
DROP INDEX idx_post_tags_tag;

CREATE TABLE post_tags (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    origin  TEXT NOT NULL DEFAULT 'source',
    PRIMARY KEY (post_id, tag_id, origin)
);

INSERT INTO post_tags (rowid, post_id, tag_id, origin)
SELECT rowid, post_id, tag_id, origin FROM post_tags_legacy;
DROP TABLE post_tags_legacy;
CREATE INDEX idx_post_tags_tag ON post_tags (tag_id);

CREATE TABLE post_tag_removals (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    PRIMARY KEY (post_id, tag_id)
);

CREATE VIEW active_post_tags AS
SELECT pt.post_id, pt.tag_id, MIN(pt.rowid) AS position,
       MAX(pt.origin = 'source') AS source, MAX(pt.origin = 'manual') AS manual
FROM post_tags pt
WHERE pt.origin <> 'source' OR NOT EXISTS (
    SELECT 1 FROM post_tag_removals r WHERE r.post_id = pt.post_id AND r.tag_id = pt.tag_id
)
GROUP BY pt.post_id, pt.tag_id;
