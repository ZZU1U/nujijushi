//! Watched feeds and the durable queue of discovered artwork.

use std::collections::HashSet;

use chrono::{DateTime, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::error::{Error, Result};
use crate::source::pixiv::CreatorScan;
use crate::source::watch::Candidate;

use super::{fmt_ts, get_ts, jobs, transaction};

const PIXIV_MISSING_LIMIT: i64 = 2;

#[derive(Debug)]
pub struct Watch {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub initial_batch: u16,
    pub enabled: bool,
    pub checkpoint: Option<DateTime<Utc>>,
    pub last_job_id: Option<i64>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub(crate) pixiv_baselined: bool,
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
        last_run_at: get_ts(row, "last_run_at")?,
        pixiv_baselined: row.get("pixiv_baselined")?,
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
    let id = queue_check(&tx, watch_id, kind, &payload)?;
    tx.commit()?;
    Ok((id, true))
}

pub(crate) fn enqueue_due(conn: &mut Connection, now: DateTime<Utc>, kind: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cutoff = fmt_ts(now - TimeDelta::days(1));
    let due = {
        let mut stmt = tx.prepare(
            "SELECT w.id FROM watched_sources w
             WHERE w.enabled = 1 AND (w.last_run_at IS NULL OR julianday(w.last_run_at) <= julianday(?1))
               AND NOT EXISTS (
                 SELECT 1 FROM jobs WHERE kind = ?2 AND payload = CAST(w.id AS TEXT)
                   AND state IN ('pending', 'running')
               )
             ORDER BY julianday(w.last_run_at), w.id",
        )?;
        stmt.query_map(params![cutoff, kind], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for watch_id in due {
        queue_check(&tx, watch_id, kind, &watch_id.to_string())?;
    }
    tx.commit()?;
    Ok(())
}

fn queue_check(conn: &Connection, watch_id: i64, kind: &str, payload: &str) -> Result<i64> {
    let id = jobs::create(conn, kind, payload, 0)?;
    conn.execute(
        "UPDATE watched_sources SET last_job_id = ?1 WHERE id = ?2",
        params![id, watch_id],
    )?;
    Ok(id)
}

pub(crate) fn start_check(conn: &Connection, id: i64, at: DateTime<Utc>) -> Result<Watch> {
    conn.query_row(
        "UPDATE watched_sources SET last_run_at = ?2 WHERE id = ?1 RETURNING *",
        params![id, fmt_ts(at)],
        from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("watch {id}")))
}

pub fn record_scan(
    conn: &mut Connection,
    watch: &Watch,
    candidates: &[Candidate],
    started: DateTime<Utc>,
) -> Result<()> {
    transaction(conn, |conn| {
        record_discoveries(conn, watch, candidates, started)?;
        conn.execute(
            "UPDATE watched_items SET pixiv_missing_scans = 0
             WHERE watch_id = ?1 AND handled = 0 AND pixiv_missing_scans > 0",
            [watch.id],
        )?;
        Ok(())
    })
}

pub(crate) fn pixiv_seen(conn: &Connection, watch_id: i64) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT source_id FROM watched_pixiv_seen WHERE watch_id = ?1")?;
    Ok(stmt
        .query_map([watch_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn record_pixiv_scan(
    conn: &mut Connection,
    watch: &Watch,
    scan: &CreatorScan,
    seen: Option<&HashSet<String>>,
    started: DateTime<Utc>,
) -> Result<()> {
    transaction(conn, |conn| {
        record_discoveries(conn, watch, &scan.candidates, started)?;
        let mut insert = conn.prepare(
            "INSERT INTO watched_pixiv_seen (watch_id, source_id) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
        )?;
        for id in &scan.visible {
            if seen.is_none_or(|seen| !seen.contains(id)) {
                insert.execute(params![watch.id, id])?;
            }
        }
        if !watch.pixiv_baselined {
            conn.execute(
                "UPDATE watched_sources SET pixiv_baselined = 1 WHERE id = ?1",
                [watch.id],
            )?;
        }
        update_pixiv_absences(conn, watch.id, &scan.visible)
    })
}

fn update_pixiv_absences(
    conn: &Connection,
    watch_id: i64,
    visible: &HashSet<String>,
) -> Result<()> {
    let mut select = conn.prepare(
        "SELECT source_id, pixiv_missing_scans FROM watched_items
         WHERE watch_id = ?1 AND handled = 0",
    )?;
    let pending = select
        .query_map([watch_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut update = conn.prepare(
        "UPDATE watched_items SET pixiv_missing_scans = ?3
         WHERE watch_id = ?1 AND source_id = ?2",
    )?;
    for (id, misses) in pending {
        let next = if visible.contains(&id) {
            0
        } else {
            (misses + 1).min(PIXIV_MISSING_LIMIT)
        };
        if next != misses {
            update.execute(params![watch_id, id, next])?;
        }
    }
    Ok(())
}

fn record_discoveries(
    conn: &Connection,
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
    if watch.checkpoint != Some(checkpoint) {
        conn.execute(
            "UPDATE watched_sources SET checkpoint = ?1 WHERE id = ?2",
            params![fmt_ts(checkpoint), watch.id],
        )?;
    }
    Ok(())
}

pub fn pending(
    conn: &Connection,
    watch_id: i64,
    limit: i64,
    defer_absent: bool,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT source_id, url FROM watched_items WHERE watch_id = ?1 AND handled = 0
         AND (NOT ?3 OR pixiv_missing_scans < ?4)
         ORDER BY last_attempt_job_id, published_at, source_id LIMIT ?2",
    )?;
    Ok(stmt
        .query_map(
            params![watch_id, limit, defer_absent, PIXIV_MISSING_LIMIT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
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

pub(crate) fn pixiv_deferred_count(conn: &Connection, watch_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM watched_items WHERE watch_id = ?1 AND handled = 0
         AND pixiv_missing_scans >= ?2",
        params![watch_id, PIXIV_MISSING_LIMIT],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_checks_use_the_actual_start_and_skip_paused_or_active_watches() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.with(|conn| {
            let kind = crate::jobs::KIND_WATCH_CHECK;
            let now = DateTime::from_timestamp(1_750_000_000, 0).unwrap();
            let day = TimeDelta::days(1);
            create(conn, "Artist", "https://x.com/artist/media", 10)?;
            create(conn, "Paused", "https://x.com/paused/media", 10)?;
            set_enabled(conn, 2, false)?;

            enqueue_due(conn, now, kind)?;
            let first = get(conn, 1)?.last_job_id.unwrap();
            assert_eq!(get(conn, 1)?.last_run_at, None);
            assert_eq!(get(conn, 2)?.last_job_id, None);
            enqueue_due(conn, now + day, kind)?;
            assert_eq!(enqueue(conn, 1, kind)?, (first, false));

            jobs::set_state(conn, first, "running", None)?;
            let started = now + day;
            start_check(conn, 1, started)?;
            enqueue_due(conn, started + day, kind)?;
            assert_eq!(get(conn, 1)?.last_job_id, Some(first));
            jobs::set_state(conn, first, "failed", None)?;

            enqueue_due(conn, started + day - TimeDelta::seconds(1), kind)?;
            assert_eq!(get(conn, 1)?.last_job_id, Some(first));
            let (manual, queued) = enqueue(conn, 1, kind)?;
            assert!(queued);
            assert_eq!(get(conn, 1)?.last_run_at, Some(started));
            let manual_started = started + TimeDelta::hours(2);
            start_check(conn, 1, manual_started)?;
            jobs::set_state(conn, manual, "done", None)?;
            enqueue_due(conn, started + day, kind)?;
            assert_eq!(get(conn, 1)?.last_job_id, Some(manual));

            set_enabled(conn, 2, true)?;
            enqueue_due(conn, manual_started + day, kind)?;
            assert_ne!(get(conn, 1)?.last_job_id, Some(manual));
            assert_eq!(get(conn, 1)?.last_run_at, Some(manual_started));
            assert!(get(conn, 2)?.last_job_id.is_some());
            assert_eq!(jobs::count_pending(conn, kind)?, 2);
            Ok(())
        })
        .unwrap();
    }

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
            let first = pending(conn, 1, 25, false)?;
            assert_eq!(first.len(), 25);
            for (id, _) in first {
                attempted(conn, 1, &id, 1, id != "01")?;
            }
            let second = pending(conn, 1, 25, false)?;
            assert_eq!(second.len(), 6);
            assert_eq!(second[0].0, "26");
            assert_eq!(second[5].0, "01");
            Ok(())
        })
        .unwrap();
    }
}
