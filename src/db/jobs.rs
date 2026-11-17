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

/// Fail jobs left running by a previous process.
pub fn reconcile(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE jobs SET state = 'failed', message = 'interrupted by restart', updated_at = ?1
         WHERE state IN ('pending', 'running')",
        [fmt_ts(Utc::now())],
    )?)
}

/// Atomically take up to `limit` pending jobs (state -> running).
pub fn claim_pending(conn: &Connection, limit: i64) -> Result<Vec<Job>> {
    let ids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT id FROM jobs WHERE state = 'pending' ORDER BY id LIMIT ?1")?;
        let rows = stmt.query_map([limit], |row| row.get::<_, i64>(0))?;
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
