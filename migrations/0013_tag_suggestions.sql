CREATE TABLE post_tag_suggestion_batches (
    post_id INTEGER PRIMARY KEY REFERENCES posts (id) ON DELETE CASCADE,
    job_id INTEGER NOT NULL UNIQUE REFERENCES jobs (id) ON DELETE CASCADE,
    fingerprint TEXT
);

CREATE TABLE post_tag_suggestions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id INTEGER NOT NULL REFERENCES post_tag_suggestion_batches (job_id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('character', 'franchise')),
    tag TEXT NOT NULL,
    character_tag_id INTEGER,
    evidence TEXT NOT NULL,
    wiki_title TEXT
);
CREATE INDEX idx_post_tag_suggestions_job ON post_tag_suggestions (job_id, id);

CREATE TABLE post_tag_suggestion_images (
    suggestion_id INTEGER NOT NULL REFERENCES post_tag_suggestions (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position > 0),
    PRIMARY KEY (suggestion_id, position)
);

CREATE TABLE tag_franchise_associations (
    character_tag_id INTEGER NOT NULL CHECK (character_tag_id > 0),
    origin TEXT NOT NULL CHECK (origin IN ('manual', 'starter', 'lookup')),
    franchise TEXT,
    wiki_title TEXT,
    checked_at INTEGER,
    PRIMARY KEY (character_tag_id, origin),
    CHECK (origin = 'lookup' OR franchise IS NOT NULL)
);

INSERT INTO tag_franchise_associations (character_tag_id, origin, franchise) VALUES
    (1686237, 'starter', 'genshin_impact'), -- Raiden Shogun
    (1651926, 'starter', 'genshin_impact'), -- Kamisato Ayaka
    (3861, 'starter', 'pokemon'), -- Pikachu
    (385430, 'starter', 'vocaloid'), -- Hatsune Miku
    (12242, 'starter', 'touhou'); -- Kirisame Marisa
