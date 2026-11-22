//! Persistent image fingerprints, candidate matches, and moderation dismissals.

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::error::Result;

const PENDING: &str = "m.duplicate_indexed = 0 AND m.is_video = 0
    AND b.ext IN ('jpg', 'jpeg', 'png', 'webp')";
const WAITING_FROM: &str = "
    FROM media m JOIN blobs b ON b.hash = m.blob_hash
    LEFT JOIN duplicate_fingerprint_failures failure ON failure.blob_hash = m.blob_hash";
const READY: &str = "(failure.retry_at IS NULL OR failure.retry_at <= unixepoch())";

#[derive(Debug, Clone, Copy)]
pub struct Fingerprint {
    pub hash: i64,
    pub aspect: f64,
}

#[derive(Debug)]
pub struct PendingMedia {
    pub id: i64,
    pub post_id: i64,
    pub blob_hash: String,
    pub ext: String,
    pub fingerprinted: bool,
    pub fingerprint: Option<Fingerprint>,
}

pub fn pending(conn: &Connection, limit: i64) -> Result<Vec<PendingMedia>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT m.id, m.post_id, b.hash, b.ext, f.blob_hash IS NOT NULL, f.hash, f.aspect
         {WAITING_FROM}
         LEFT JOIN image_fingerprints f ON f.blob_hash = b.hash
         WHERE {PENDING} AND {READY} ORDER BY m.id LIMIT ?1"
    ))?;
    Ok(stmt
        .query_map([limit], |row| {
            let hash: Option<i64> = row.get(5)?;
            let aspect: Option<f64> = row.get(6)?;
            Ok(PendingMedia {
                id: row.get(0)?,
                post_id: row.get(1)?,
                blob_hash: row.get(2)?,
                ext: row.get(3)?,
                fingerprinted: row.get(4)?,
                fingerprint: hash
                    .zip(aspect)
                    .map(|(hash, aspect)| Fingerprint { hash, aspect }),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn has_pending(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 {WAITING_FROM} WHERE {PENDING} AND {READY})"),
        [],
        |row| row.get(0),
    )?)
}

pub(crate) struct Progress {
    pub pending: i64,
    pub failed: i64,
}

pub(crate) fn progress(conn: &Connection) -> Result<Progress> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FILTER (WHERE {READY}),
                    COUNT(*) FILTER (WHERE failure.retry_at > unixepoch())
             {WAITING_FROM} WHERE {PENDING}"
        ),
        [],
        |row| {
            Ok(Progress {
                pending: row.get(0)?,
                failed: row.get(1)?,
            })
        },
    )?)
}

pub(crate) fn retry_failed(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        &format!(
            "UPDATE duplicate_fingerprint_failures SET retry_at = 0
             WHERE retry_at > unixepoch() AND blob_hash IN (
                 SELECT m.blob_hash FROM media m JOIN blobs b ON b.hash = m.blob_hash
                 WHERE {PENDING}
             )"
        ),
        [],
    )?)
}

pub(crate) fn record_failure(conn: &Connection, item: &PendingMedia) -> Result<bool> {
    // Back off per blob, so more copies of one unreadable image do not cause more retries.
    Ok(conn.execute(
        "INSERT INTO duplicate_fingerprint_failures (blob_hash, attempts, retry_at)
         SELECT ?3, 1, unixepoch() + 15
         WHERE EXISTS (
             SELECT 1 FROM media WHERE id = ?1 AND post_id = ?2 AND blob_hash = ?3
             AND duplicate_indexed = 0 AND is_video = 0
         ) AND NOT EXISTS (SELECT 1 FROM image_fingerprints WHERE blob_hash = ?3)
         ON CONFLICT (blob_hash) DO UPDATE SET
             attempts = attempts + 1,
             retry_at = unixepoch() + MIN(3600, 15 * (1 << MIN(attempts, 8)))
         WHERE retry_at <= unixepoch()",
        params![item.id, item.post_id, item.blob_hash],
    )? > 0)
}

