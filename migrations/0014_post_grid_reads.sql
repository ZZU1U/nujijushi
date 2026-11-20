-- The UNIQUE (post_id, ordinal) constraint already provides this index.
DROP INDEX idx_media_post;

CREATE INDEX idx_posts_imported ON posts (imported_at DESC, id DESC);

-- Match db::queue::DELIVERED_STATES for the archive's hide-sent filter.
CREATE INDEX idx_queue_delivered_post ON queue (post_id)
WHERE state IN ('sent', 'sending', 'unknown');

-- Allow a tag's assignments to be read in post order from the index alone.
DROP INDEX idx_post_tags_tag;
CREATE INDEX idx_post_tags_tag ON post_tags (tag_id, post_id, origin);

-- Keep source suppression in one place. Filters and tag counts can query
-- visible links directly, while consumers of provenance get one row per pair.
CREATE VIEW visible_post_tag_links AS
SELECT pt.rowid AS position, pt.post_id, pt.tag_id, pt.origin
FROM post_tags pt
WHERE pt.origin <> 'source' OR NOT EXISTS (
    SELECT 1 FROM post_tag_removals r WHERE r.post_id = pt.post_id AND r.tag_id = pt.tag_id
);

DROP VIEW active_post_tags;
CREATE VIEW active_post_tags AS
SELECT post_id, tag_id, MIN(position) AS position,
       MAX(origin = 'source') AS source, MAX(origin = 'manual') AS manual
FROM visible_post_tag_links
GROUP BY post_id, tag_id;
