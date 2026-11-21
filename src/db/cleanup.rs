//! Select sent posts eligible for compaction and remove their disposable records.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::error::Result;

#[derive(Serialize)]
pub(crate) struct Candidate {
    pub id: i64,
    pub has_edits: bool,
    pub suggestions: i64,
    batch: Option<i64>,
    delivery: f64,
    updated_at: String,
    gallery: String,
}

pub(crate) fn candidates(conn: &Connection, cutoff: DateTime<Utc>) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(
        "WITH delivered AS (
             SELECT post_id, MAX(julianday(sent_at)) AS latest FROM (
                  SELECT post_id, sent_at FROM queue WHERE state = 'sent'
                  UNION ALL
                  SELECT post_id, sent_at FROM sends WHERE ok = 1
             ) GROUP BY post_id
         ), candidates AS (
             SELECT p.id, p.updated_at, d.latest, b.job_id,
                    EXISTS(SELECT 1 FROM media_merge_sources WHERE post_id = p.id) AS has_edits,
                    (SELECT COUNT(*) FROM post_tag_suggestions WHERE job_id = b.job_id) AS suggestions
               FROM posts p JOIN delivered d ON p.id = d.post_id
               LEFT JOIN post_tag_suggestion_batches b ON b.post_id = p.id
              WHERE d.latest BETWEEN julianday(p.imported_at) AND julianday(?1)
                AND NOT EXISTS(SELECT 1 FROM queue q WHERE q.post_id = p.id
                    AND q.state NOT IN ('sent', 'skipped'))
                AND NOT EXISTS(SELECT 1 FROM mirrors m WHERE m.post_id = p.id AND m.state <> 'done')
                AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.post_id = p.id
                    AND j.kind IN ('merge_media', 'compress_media', 'suggest_tags')
                    AND j.state IN ('pending', 'running'))
         )
         SELECT id, has_edits, suggestions, job_id, latest, updated_at FROM candidates
          WHERE has_edits OR job_id IS NOT NULL ORDER BY id",
    )?;
    let rows = stmt.query_map([super::fmt_ts(cutoff)], |row| {
        Ok(Candidate {
            id: row.get(0)?,
            has_edits: row.get(1)?,
            suggestions: row.get(2)?,
            batch: row.get(3)?,
            delivery: row.get(4)?,
            updated_at: row.get(5)?,
            gallery: String::new(),
        })
    })?;
    let mut candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    for post in &mut candidates {
        post.gallery = super::media_merges::current_revision(conn, post.id)?;
    }
    Ok(candidates)
}

pub(crate) fn apply(conn: &Connection, candidates: &[Candidate]) -> Result<()> {
    for post in candidates {
        if post.has_edits {
            super::media_merges::compact(conn, post.id)?;
        }
        conn.execute(
            "DELETE FROM post_tag_suggestion_batches WHERE post_id = ?1",
            params![post.id],
        )?;
    }
    Ok(())
}
