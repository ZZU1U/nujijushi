//! Watched feeds and the durable queue of discovered artwork.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::error::{Error, Result};
use crate::source::watch::Candidate;

use super::{fmt_ts, get_ts, jobs, transaction};

#[derive(Debug)]
pub struct Watch {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub initial_batch: u16,
    pub enabled: bool,
    pub checkpoint: Option<DateTime<Utc>>,
    pub last_job_id: Option<i64>,
}

fn from_row(row: &Row<'_>) -> rusqlite::Result<Watch> {
    Ok(Watch {
        id: row.get("id")?,
        name: row.get("name")?,
        url: row.get("url")?,
        initial_batch: row.get("initial_batch")?,
        enabled: row.get("enabled")?,
        checkpoint: get_ts(row, "checkpoint")?,
        last_job_id: row.get("last_job_id")?,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Watch>> {
    let mut stmt = conn.prepare("SELECT * FROM watched_sources ORDER BY name, id")?;
    Ok(stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Watch> {
    conn.query_row(
        "SELECT * FROM watched_sources WHERE id = ?1",
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("watch {id}")))
}

pub fn create(conn: &Connection, name: &str, url: &str, initial_batch: u16) -> Result<bool> {
    Ok(conn.execute(
        "INSERT INTO watched_sources (name, url, initial_batch) VALUES (?1, ?2, ?3)
         ON CONFLICT (url) DO NOTHING",
        params![name, url, initial_batch],
    )? != 0)
}

pub fn remove(conn: &Connection, id: i64, kind: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM watched_sources WHERE id = ?1 AND NOT EXISTS (
            SELECT 1 FROM jobs WHERE kind = ?2 AND payload = CAST(?1 AS TEXT)
            AND state IN ('pending', 'running')
         )",
        params![id, kind],
    )? != 0)
}

pub fn set_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    let changed = conn.execute(
        "UPDATE watched_sources SET enabled = ?2 WHERE id = ?1",
        params![id, enabled],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(format!("watch {id}")));
    }
    Ok(())
}

pub fn enqueue(conn: &mut Connection, watch_id: i64, kind: &str) -> Result<(i64, bool)> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !get(&tx, watch_id)?.enabled {
        return Err(Error::invalid("Watch is paused"));
    }
    let payload = watch_id.to_string();
    if let Some(id) = jobs::find_active(&tx, kind, &payload)? {
        return Ok((id, false));
    }
    let id = jobs::create(&tx, kind, &payload, 0)?;
    tx.execute(
        "UPDATE watched_sources SET last_job_id = ?1 WHERE id = ?2",
        params![id, watch_id],
    )?;
    tx.commit()?;
    Ok((id, true))
}

pub fn record_scan(
    conn: &mut Connection,
    watch: &Watch,
    candidates: &[Candidate],
    started: DateTime<Utc>,
) -> Result<()> {
    let checkpoint = candidates
        .iter()
        .map(|item| item.published_at)
        .max()
        .into_iter()
        .chain(watch.checkpoint)
        .max()
        .unwrap_or(started);
    transaction(conn, |conn| {
        let mut insert = conn.prepare(
            "INSERT INTO watched_items (watch_id, source_id, url, published_at)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (watch_id, source_id) DO NOTHING",
        )?;
        for item in candidates {
            insert.execute(params![
                watch.id,
                item.source_id,
                item.url,
                fmt_ts(item.published_at)
            ])?;
        }
        conn.execute(
            "UPDATE watched_sources SET checkpoint = ?1 WHERE id = ?2",
            params![fmt_ts(checkpoint), watch.id],
        )?;
        Ok(())
    })
}

pub fn pending(conn: &Connection, watch_id: i64, limit: i64) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT source_id, url FROM watched_items WHERE watch_id = ?1 AND handled = 0
         ORDER BY last_attempt_job_id, published_at, source_id LIMIT ?2",
    )?;
    Ok(stmt
        .query_map(params![watch_id, limit], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn attempted(
    conn: &Connection,
    watch_id: i64,
    source_id: &str,
    job_id: i64,
    handled: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE watched_items SET last_attempt_job_id = ?3, handled = ?4
         WHERE watch_id = ?1 AND source_id = ?2",
        params![watch_id, source_id, job_id, handled],
    )?;
    Ok(())
}

pub fn pending_count(conn: &Connection, watch_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM watched_items WHERE watch_id = ?1 AND handled = 0",
        [watch_id],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_scan_can_discover_more_than_one_download_batch() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.with(|conn| {
            create(conn, "Artist", "https://x.com/artist/media", 10)?;
            let at = DateTime::from_timestamp(1_750_000_000, 0).unwrap();
            let discovered: Vec<_> = (1..=30)
                .rev()
                .map(|id| Candidate {
                    source_id: format!("{id:02}"),
                    url: format!("https://x.com/i/web/status/{id}"),
                    published_at: at,
                })
                .collect();
            let watch = get(conn, 1)?;
            record_scan(conn, &watch, &discovered, at)?;
            assert_eq!(get(conn, 1)?.checkpoint, Some(at));
            let first = pending(conn, 1, 25)?;
            assert_eq!(first.len(), 25);
            for (id, _) in first {
                attempted(conn, 1, &id, 1, id != "01")?;
            }
            let second = pending(conn, 1, 25)?;
            assert_eq!(second.len(), 6);
            assert_eq!(second[0].0, "26");
            assert_eq!(second[5].0, "01");
            Ok(())
        })
        .unwrap();
    }
}
