-- nujijushi 0001: archive core (authors, posts, blobs, media, tags)
PRAGMA foreign_keys = ON;

CREATE TABLE authors (
    id          INTEGER PRIMARY KEY,
    source      TEXT NOT NULL,
    source_id   TEXT NOT NULL,
    name        TEXT,
    handle      TEXT,
    url         TEXT,
    blocked     INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    UNIQUE (source, source_id)
);

-- state: new | approved | rejected | archived
-- nsfw_override: NULL = derive from rating/rules, 0 = force SFW, 1 = force NSFW
CREATE TABLE posts (
    id            INTEGER PRIMARY KEY,
    source        TEXT NOT NULL,
    source_id     TEXT NOT NULL,
    url           TEXT,
    author_id     INTEGER REFERENCES authors (id) ON DELETE SET NULL,
    title         TEXT,
    description   TEXT,
    rating        TEXT NOT NULL DEFAULT 'sfw',
    posted_at     TEXT,
    imported_at   TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    nsfw_override INTEGER,
    state         TEXT NOT NULL DEFAULT 'new',
    notes         TEXT,
    raw_json      TEXT,
    UNIQUE (source, source_id)
);

CREATE INDEX idx_posts_state ON posts (state, imported_at DESC);
CREATE INDEX idx_posts_author ON posts (author_id);

CREATE TABLE blobs (
    hash       TEXT PRIMARY KEY,
    ext        TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE media (
    id          INTEGER PRIMARY KEY,
    post_id     INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    blob_hash   TEXT NOT NULL REFERENCES blobs (hash),
    mime        TEXT,
    width       INTEGER,
    height      INTEGER,
    is_video    INTEGER NOT NULL DEFAULT 0,
    poster_hash TEXT,
    UNIQUE (post_id, ordinal)
);

CREATE INDEX idx_media_post ON media (post_id, ordinal);
CREATE INDEX idx_media_blob ON media (blob_hash);

CREATE TABLE tags (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL UNIQUE,
    display TEXT
);

CREATE TABLE post_tags (
    post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    origin  TEXT NOT NULL DEFAULT 'source',
    PRIMARY KEY (post_id, tag_id)
);

CREATE INDEX idx_post_tags_tag ON post_tags (tag_id);

CREATE TABLE tag_aliases (
    from_tag TEXT PRIMARY KEY,
    to_tag   TEXT NOT NULL
);

-- action: block (never publish) | nsfw (force NSFW classification)
CREATE TABLE tag_rules (
    tag    TEXT PRIMARY KEY,
    action TEXT NOT NULL,
    note   TEXT
);
