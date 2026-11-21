//! Import job bookkeeping (progress shown in CLI and web UI).

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::Result;

use super::{fmt_ts, get_ts_required};

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: Option<String>,
    pub state: String,
    pub progress: i64,
    pub total: i64,
    pub message: Option<String>,
    pub post_id: Option<i64>,
    /// Telegram user that asked for the import (None for CLI/web).
    pub submitted_by: Option<i64>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl Job {
    pub fn is_active(&self) -> bool {
        matches!(self.state.as_str(), "pending" | "running")
    }
}

const COLUMNS: &str = "id, kind, payload, state, progress, total, message, post_id, submitted_by,
     created_at, updated_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        id: row.get("id")?,
        kind: row.get("kind")?,
        payload: row.get("payload")?,
        state: row.get("state")?,
        progress: row.get("progress")?,
        total: row.get("total")?,
        message: row.get("message")?,
        post_id: row.get("post_id")?,
        submitted_by: row.get("submitted_by")?,
        created_at: get_ts_required(row, "created_at")?,
        updated_at: get_ts_required(row, "updated_at")?,
    })
}

pub fn create(conn: &Connection, kind: &str, payload: &str, total: i64) -> Result<i64> {
    create_for(conn, kind, payload, total, None)
}

/// Same as [`create`] but records who asked (a Telegram user id).
pub fn create_for(
    conn: &Connection,
    kind: &str,
    payload: &str,
    total: i64,
    submitted_by: Option<i64>,
) -> Result<i64> {
    let now = fmt_ts(Utc::now());
    conn.execute(
        "INSERT INTO jobs (kind, payload, state, progress, total, submitted_by, created_at, updated_at)
         VALUES (?1, ?2, 'pending', 0, ?3, ?4, ?5, ?5)",
        params![kind, payload, total, submitted_by, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn set_state(conn: &Connection, id: i64, state: &str, message: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET state = ?1, message = COALESCE(?2, message), updated_at = ?3 WHERE id = ?4",
        params![state, message, fmt_ts(Utc::now()), id],
    )?;
    Ok(())
}

pub fn label_running(conn: &Connection, id: i64, message: &str) -> Result<()> {
    let changed = conn.execute(
        "UPDATE jobs SET message = ?2, updated_at = ?3 WHERE id = ?1 AND state = 'running'",
        params![id, message, fmt_ts(Utc::now())],
    )?;
    if changed == 0 {
        return Err(crate::error::Error::invalid(
            "The gallery edit was interrupted. Reload the post and try again.",
        ));
    }
    Ok(())
}

pub fn progress(conn: &Connection, id: i64, done: i64, total: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET progress = ?1, total = ?2, updated_at = ?3 WHERE id = ?4",
        params![done, total, fmt_ts(Utc::now()), id],
    )?;
    Ok(())
}

pub fn attach_post(conn: &Connection, id: i64, post_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET post_id = ?1 WHERE id = ?2",
        params![post_id, id],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Job>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
            [id],
            from_row,
        )
        .optional()?)
}

pub fn list(conn: &Connection, limit: i64) -> Result<Vec<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs ORDER BY id DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map([limit], from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Id of a `pending`/`running` job with this kind and payload, if one exists.
///
/// Used to refuse a duplicate request (two "publish this channel" clicks) while
/// the first one is still in the worker.
pub fn find_active(conn: &Connection, kind: &str, payload: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM jobs WHERE kind = ?1 AND payload = ?2
               AND state IN ('pending', 'running') ORDER BY id DESC LIMIT 1",
            params![kind, payload],
            |row| row.get(0),
        )
        .optional()?)
}

/// Active publishing work and five recent results for one channel.
pub fn channel_publish_activity(conn: &Connection, channel_id: i64) -> Result<Vec<Job>> {
    let mut stmt = conn.prepare(&format!(
        "WITH channel_jobs AS (
             SELECT {COLUMNS} FROM jobs
             WHERE (kind IN ('publish_channel', 'flush_channel') AND payload = CAST(?1 AS TEXT))
                OR (kind = 'publish_item' AND EXISTS (
                    SELECT 1 FROM queue q WHERE q.id = CAST(jobs.payload AS INTEGER)
                    AND q.channel_id = ?1 AND jobs.created_at >= q.added_at
                ))
         )
         SELECT * FROM channel_jobs WHERE state IN ('pending', 'running')
         UNION ALL
         SELECT * FROM (SELECT * FROM channel_jobs
             WHERE state NOT IN ('pending', 'running') ORDER BY id DESC LIMIT 5)
         ORDER BY id DESC"
    ))?;
    Ok(stmt
        .query_map([channel_id], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// How many jobs of this kind are waiting for a worker slot (`pending`).
pub fn count_pending(conn: &Connection, kind: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE kind = ?1 AND state = 'pending'",
        [kind],
        |row| row.get(0),
    )?)
}

/// Fail jobs left running by a previous process.
pub fn reconcile_except(conn: &Connection, kinds: &[&str]) -> Result<usize> {
    let placeholders = vec!["?"; kinds.len()].join(", ");
    let sql = format!(
        "UPDATE jobs SET state = 'failed', message = 'interrupted by restart', updated_at = ?
         WHERE state IN ('pending', 'running') AND kind NOT IN ({placeholders})"
    );
    let now = fmt_ts(Utc::now());
    Ok(conn.execute(
        &sql,
        rusqlite::params_from_iter(std::iter::once(now.as_str()).chain(kinds.iter().copied())),
    )?)
}

pub fn interrupt_kind(conn: &Connection, kind: &str) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'failed', message = 'interrupted by restart', updated_at = ?1
         WHERE kind = ?2 AND state = 'running'",
        params![fmt_ts(Utc::now()), kind],
    )?)
}

