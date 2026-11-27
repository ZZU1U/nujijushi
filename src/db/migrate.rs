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
    include_str!("../../migrations/0021_duplicate_index_recovery.sql"),
    include_str!("../../migrations/0022_delivery_links.sql"),
    include_str!("../../migrations/0023_daily_watches.sql"),
    include_str!("../../migrations/0024_pixiv_watch_state.sql"),
    include_str!("../../migrations/0025_post_caption_url.sql"),
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
            "duplicate_fingerprint_failures",
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
            "watched_pixiv_seen",
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
                 WHERE name IN ('title_edited', 'description_edited', 'caption_url')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(edit_columns, 3);
        let delivery_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sends')
                 WHERE name IN ('delivery_target', 'delivery_chat_id', 'delivery_username')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(delivery_columns, 3);
    }

    #[test]
    fn delivery_link_migration_keeps_the_original_destination_for_matching_sends() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..21] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 21).unwrap();
        conn.execute_batch(
            "INSERT INTO posts (id, source, source_id, imported_at, updated_at)
                 VALUES (1, 'local', 'delivered', '2026-01-01', '2026-01-01');
             INSERT INTO channels (id, alias, title, tg_id, created_at)
                 VALUES (1, 'artwork', 'Artwork', '@new_channel', '2026-01-01');
             INSERT INTO queue (id, post_id, channel_id, state, added_at, sent_at, tg_message_ids,
                                delivery_target)
                 VALUES (1, 1, 1, 'sent', '2026-01-01', '2026-01-03', '22,23', '@old_channel');
             INSERT INTO sends (channel_id, queue_id, post_id, sent_at, ok, tg_message_ids)
                 VALUES (1, 1, 1, '2026-01-02', 1, '22,23'),
                        (1, 1, 1, '2026-01-03', 1, '22,23');",
        )
        .unwrap();

        migrate(&mut conn).unwrap();
        super::super::queue::requeue(&conn, 1).unwrap();
        let links = super::super::queue::delivery_links(&conn, 1).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://t.me/old_channel/22");
        assert_eq!(
            conn.query_row(
                "SELECT delivery_target FROM sends WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0)
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn duplicate_recovery_requeues_legacy_null_fingerprints() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..20] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 20).unwrap();
        conn.execute_batch(
            "INSERT INTO posts (id, source, source_id, imported_at, updated_at)
                 VALUES (1, 'local', 'first', '', ''), (2, 'local', 'second', '', '');
             INSERT INTO blobs (hash, ext, bytes, created_at)
                 VALUES ('unreadable', 'png', 10, ''), ('readable', 'png', 10, '');
             INSERT INTO media (id, post_id, ordinal, blob_hash, duplicate_indexed)
                 VALUES (1, 1, 0, 'unreadable', 1), (2, 1, 1, 'readable', 1),
                        (3, 2, 0, 'readable', 1);
             INSERT INTO image_fingerprints (blob_hash, hash, aspect)
                 VALUES ('unreadable', NULL, NULL), ('readable', 42, 1.0);
             INSERT INTO duplicate_matches VALUES (2, 3, 1, 2, 0);
             INSERT INTO duplicate_dismissals VALUES (1, 2);",
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let pending = crate::db::duplicates::pending(&conn, 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].blob_hash, "unreadable");
        assert!(!pending[0].fingerprinted);
        assert_eq!(
            conn.query_row("SELECT hash FROM image_fingerprints", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            42
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM duplicate_matches", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(crate::db::duplicates::evidence(&conn, 1, 2).unwrap()[0].exact);
        assert!(crate::db::duplicates::pairs(&conn, 0).unwrap().is_empty());
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
    fn daily_watch_migration_uses_the_last_settled_check() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..22] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 22).unwrap();
        conn.execute_batch(
            "INSERT INTO watched_sources (name, url, initial_batch)
                 VALUES ('Artist', 'https://x.com/artist/media', 3),
                        ('Never checked', 'https://x.com/other/media', 3);
             INSERT INTO jobs (id, kind, payload, state, created_at, updated_at)
                 VALUES (1, 'watch_check', '1', 'done', '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z'),
                        (2, 'watch_check', '1', 'failed', '2026-01-02T00:00:00Z', '2026-01-02T00:01:00Z'),
                        (3, 'watch_check', '1', 'pending', '2026-01-03T00:00:00Z', '2026-01-03T00:00:00Z');
             UPDATE watched_sources SET last_job_id = 3 WHERE id = 1;",
        )
        .unwrap();

        migrate(&mut conn).unwrap();

        let watch = super::super::watches::get(&conn, 1).unwrap();
        assert_eq!(watch.last_job_id, Some(3));
        assert_eq!(
            watch.last_run_at,
            super::super::parse_ts("2026-01-02T00:01:00Z")
        );
        assert_eq!(
            super::super::watches::get(&conn, 2).unwrap().last_run_at,
            None
        );
    }

    #[test]
    fn pixiv_watch_migration_preserves_existing_checkpoint_and_queue() {
        let mut conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..23] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 23).unwrap();
        conn.execute_batch(
            "INSERT INTO watched_sources (id, name, url, initial_batch, checkpoint)
                 VALUES (1, 'Artist', 'https://www.pixiv.net/users/7/artworks', 2, '2026-01-01T00:00:00Z');
             INSERT INTO watched_items (watch_id, source_id, url, published_at, last_attempt_job_id)
                 VALUES (1, '42', 'https://www.pixiv.net/artworks/42', '2026-01-01T00:00:00Z', 8);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();

        use super::super::{parse_ts, watches};
        let watch = watches::get(&conn, 1).unwrap();
        assert_eq!(watch.checkpoint, parse_ts("2026-01-01T00:00:00Z"));
        assert!(!watch.pixiv_baselined);
        assert!(watches::pixiv_seen(&conn, 1).unwrap().is_empty());
        assert_eq!(
            watches::pending(&conn, 1, 1, true).unwrap(),
            [(
                "42".to_owned(),
                "https://www.pixiv.net/artworks/42".to_owned()
            )]
        );
        assert_eq!(watches::pixiv_deferred_count(&conn, 1).unwrap(), 0);
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
