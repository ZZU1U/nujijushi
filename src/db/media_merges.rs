//! The retained source gallery and the spans replaced by active merges.

use std::ops::RangeInclusive;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    jobs,
    posts::{self, NewMedia},
};
use crate::blob::BlobStore;
use crate::error::{Error, Result};
use crate::model::MediaItem;

struct Merge {
    id: i64,
    range: RangeInclusive<usize>,
    item: NewMedia,
}

fn merges(conn: &Connection, post_id: i64) -> Result<Vec<Merge>> {
    let mut statement = conn.prepare(
        "SELECT m.id, m.first_ordinal, m.last_ordinal, m.blob_hash, m.width, m.height, b.ext, b.bytes
         FROM media_merges m JOIN blobs b ON b.hash = m.blob_hash
         WHERE m.post_id = ?1 ORDER BY m.first_ordinal",
    )?;
    Ok(statement
        .query_map([post_id], |row| {
            Ok(Merge {
                id: row.get(0)?,
                range: row.get::<_, u32>(1)? as usize..=row.get::<_, u32>(2)? as usize,
                item: NewMedia {
                    blob_hash: row.get(3)?,
                    width: row.get(4)?,
                    height: row.get(5)?,
                    ext: row.get(6)?,
                    bytes: row.get(7)?,
                    mime: Some("image/png".into()),
                    is_video: false,
                    poster_hash: None,
                    sensitive: false,
                    meta_json: None,
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn sources(conn: &Connection, post_id: i64) -> Result<Vec<NewMedia>> {
    let mut statement = conn.prepare(
        "SELECT s.blob_hash, b.ext, b.bytes, s.mime, s.width, s.height,
                s.is_video, s.poster_hash, s.sensitive, s.meta_json
         FROM media_merge_sources s JOIN blobs b ON b.hash = s.blob_hash
         WHERE s.post_id = ?1 ORDER BY s.ordinal",
    )?;
    Ok(statement
        .query_map([post_id], |row| {
            Ok(NewMedia {
                blob_hash: row.get(0)?,
                ext: row.get(1)?,
                bytes: row.get(2)?,
                mime: row.get(3)?,
                width: row.get(4)?,
                height: row.get(5)?,
                is_video: row.get(6)?,
                poster_hash: row.get(7)?,
                sensitive: row.get(8)?,
                meta_json: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn save_sources(conn: &Connection, post_id: i64, sources: &[NewMedia]) -> Result<()> {
    conn.execute(
        "DELETE FROM media_merge_sources WHERE post_id = ?1",
        [post_id],
    )?;
    let mut statement = conn.prepare(
        "INSERT INTO media_merge_sources (post_id, ordinal, blob_hash, mime, width, height,
                                        is_video, poster_hash, sensitive, meta_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for (ordinal, item) in sources.iter().enumerate() {
        statement.execute(params![
            post_id,
            ordinal as i64,
            item.blob_hash,
            item.mime,
            item.width,
            item.height,
            item.is_video,
            item.poster_hash,
            item.sensitive,
            item.meta_json
        ])?;
    }
    Ok(())
}

// Each visible position covers one source image or a whole merged span.
fn layout(source_count: usize, merges: &[Merge]) -> Result<Vec<RangeInclusive<usize>>> {
    let mut slots = Vec::new();
    let mut next = 0;
    for merge in merges {
        let (first, last) = (*merge.range.start(), *merge.range.end());
        if first < next || last >= source_count || last <= first {
            return Err(Error::other("invalid gallery merge span"));
        }
        slots.extend((next..first).map(|index| index..=index));
        slots.push(merge.range.clone());
        next = last + 1;
    }
    slots.extend((next..source_count).map(|index| index..=index));
    Ok(slots)
}

fn rebuild(conn: &Connection, post_id: i64, sources: &[NewMedia]) -> Result<()> {
    let merges = merges(conn, post_id)?;
    let mut merged = merges.into_iter().peekable();
    let mut active = Vec::new();
    let mut position = 0;
    while position < sources.len() {
        if let Some(merge) = merged.next_if(|merge| *merge.range.start() == position) {
            let last = *merge.range.end();
            let mut item = merge.item;
            let panels = sources
                .get(merge.range)
                .ok_or_else(|| Error::other("invalid gallery merge span"))?;
            item.sensitive = panels.iter().any(|item| item.sensitive);
            active.push(item);
            position = last + 1;
        } else {
            active.push(sources[position].clone());
            position += 1;
        }
    }
    if merged.peek().is_some() {
        return Err(Error::other("invalid gallery merge span"));
    }
    let current = posts::media(conn, post_id)?;
    if !current
        .iter()
        .map(NewMedia::from)
        .eq(active.iter().cloned())
    {
        posts::replace_media(conn, post_id, &active)?;
    }
    Ok(())
}

/// Apply fresh source media and report when it replaces local edits.
pub(super) fn refresh(conn: &Connection, post_id: i64, imported: &[NewMedia]) -> Result<bool> {
    let previous = sources(conn, post_id)?;
    if previous.is_empty() {
        posts::replace_media(conn, post_id, imported)?;
        return Ok(false);
    }
    if previous
        .iter()
        .map(|item| &item.blob_hash)
        .eq(imported.iter().map(|item| &item.blob_hash))
    {
        // The hashes are still retained by the source gallery for this transaction.
        if previous != imported {
            save_sources(conn, post_id, imported)?;
            rebuild(conn, post_id, imported)?;
        }
        return Ok(false);
    }
    conn.execute("DELETE FROM media_merges WHERE post_id = ?1", [post_id])?;
    conn.execute(
        "DELETE FROM media_merge_sources WHERE post_id = ?1",
        [post_id],
    )?;
    posts::replace_media(conn, post_id, imported)?;
    Ok(true)
}

pub fn gallery_version(media: &[MediaItem]) -> String {
    let mut hash = blake3::Hasher::new();
    for item in media {
        hash.update(&item.id.to_le_bytes());
        hash.update(&(item.blob_hash.len() as u64).to_le_bytes());
        hash.update(item.blob_hash.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

fn verify_gallery(media: &[MediaItem], version: &str) -> Result<()> {
    if gallery_version(media) != version {
        Err(Error::invalid(
            "The gallery changed. Reload the post and select the panels again.",
        ))
    } else {
        Ok(())
    }
}

/// Pair each visible merged image with its source span's ID for the Undo form.
pub fn undo_ids(conn: &Connection, post_id: i64) -> Result<Vec<(usize, i64)>> {
    let merges = merges(conn, post_id)?;
    let mut replaced = 0;
    let mut ids = Vec::with_capacity(merges.len());
    for merge in merges {
        let position = merge
            .range
            .start()
            .checked_sub(replaced)
            .ok_or_else(|| Error::other("invalid gallery merge span"))?;
        ids.push((position, merge.id));
        replaced += merge.range.end() - merge.range.start();
    }
    Ok(ids)
}

pub fn undo(conn: &mut Connection, post_id: i64, merge_id: i64, version: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_gallery(&posts::media(&tx, post_id)?, version)?;
    if tx.execute(
        "DELETE FROM media_merges WHERE post_id = ?1 AND id = ?2",
        params![post_id, merge_id],
    )? == 0
    {
        return Err(Error::NotFound(
            "This merge is no longer in the gallery".into(),
        ));
    }
    let sources = sources(&tx, post_id)?;
    rebuild(&tx, post_id, &sources)?;
    if merges(&tx, post_id)?.is_empty() {
        tx.execute(
            "DELETE FROM media_merge_sources WHERE post_id = ?1",
            [post_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn range(media: &[MediaItem], first_id: i64, last_id: i64) -> Result<RangeInclusive<usize>> {
    let first = media.iter().position(|item| item.id == first_id);
    let last = media.iter().position(|item| item.id == last_id);
    match first.zip(last) {
        Some((first, last)) if last > first => Ok(first..=last),
        _ => Err(Error::invalid("Select at least two consecutive images")),
    }
}

pub fn latest_job(conn: &Connection, post_id: i64) -> Result<Option<jobs::Job>> {
    let id: Option<i64> = conn.query_row(
        "SELECT id FROM jobs WHERE post_id = ?1 AND kind = 'merge_media' ORDER BY id DESC LIMIT 1",
        [post_id], |row| row.get(0),
    ).optional()?;
    id.map(|id| jobs::get(conn, id))
        .transpose()
        .map(Option::flatten)
}

pub fn enqueue(
    conn: &mut Connection,
    post_id: i64,
    selected: &[i64],
    version: &str,
) -> Result<bool> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&media, version)?;
    let indices: Vec<_> = media
        .iter()
        .enumerate()
        .filter(|(_, item)| selected.contains(&item.id))
        .map(|(index, _)| index)
        .collect();
    let (first, last) = indices
        .first()
        .zip(indices.last())
        .filter(|(first, last)| {
            indices.len() == selected.len()
                && indices.len() >= 2
                && *last - *first + 1 == indices.len()
        })
        .ok_or_else(|| Error::invalid("Select at least two consecutive images"))?;
    if media[*first..=*last].iter().any(|item| item.is_video) {
        return Err(Error::invalid("A merge can only contain still images"));
    }
    if latest_job(&tx, post_id)?.is_some_and(|job| job.is_active()) {
        return Ok(false);
    }
    let payload = format!("{}:{}:{version}", media[*first].id, media[*last].id);
    let job_id = jobs::create(&tx, "merge_media", &payload, indices.len() as i64)?;
    jobs::attach_post(&tx, job_id, post_id)?;
    tx.commit()?;
    Ok(true)
}

pub struct Intent<'a> {
    pub post_id: i64,
    pub first_id: i64,
    pub last_id: i64,
    pub version: &'a str,
}

pub fn panels(conn: &Connection, intent: &Intent<'_>) -> Result<Vec<MediaItem>> {
    let media = posts::media(conn, intent.post_id)?;
    verify_gallery(&media, intent.version)?;
    let range = range(&media, intent.first_id, intent.last_id)?;
    Ok(media[range].to_vec())
}

pub fn finish(
    conn: &mut Connection,
    store: &BlobStore,
    job_id: i64,
    intent: &Intent<'_>,
    png: &[u8],
    thumbnail: &[u8],
) -> Result<String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if jobs::get(&tx, job_id)?.is_none_or(|job| job.state != "running") {
        return Err(Error::invalid(
            "This merge was interrupted. Select the panels again.",
        ));
    }
    let post_id = intent.post_id;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&media, intent.version)?;
    let range = range(&media, intent.first_id, intent.last_id)?;
    let mut sources = sources(&tx, post_id)?;
    if sources.is_empty() {
        sources = media.iter().map(NewMedia::from).collect();
        save_sources(&tx, post_id, &sources)?;
    }
    let slots = layout(sources.len(), &merges(&tx, post_id)?)?;
    if slots.len() != media.len() {
        return Err(Error::other("invalid gallery merge layout"));
    }
    let first = *slots[*range.start()].start() as i64;
    let last = *slots[*range.end()].end() as i64;
    let output = store.ingest_bytes(png, "png")?;
    store.record(&tx, &output)?;
    store.write_thumb_bytes(&output.hash, thumbnail)?;
    tx.execute(
        "DELETE FROM media_merges WHERE post_id = ?1 AND first_ordinal BETWEEN ?2 AND ?3",
        params![post_id, first, last],
    )?;
    tx.execute(
        "INSERT INTO media_merges (post_id, first_ordinal, last_ordinal, blob_hash, width, height)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            post_id,
            first,
            last,
            output.hash,
            output.width,
            output.height
        ],
    )?;
    rebuild(&tx, post_id, &sources)?;
    let message = format!("Merged {} images", range.count());
    jobs::set_state(&tx, job_id, "done", Some(&message))?;
    tx.commit()?;
    Ok(message)
}

const ORPHAN_BLOBS: &str = "FROM blobs b
         WHERE NOT EXISTS (SELECT 1 FROM media m WHERE m.blob_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media m WHERE m.poster_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media_merge_sources s WHERE s.blob_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media_merge_sources s WHERE s.poster_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media_merges m WHERE m.blob_hash = b.hash)";

pub fn orphan_blobs(conn: &Connection) -> Result<Vec<(String, String, i64)>> {
    let mut statement = conn.prepare(&format!("SELECT b.hash, b.ext, b.bytes {ORPHAN_BLOBS}"))?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn orphan_blob_totals(conn: &Connection) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*), COALESCE(SUM(b.bytes), 0) {ORPHAN_BLOBS}"),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}