pub fn running_of(conn: &Connection, kind: &str) -> Result<Vec<Job>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE kind = ?1 AND state = 'running'"
    ))?;
    Ok(stmt
        .query_map([kind], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn interrupt_running(conn: &Connection, id: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'failed', message = 'interrupted by restart', updated_at = ?2
         WHERE id = ?1 AND state = 'running'",
        params![id, fmt_ts(Utc::now())],
    )?)
}

/// Atomically take up to `limit` pending jobs of any kind (state -> running).
pub fn claim_pending(conn: &Connection, limit: i64) -> Result<Vec<Job>> {
    claim_matching(conn, "", &[], limit)
}

/// Same, but only jobs whose `kind` is one of `kinds`.
///
/// The worker uses this to keep publish/upload jobs out of the import slots: a
/// gallery-dl run can take minutes, and a queued "publish now" must not wait
/// behind it.
pub fn claim_pending_of(conn: &Connection, kinds: &[&str], limit: i64) -> Result<Vec<Job>> {
    if kinds.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    // the placeholders and the `kind` list are built here, never from user input
    let placeholders = vec!["?"; kinds.len()].join(", ");
    let filter = format!(" AND kind IN ({placeholders})");
    claim_matching(conn, &filter, kinds, limit)
}

fn claim_matching(conn: &Connection, filter: &str, kinds: &[&str], limit: i64) -> Result<Vec<Job>> {
    let ids: Vec<i64> = {
        let sql = format!(
            "SELECT id FROM jobs WHERE state = 'pending'{filter} ORDER BY id LIMIT ?{}",
            kinds.len() + 1
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound = kinds
            .iter()
            .map(|kind| rusqlite::types::Value::Text(kind.to_string()))
            .chain(std::iter::once(rusqlite::types::Value::Integer(limit)));
        let rows = stmt.query_map(rusqlite::params_from_iter(bound), |row| {
            row.get::<_, i64>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };
    let mut claimed = Vec::new();
    for id in ids {
        let updated = conn.execute(
            "UPDATE jobs SET state = 'running', updated_at = ?1 WHERE id = ?2 AND state = 'pending'",
            params![fmt_ts(Utc::now()), id],
        )?;
        if updated > 0
            && let Some(job) = get(conn, id)?
        {
            claimed.push(job);
        }
    }
    Ok(claimed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> crate::db::Db {
        crate::db::Db::open_in_memory().unwrap()
    }

    #[test]
    fn find_active_ignores_finished_jobs() {
        let db = db();
        let id = db
            .with(|conn| create(conn, "publish_channel", "7", 0))
            .unwrap();
        // pending and running both count: a second click must be refused
        for state in ["pending", "running"] {
            db.with({
                let state = state.to_string();
                move |conn| set_state(conn, id, &state, None)
            })
            .unwrap();
            assert_eq!(
                db.with(|conn| find_active(conn, "publish_channel", "7"))
                    .unwrap(),
                Some(id),
                "a {state} job blocks the duplicate"
            );
        }
        // a different payload, or another kind, is a different action
        assert_eq!(
            db.with(|conn| find_active(conn, "publish_channel", "8"))
                .unwrap(),
            None
        );
        assert_eq!(
            db.with(|conn| find_active(conn, "publish_item", "7"))
                .unwrap(),
            None
        );
        // finished jobs no longer block anything
        for state in ["done", "failed"] {
            db.with({
                let state = state.to_string();
                move |conn| set_state(conn, id, &state, None)
            })
            .unwrap();
            assert_eq!(
                db.with(|conn| find_active(conn, "publish_channel", "7"))
                    .unwrap(),
                None,
                "a {state} job does not block"
            );
        }
    }

    #[test]
    fn claim_pending_of_takes_only_the_kinds_asked_for() {
        let db = db();
        let import = db.with(|conn| create(conn, "import", "a", 0)).unwrap();
        let publish = db
            .with(|conn| create(conn, "publish_channel", "1", 0))
            .unwrap();
        db.with(|conn| create(conn, "import", "b", 0)).unwrap();

        // the quick claim takes the publish and leaves every import alone
        let quick = db
            .with(|conn| claim_pending_of(conn, &["publish_channel", "publish_item"], 4))
            .unwrap();
        assert_eq!(
            quick.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![publish],
            "publish jobs are claimed outside the import slots"
        );

        // a zero limit claims nothing: no import slot is free
        assert!(
            db.with(|conn| claim_pending_of(conn, &["import"], 0))
                .unwrap()
                .is_empty()
        );
        // and the import claim only ever sees imports
        let imports = db
            .with(|conn| claim_pending_of(conn, &["import"], 5))
            .unwrap();
        assert_eq!(imports.len(), 2, "both queued imports wait for a slot");
        assert_eq!(imports[0].id, import);
        assert!(imports.iter().all(|job| job.kind == "import"));
    }
    #[test]
    fn count_pending_counts_only_waiting_jobs_of_that_kind() {
        let db = db();
        for kind in ["import", "import", "publish_channel"] {
            db.with({
                let kind = kind.to_string();
                move |conn| create(conn, &kind, "x", 0)
            })
            .unwrap();
        }
        assert_eq!(
            db.with(|conn| count_pending(conn, "import")).unwrap(),
            2,
            "imports wait for a slot"
        );
        // the worker claimed one import, so one import is left waiting …
        db.with(|conn| claim_pending(conn, 1)).unwrap();
        assert_eq!(db.with(|conn| count_pending(conn, "import")).unwrap(), 1);
        // … and the count is per kind: the queued publish is untouched by it
        assert_eq!(
            db.with(|conn| count_pending(conn, "publish_channel"))
                .unwrap(),
            1,
            "publishing is not what /import reports"
        );
    }
}
