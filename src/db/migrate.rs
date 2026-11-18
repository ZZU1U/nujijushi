//! Schema migrations, embedded in the binary. Applied via `PRAGMA user_version`.

use rusqlite::Connection;

use crate::error::Result;

const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/0001_init.sql"),
    include_str!("../../migrations/0002_queue.sql"),
    include_str!("../../migrations/0003_bot.sql"),
    include_str!("../../migrations/0004_storage.sql"),
    include_str!("../../migrations/0005_media_hygiene.sql"),
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
            "jobs",
            "media",
            "post_tags",
            "posts",
            "queue",
            "sends",
            "settings",
            "tag_aliases",
            "tag_rules",
            "tags",
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
            media_columns.iter().any(|column| column == "sensitive"),
            "media.sensitive is missing: {media_columns:?}"
        );
    }
}
