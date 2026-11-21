//! Schema migrations, embedded in the binary. Applied via `PRAGMA user_version`.

use rusqlite::Connection;

use crate::error::Result;

const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/0001_init.sql"),
    include_str!("../../migrations/0002_queue.sql"),
    include_str!("../../migrations/0003_bot.sql"),
    include_str!("../../migrations/0004_storage.sql"),
    include_str!("../../migrations/0005_media_hygiene.sql"),
    include_str!("../../migrations/0006_media_meta.sql"),
    include_str!("../../migrations/0007_tag_provenance.sql"),
    include_str!("../../migrations/0008_queue_alerts.sql"),
    include_str!("../../migrations/0009_watched_sources.sql"),
    include_str!("../../migrations/0010_duplicate_artwork.sql"),
    include_str!("../../migrations/0011_watch_enabled.sql"),
    include_str!("../../migrations/0012_guest_reputation.sql"),
    include_str!("../../migrations/0013_tag_suggestions.sql"),
    include_str!("../../migrations/0014_post_grid_reads.sql"),
    include_str!("../../migrations/0015_media_merges.sql"),
    include_str!("../../migrations/0016_media_compressions.sql"),
    include_str!("../../migrations/0017_tagger_character_names.sql"),
    include_str!("../../migrations/0018_post_text_edits.sql"),
    include_str!("../../migrations/0019_gallery_visibility.sql"),
    include_str!("../../migrations/0020_gallery_order.sql"),
];

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

/// Apply all pending migrations. Each migration runs in its own transaction.
pub fn migrate(conn: &mut Connection) -> Result<i64> {
    let current = schema_version(conn)?;
    let target = MIGRATIONS.len() as i64;
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        // Development archives can already contain either feature under a
        // different version. Check the schema before applying these migrations.
        let installed = match version {
            12 => has_table(conn, "reputation_events")?,
            13 => has_table(conn, "post_tag_suggestion_batches")?,
            _ => version <= current,
        };
        if installed && version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        if !installed {
            tx.execute_batch(sql)?;
        }
        tx.pragma_update(None, "user_version", version.max(current))?;
        tx.commit()?;
        tracing::info!(version, "applied migration");
    }
    Ok(target.max(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_to_head_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 0);
        let head = migrate(&mut conn).unwrap();
        assert_eq!(head, MIGRATIONS.len() as i64);
        assert_eq!(schema_version(&conn).unwrap(), head);
        // running again must not fail
        migrate(&mut conn).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        for expected in [
            "authors",
            "blobs",
            "channels",
            "duplicate_dismissals",
            "duplicate_matches",
            "image_fingerprints",
            "jobs",
            "media",
            "media_compressions",
            "media_gallery_order",
            "media_hidden_sources",
            "media_merge_members",
            "media_merge_sources",
            "media_merges",
            "tag_character_names",
            "post_tag_removals",
            "post_tag_suggestion_batches",
            "post_tag_suggestion_images",
            "post_tag_suggestions",
            "post_tags",
            "posts",
            "queue",
            "reputation_events",
            "reputation_keys",
            "reputation_posts",
            "reputation_publications",
            "reputation_subjects",
            "reputation_suggestions",
            "sends",
            "settings",
            "tag_aliases",
            "tag_franchise_associations",
            "tag_rules",
            "tags",
            "watched_items",
            "watched_sources",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}"
            );
        }

        // 0005 added the per-media flag; a fresh database must have it
        let media_columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('media')")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            media_columns.iter().any(|column| column == "sensitive")
                && media_columns.iter().any(|column| column == "meta_json"),
            "media.sensitive / media.meta_json are missing: {media_columns:?}"
        );
        let edit_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('posts')
                 WHERE name IN ('title_edited', 'description_edited')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edit_columns, 2);
    }

    #[test]
    fn upgrades_existing_contiguous_merges_to_explicit_membership() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..18] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 18).unwrap();
        conn.execute_batch(
            "INSERT INTO posts (id, source, source_id, imported_at, updated_at)
             VALUES (1, 'local', 'sample', '2026-01-01', '2026-01-01');
             INSERT INTO blobs (hash, ext, bytes, created_at)
             VALUES ('source', 'png', 1, '2026-01-01'), ('merged', 'png', 1, '2026-01-01');
             INSERT INTO media_merge_sources (post_id, ordinal, blob_hash, is_video, sensitive)
             VALUES (1, 0, 'source', 0, 0), (1, 1, 'source', 0, 0), (1, 2, 'source', 0, 0),
                    (1, 3, 'source', 0, 0);
             INSERT INTO media_merges (id, post_id, first_ordinal, last_ordinal, blob_hash, width, height)
             VALUES (9, 1, 1, 3, 'merged', 3, 1);",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        let members = conn
            .prepare("SELECT source_ordinal FROM media_merge_members WHERE merge_id = 9 ORDER BY source_ordinal")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(members, [1, 2, 3]);
    }

    #[test]
    fn upgrades_when_a_development_archive_already_uses_version_twelve() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..11] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 12).unwrap();
        assert!(!has_table(&conn, "reputation_events").unwrap());

        migrate(&mut conn).unwrap();
        assert!(has_table(&conn, "reputation_events").unwrap());
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
        migrate(&mut conn).unwrap();
    }

    #[test]
    fn preserves_tag_suggestions_from_earlier_development_archives() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..11] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch(MIGRATIONS[12]).unwrap();
        conn.pragma_update(None, "user_version", 12).unwrap();
        conn.execute(
            "INSERT INTO tag_franchise_associations (character_tag_id, origin, franchise, wiki_title)
             VALUES (42, 'manual', 'custom_franchise', NULL),
                    (42, 'lookup', 'old_franchise', 'previously_resolved_name')",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        assert!(has_table(&conn, "reputation_events").unwrap());
        assert_eq!(schema_version(&conn).unwrap(), MIGRATIONS.len() as i64);
        let franchise: String = conn
            .query_row(
                "SELECT franchise FROM tag_franchise_associations
                 WHERE character_tag_id = 42 AND origin = 'manual'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(franchise, "custom_franchise");
        let character_id =
            super::super::tag_suggestions::character_id(&conn, "previously_resolved_name").unwrap();
        assert_eq!(character_id, Some(Some(42)));
    }

    #[test]
    fn upgrading_from_v6_preserves_tag_links_and_origins() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..6] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 6).unwrap();
        conn.execute_batch(
            "INSERT INTO posts (id, source, source_id, imported_at, updated_at)
             VALUES (1, 'pixiv', '42', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO tags (id, name) VALUES (1, 'imported'), (2, 'curated');
             INSERT INTO post_tags (post_id, tag_id, origin)
             VALUES (1, 2, 'manual'), (1, 1, 'source');",
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let tags = crate::db::tags::provenance(&conn, 1).unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].name, "curated");
        assert!(tags[0].manual && !tags[0].source);
        assert_eq!(tags[1].name, "imported");
        assert!(tags[1].source && !tags[1].manual);
        assert!(crate::db::tags::removals(&conn, 1).unwrap().is_empty());
    }

    #[test]
    fn upgrading_from_v10_keeps_existing_watches_enabled() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..10] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 10).unwrap();
        conn.execute(
            "INSERT INTO watched_sources (name, url, initial_batch, checkpoint)
             VALUES ('Artist', 'https://x.com/artist/media', 3, '2025-01-02T00:00:00Z')",
            [],
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let watch = super::super::watches::get(&conn, 1).unwrap();
        assert!(watch.enabled);
        assert_eq!(watch.initial_batch, 3);
        assert!(watch.checkpoint.is_some());
    }
}
