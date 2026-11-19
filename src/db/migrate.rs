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
];

pub fn schema_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Apply all pending migrations. Each migration runs in its own transaction.
pub fn migrate(conn: &mut Connection) -> Result<i64> {
    let current = schema_version(conn)?;
    let target = MIGRATIONS.len() as i64;
    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, "applied migration");
    }
    Ok(target)
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
            "post_tag_removals",
            "post_tags",
            "posts",
            "queue",
            "sends",
            "settings",
            "tag_aliases",
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
}
