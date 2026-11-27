//! Per-post mirror bookkeeping (which sinks hold which post).

use chrono::{DateTime, Duration, Utc};
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

/// A failed upload is retried only after this delay. Without it a permanently
/// broken row would occupy every drain batch and keep newer posts from uploading.
pub const FAILED_RETRY_SECS: i64 = 60;

/// Work the worker may upload now: pending rows, plus failures old enough to
/// retry. Pending always comes first, so broken rows cannot starve new posts.
pub fn pending(conn: &Connection, sink: Option<&str>, limit: i64) -> Result<Vec<Mirror>> {
    pending_at(conn, sink, limit, Utc::now())
}

pub(crate) fn pending_at(
    conn: &Connection,
    sink: Option<&str>,
    limit: i64,
    now: DateTime<Utc>,
) -> Result<Vec<Mirror>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM mirrors
         WHERE (state = 'pending' OR (state = 'failed' AND updated_at <= ?2))
           AND (?1 IS NULL OR sink = ?1)
         ORDER BY CASE state WHEN 'pending' THEN 0 ELSE 1 END, post_id, sink
         LIMIT {}",
        limit.max(1)
    );
    let cutoff = fmt_ts(now - Duration::seconds(FAILED_RETRY_SECS));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![sink, cutoff], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

/// A row that cannot succeed without an operator (disabled sink, missing media).
/// It leaves the automatic retry pool instead of being retried every drain.
pub fn mark_blocked(conn: &Connection, post_id: i64, sink: &str, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE mirrors SET state = 'blocked', error = ?1, updated_at = ?2
         WHERE post_id = ?3 AND sink = ?4 AND state != 'done'",
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
        post_with(db, "1")
    }

    fn post_with(db: &crate::db::Db, source_id: &str) -> i64 {
        let mut post = crate::source::NormalizedPost::new("pixiv", source_id);
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

        // failures get a backoff before another automatic attempt
        db.with(|conn| enqueue(conn, post_id, "s3")).unwrap();
        db.with(|conn| mark_failed(conn, post_id, "s3", "boom"))
            .unwrap();
        assert!(
            db.with(|conn| pending(conn, Some("s3"), 10))
                .unwrap()
                .is_empty(),
            "a fresh failure is not retried immediately"
        );
        let later = Utc::now() + Duration::seconds(FAILED_RETRY_SECS + 1);
        let retry_rows = db
            .with(|conn| pending_at(conn, Some("s3"), 10, later))
            .unwrap();
        assert_eq!(retry_rows.len(), 1);
        assert_eq!(retry_rows[0].attempts, 1);
        assert_eq!(retry_rows[0].error.as_deref(), Some("boom"));
    }

    #[test]
    fn pending_uploads_are_never_starved_by_failures() {
        let db = db();
        let broken = post_with(&db, "1");
        let healthy = post_with(&db, "2");
        db.with(|conn| {
            enqueue(conn, broken, "local")?;
            mark_failed(conn, broken, "local", "boom")?;
            enqueue(conn, healthy, "local")?;
            Ok(())
        })
        .unwrap();

        let later = Utc::now() + Duration::seconds(FAILED_RETRY_SECS + 1);
        let first = db.with(|conn| pending_at(conn, None, 1, later)).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].post_id, healthy, "pending work goes first");
        let all = db.with(|conn| pending_at(conn, None, 10, later)).unwrap();
        assert_eq!(all.len(), 2, "the failure is still retried eventually");
    }

    #[test]
    fn blocked_rows_leave_the_automatic_retry_pool() {
        let db = db();
        let post_id = post(&db);
        db.with(|conn| {
            enqueue(conn, post_id, "local")?;
            mark_blocked(conn, post_id, "local", "sink is no longer enabled")?;
            Ok(())
        })
        .unwrap();

        let later = Utc::now() + Duration::seconds(FAILED_RETRY_SECS + 1);
        assert!(
            db.with(|conn| pending_at(conn, None, 10, later))
                .unwrap()
                .is_empty()
        );
        let row = db
            .with(|conn| get(conn, post_id, "local"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "blocked");
        assert_eq!(row.error.as_deref(), Some("sink is no longer enabled"));
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
