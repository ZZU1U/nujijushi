-- FoidFinder returns Danbooru labels without numeric IDs. Keep the ID-based
-- franchise associations and cache resolved labels separately.
CREATE TABLE tag_character_names (
    name TEXT PRIMARY KEY,
    character_tag_id INTEGER CHECK (character_tag_id > 0),
    checked_at INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;

INSERT INTO tag_character_names (name, character_tag_id) VALUES
    ('raiden_shogun', 1686237),
    ('kamisato_ayaka', 1651926),
    ('pikachu', 3861),
    ('hatsune_miku', 385430),
    ('kirisame_marisa', 12242);

INSERT OR IGNORE INTO tag_character_names (name, character_tag_id)
SELECT name, MIN(character_tag_id) FROM (
    SELECT tag AS name, character_tag_id FROM post_tag_suggestions
    WHERE kind = 'character' AND character_tag_id > 0
    UNION ALL
    SELECT wiki_title, character_tag_id FROM tag_franchise_associations
    WHERE wiki_title IS NOT NULL
)
GROUP BY name
HAVING COUNT(DISTINCT character_tag_id) = 1;
