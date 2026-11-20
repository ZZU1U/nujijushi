//! Per-post mirror bookkeeping (which sinks hold which post).

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::Result;

use super::fmt_ts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mirror {
    pub post_id: i64,
    pub sink: String,
    /// pending | done | failed | blocked
    pub state: String,
    pub external_id: Option<String>,
    pub bytes: i64,
    pub attempts: i64,
    pub error: Option<String>,
}

impl Mirror {
    pub fn is_done(&self) -> bool {
        self.state == "done"
    }
}

const COLUMNS: &str = "post_id, sink, state, external_id, bytes, attempts, error";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Mirror> {
    Ok(Mirror {
        post_id: row.get("post_id")?,
        sink: row.get("sink")?,
        state: row.get("state")?,
        external_id: row.get("external_id")?,
        bytes: row.get("bytes")?,
        attempts: row.get("attempts")?,
        error: row.get("error")?,
    })
}

/// Queue a post for a sink unless it is already there or already done.
pub fn enqueue(conn: &Connection, post_id: i64, sink: &str) -> Result<bool> {
    let inserted = conn.execute(
        "INSERT INTO mirrors (post_id, sink, state, updated_at) VALUES (?1, ?2, 'pending', ?3)
         ON CONFLICT (post_id, sink) DO UPDATE SET
            state = CASE WHEN mirrors.state = 'done' THEN 'done' ELSE 'pending' END,
            updated_at = excluded.updated_at
         WHERE mirrors.state != 'done'",
        params![post_id, sink, fmt_ts(Utc::now())],
    )?;
    Ok(inserted > 0)
}

pub fn get(conn: &Connection, post_id: i64, sink: &str) -> Result<Option<Mirror>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM mirrors WHERE post_id = ?1 AND sink = ?2"),
            params![post_id, sink],
            from_row,
        )
        .optional()?)
}

/// Oldest pending work first, so a queue drains roughly in import order.
pub fn pending(conn: &Connection, sink: Option<&str>, limit: i64) -> Result<Vec<Mirror>> {
    let mut sql = format!("SELECT {COLUMNS} FROM mirrors WHERE state IN ('pending', 'failed')");
    if sink.is_some() {
        sql.push_str(" AND sink = ?1");
    }
    sql.push_str(" ORDER BY post_id, sink LIMIT ");
    sql.push_str(&limit.max(1).to_string());
    let mut stmt = conn.prepare(&sql)?;
    let rows = match sink {
        Some(sink) => stmt
            .query_map([sink], from_row)?
            .collect::<rusqlite::Result<Vec<_>>>(),
        None => stmt
            .query_map([], from_row)?
            .collect::<rusqlite::Result<Vec<_>>>(),
    }?;
    Ok(rows)
}

pub fn mark_done(
    conn: &Connection,
    post_id: i64,
    sink: &str,
    external_id: Option<&str>,
    bytes: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE mirrors SET state = 'done', external_id = ?1, bytes = ?2, error = NULL,
            attempts = attempts + 1, updated_at = ?3
         WHERE post_id = ?4 AND sink = ?5",
        params![external_id, bytes, fmt_ts(Utc::now()), post_id, sink],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, post_id: i64, sink: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE mirrors SET state = 'failed', error = ?1, attempts = attempts + 1, updated_at = ?2
         WHERE post_id = ?3 AND sink = ?4",
        params![error, fmt_ts(Utc::now()), post_id, sink],
    )?;
    Ok(())
}

pub fn block_empty(conn: &Connection, post_id: i64, sink: &str) -> Result<()> {
    conn.execute(
        "UPDATE mirrors SET state = 'blocked', error = 'No outgoing media', updated_at = ?3
         WHERE post_id = ?1 AND sink = ?2 AND state != 'done'",
        params![post_id, sink, fmt_ts(Utc::now())],
    )?;
    Ok(())
}

pub fn has_blocked(conn: &Connection, post_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM mirrors WHERE post_id = ?1 AND state = 'blocked')",
        [post_id],
        |row| row.get(0),
    )?)
}

pub fn unblock(conn: &Connection, post_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE mirrors SET state = 'pending', error = NULL, updated_at = ?2
         WHERE post_id = ?1 AND state = 'blocked'",
        params![post_id, fmt_ts(Utc::now())],
    )?;
    Ok(())
}

/// Counts per sink, for `storage ls` and the dashboard.
pub fn summary(conn: &Connection) -> Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT sink, state, COUNT(*) FROM mirrors GROUP BY sink, state ORDER BY sink, state",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> crate::db::Db {
        crate::db::Db::open_in_memory().unwrap()
    }

    fn post(db: &crate::db::Db) -> i64 {
        let mut post = crate::source::NormalizedPost::new("pixiv", "1");
        post.title = Some("x".into());
        db.with(|conn| {
            let (id, _) = crate::db::posts::upsert(conn, &post, &[])?;
            Ok(id)
        })
        .unwrap()
    }

    #[test]
    fn enqueue_done_and_refail_cycle() {
        let db = db();
        let post_id = post(&db);
        assert!(db.with(|conn| enqueue(conn, post_id, "local")).unwrap());
        // queuing twice keeps one row
        db.with(|conn| enqueue(conn, post_id, "local")).unwrap();
        let first_batch = db.with(|conn| pending(conn, None, 10)).unwrap();
        assert_eq!(first_batch.len(), 1);

        db.with(|conn| mark_done(conn, post_id, "local", Some("path/1"), 123))
            .unwrap();
        let stored = db
            .with(|conn| get(conn, post_id, "local"))
            .unwrap()
            .unwrap();
        assert!(stored.is_done());
        assert_eq!(stored.external_id.as_deref(), Some("path/1"));
        assert_eq!(stored.bytes, 123);
        assert!(db.with(|conn| pending(conn, None, 10)).unwrap().is_empty());

        // a done mirror is not re-queued by enqueue
        db.with(|conn| enqueue(conn, post_id, "local")).unwrap();
        assert!(db.with(|conn| pending(conn, None, 10)).unwrap().is_empty());

        // failures are retried
        db.with(|conn| enqueue(conn, post_id, "s3")).unwrap();
        db.with(|conn| mark_failed(conn, post_id, "s3", "boom"))
            .unwrap();
        let retry_rows = db.with(|conn| pending(conn, Some("s3"), 10)).unwrap();
        assert_eq!(retry_rows.len(), 1);
        assert_eq!(retry_rows[0].attempts, 1);
        assert_eq!(retry_rows[0].error.as_deref(), Some("boom"));
    }

    #[test]
    fn summary_groups_by_sink_and_state() {
        let db = db();
        let post_id = post(&db);
        db.with(|conn| enqueue(conn, post_id, "local")).unwrap();
        db.with(|conn| enqueue(conn, post_id, "webdav")).unwrap();
        db.with(|conn| mark_done(conn, post_id, "webdav", None, 1))
            .unwrap();
        let rows = db.with(|conn| summary(conn)).unwrap();
        assert!(rows.contains(&("local".to_string(), "pending".to_string(), 1)));
        assert!(rows.contains(&("webdav".to_string(), "done".to_string(), 1)));
    }
}
