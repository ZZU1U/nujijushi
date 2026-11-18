-- nujijushi 0006: keep each item's own gallery-dl metadata

-- Posts already store their metadata (posts.raw_json, from the group's first
-- sidecar). Sources like twitter write one sidecar per image and the extras were
-- read and dropped after grouping; a ~8 KB JSON blob per item is cheap enough to
-- keep so nothing the extractor reported is lost.
ALTER TABLE media ADD COLUMN meta_json TEXT;