/// Commit progress and matches together, discarding results for media that were replaced.
pub fn finish(
    conn: &mut Connection,
    item: &PendingMedia,
    fingerprint: Option<Fingerprint>,
) -> Result<()> {
    super::transaction(conn, |conn| {
        let marked = conn.execute(
            "UPDATE media SET duplicate_indexed = 1 WHERE id = ?1 AND post_id = ?2
             AND blob_hash = ?3 AND duplicate_indexed = 0 AND is_video = 0",
            params![item.id, item.post_id, item.blob_hash],
        )?;
        if marked == 0 {
            return Ok(());
        }
        conn.execute(
            "INSERT INTO image_fingerprints (blob_hash, hash, aspect) VALUES (?1, ?2, ?3)
             ON CONFLICT (blob_hash) DO NOTHING",
            params![
                item.blob_hash,
                fingerprint.map(|fp| fp.hash),
                fingerprint.map(|fp| fp.aspect)
            ],
        )?;
        conn.execute(
            "DELETE FROM duplicate_fingerprint_failures WHERE blob_hash = ?1",
            [&item.blob_hash],
        )?;
        if let Some(fingerprint) = fingerprint {
            for (hash, distance) in near_candidates(conn, item, fingerprint)? {
                insert_matches(conn, item, &hash, i64::from(distance) + 1)?;
            }
        }
        Ok(())
    })
}

fn near_candidates(
    conn: &Connection,
    item: &PendingMedia,
    fp: Fingerprint,
) -> Result<Vec<(String, u32)>> {
    // At distance <= 5, at least one of the four bands differs by at most one bit.
    let placeholders = vec!["?"; 17].join(", ");
    let filters = (0..4)
        .map(|band| format!("band{band} IN ({placeholders})"))
        .collect::<Vec<_>>();
    let sql = format!(
        "SELECT blob_hash, hash, aspect FROM image_fingerprints WHERE ({}) AND blob_hash <> ?",
        filters.join(" OR ")
    );
    let mut values = Vec::with_capacity(69);
    for band in 0..4 {
        let value = (fp.hash >> (band * 16)) & 65535;
        values.push(rusqlite::types::Value::Integer(value));
        for bit in 0..16 {
            values.push(rusqlite::types::Value::Integer(value ^ (1 << bit)));
        }
    }
    values.push(item.blob_hash.clone().into());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, f64>(2)?,
        ))
    })?;
    let mut candidates = Vec::new();
    for row in rows {
        let (hash, other, aspect) = row?;
        let distance = (fp.hash ^ other).count_ones();
        if distance <= 5 && (fp.aspect.max(aspect) / fp.aspect.min(aspect)) <= 1.10 {
            candidates.push((hash, distance));
        }
    }
    Ok(candidates)
}

fn insert_matches(
    conn: &Connection,
    item: &PendingMedia,
    other_hash: &str,
    score: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO duplicate_matches
         (left_media_id, right_media_id, left_post_id, right_post_id, distance)
         SELECT CASE WHEN other.post_id < current.post_id THEN other.id ELSE current.id END,
                CASE WHEN other.post_id < current.post_id THEN current.id ELSE other.id END,
                MIN(other.post_id, current.post_id), MAX(other.post_id, current.post_id), ?3
         FROM media current JOIN media other ON other.blob_hash = ?2
         JOIN posts a ON a.id = current.post_id JOIN posts b ON b.id = other.post_id
         WHERE current.id = ?1 AND other.duplicate_indexed = 1 AND a.source <> b.source
           AND NOT EXISTS (
               SELECT 1 FROM duplicate_dismissals d
               WHERE d.left_post_id = MIN(other.post_id, current.post_id)
                 AND d.right_post_id = MAX(other.post_id, current.post_id)
           )
         ON CONFLICT (left_media_id, right_media_id) DO NOTHING",
        params![item.id, other_hash, score],
    )?;
    Ok(())
}

pub const PAGE_SIZE: i64 = 20;

// Match archived hashes independently of fingerprints, including originals
// displaced by a merge or compression. Use explicit merge membership for gapped merges.
const EXACT_MATCHES: &str = "
    WITH originals AS NOT MATERIALIZED (
        SELECT s.post_id, s.ordinal, s.blob_hash FROM media_merge_sources s
        WHERE EXISTS (
            SELECT 1 FROM media_merges m JOIN media_merge_members mm ON mm.merge_id = m.id
            WHERE m.post_id = s.post_id AND mm.source_ordinal = s.ordinal
        ) OR EXISTS (
            SELECT 1 FROM media_compressions c WHERE c.post_id = s.post_id
            AND c.first_ordinal = s.ordinal
        )
    ), exact (left_post_id, right_post_id, left_ordinal, right_ordinal, blob_hash) AS NOT MATERIALIZED (
        SELECT a.post_id, b.post_id, a.ordinal, b.ordinal, a.blob_hash
        FROM media a JOIN media b USING (blob_hash) WHERE a.post_id < b.post_id
        UNION ALL
        SELECT s.post_id, m.post_id, s.ordinal, m.ordinal, s.blob_hash
        FROM originals s JOIN media m USING (blob_hash) WHERE s.post_id < m.post_id
        UNION ALL
        SELECT m.post_id, s.post_id, m.ordinal, s.ordinal, s.blob_hash
        FROM media m JOIN originals s USING (blob_hash) WHERE m.post_id < s.post_id
        UNION ALL
        SELECT s.post_id, t.post_id, s.ordinal, t.ordinal, s.blob_hash
        FROM originals s JOIN originals t USING (blob_hash) WHERE s.post_id < t.post_id
    )
";

pub fn pairs(conn: &Connection, offset: i64) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare(&format!(
        "{EXACT_MATCHES}
         SELECT m.left_post_id, m.right_post_id FROM (
             SELECT left_post_id, right_post_id, distance FROM duplicate_matches
             UNION ALL
             SELECT left_post_id, right_post_id, 0 FROM exact
         ) m
         WHERE NOT EXISTS (SELECT 1 FROM duplicate_dismissals d
               WHERE d.left_post_id = m.left_post_id AND d.right_post_id = m.right_post_id)
         GROUP BY m.left_post_id, m.right_post_id
         ORDER BY MIN(m.distance), m.right_post_id DESC, m.left_post_id DESC
         LIMIT ?1 OFFSET ?2",
    ))?;
    Ok(stmt
        .query_map(params![PAGE_SIZE + 1, offset], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, serde::Serialize)]
pub struct Evidence {
    pub left_hash: String,
    pub right_hash: String,
    pub left_position: i64,
    pub right_position: i64,
    pub exact: bool,
}

pub fn evidence(conn: &Connection, left: i64, right: i64) -> Result<Vec<Evidence>> {
    let mut stmt = conn.prepare(&format!(
        "{EXACT_MATCHES}
         SELECT a.blob_hash, b.blob_hash, a.ordinal AS left_ordinal,
                b.ordinal AS right_ordinal, m.distance AS distance
         FROM duplicate_matches m JOIN media a ON a.id = m.left_media_id
         JOIN media b ON b.id = m.right_media_id
         WHERE m.left_post_id = ?1 AND m.right_post_id = ?2
         UNION
         SELECT blob_hash, blob_hash, left_ordinal, right_ordinal, 0
         FROM exact WHERE left_post_id = ?1 AND right_post_id = ?2
         ORDER BY distance, left_ordinal, right_ordinal",
    ))?;
    Ok(stmt
        .query_map(params![left, right], |row| {
            Ok(Evidence {
                left_hash: row.get(0)?,
                right_hash: row.get(1)?,
                left_position: row.get::<_, i64>(2)? + 1,
                right_position: row.get::<_, i64>(3)? + 1,
                exact: row.get::<_, i64>(4)? == 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn dismiss(conn: &Connection, first: i64, second: i64) -> Result<bool> {
    let (left, right) = (first.min(second), first.max(second));
    Ok(conn
        .query_row(
            &format!(
                "{EXACT_MATCHES}
            INSERT INTO duplicate_dismissals (left_post_id, right_post_id)
         SELECT ?1, ?2 WHERE EXISTS (
             SELECT 1 FROM duplicate_matches WHERE left_post_id = ?1 AND right_post_id = ?2
             UNION ALL
             SELECT 1 FROM exact WHERE left_post_id = ?1 AND right_post_id = ?2
         )
         ON CONFLICT (left_post_id, right_post_id) DO NOTHING RETURNING 1"
            ),
            params![left, right],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}
