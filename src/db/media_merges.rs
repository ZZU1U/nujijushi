//! The retained source gallery and its visible image edits.

use std::collections::HashMap;
use std::ops::RangeInclusive;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    jobs,
    posts::{self, NewMedia},
};
use crate::blob::{BlobStore, Ingested};
use crate::error::{Error, Result};
use crate::media::compress::Compression;
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
                    mime: Some(
                        if row.get_ref(6)?.as_str()? == "jpg" {
                            "image/jpeg"
                        } else {
                            "image/png"
                        }
                        .into(),
                    ),
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

fn compressions(conn: &Connection, post_id: i64) -> Result<HashMap<usize, NewMedia>> {
    let mut statement = conn.prepare(
        "SELECT c.first_ordinal, c.blob_hash, c.width, c.height, b.bytes
         FROM media_compressions c JOIN blobs b ON b.hash = c.blob_hash WHERE c.post_id = ?1",
    )?;
    Ok(statement
        .query_map([post_id], |row| {
            Ok((
                row.get::<_, u32>(0)? as usize,
                NewMedia {
                    blob_hash: row.get(1)?,
                    width: row.get(2)?,
                    height: row.get(3)?,
                    bytes: row.get(4)?,
                    ext: "jpg".into(),
                    mime: Some("image/jpeg".into()),
                    is_video: false,
                    poster_hash: None,
                    sensitive: false,
                    meta_json: None,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn rebuild(conn: &Connection, post_id: i64, sources: &[NewMedia]) -> Result<()> {
    let merges = merges(conn, post_id)?;
    let slots = layout(sources.len(), &merges)?;
    let mut merged = merges.into_iter().peekable();
    let mut compressed = compressions(conn, post_id)?;
    let mut active = Vec::with_capacity(slots.len());
    for range in slots {
        let start = *range.start();
        let mut item = match merged.next_if(|merge| *merge.range.start() == start) {
            Some(merge) => merge.item,
            None => sources[start].clone(),
        };
        item.sensitive = sources[range].iter().any(|item| item.sensitive);
        if let Some(mut replacement) = compressed.remove(&start) {
            replacement.sensitive = item.sensitive;
            replacement.meta_json = item.meta_json;
            item = replacement;
        }
        active.push(item);
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
        "DELETE FROM media_compressions WHERE post_id = ?1",
        [post_id],
    )?;
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
    let first: i64 = tx
        .query_row(
            "DELETE FROM media_merges WHERE post_id = ?1 AND id = ?2 RETURNING first_ordinal",
            params![post_id, merge_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::NotFound("This merge is no longer in the gallery".into()))?;
    delete_compressions(&tx, post_id, first, first)?;
    restore(&tx, post_id)?;
    tx.commit()?;
    Ok(())
}

fn restore(conn: &Connection, post_id: i64) -> Result<()> {
    rebuild(conn, post_id, &sources(conn, post_id)?)?;
    conn.execute(
        "DELETE FROM media_merge_sources WHERE post_id = ?1
         AND NOT EXISTS (SELECT 1 FROM media_merges WHERE post_id = ?1)
         AND NOT EXISTS (SELECT 1 FROM media_compressions WHERE post_id = ?1)",
        [post_id],
    )?;
    Ok(())
}

fn delete_compressions(conn: &Connection, post_id: i64, first: i64, last: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM media_compressions WHERE post_id = ?1 AND first_ordinal BETWEEN ?2 AND ?3",
        params![post_id, first, last],
    )?)
}

pub fn compressed_positions(conn: &Connection, post_id: i64) -> Result<Vec<usize>> {
    let compressed = compressions(conn, post_id)?;
    if compressed.is_empty() {
        return Ok(Vec::new());
    }
    let sources = sources(conn, post_id)?;
    let slots = layout(sources.len(), &merges(conn, post_id)?)?;
    Ok(slots
        .iter()
        .enumerate()
        .filter_map(|(position, range)| compressed.contains_key(range.start()).then_some(position))
        .collect())
}

fn media_position(media: &[MediaItem], media_id: i64) -> Result<usize> {
    media
        .iter()
        .position(|item| item.id == media_id)
        .ok_or_else(|| Error::invalid("The image is no longer in the gallery"))
}

fn source_position(
    source_count: usize,
    merges: &[Merge],
    visible_position: usize,
) -> Result<usize> {
    layout(source_count, merges)?
        .get(visible_position)
        .map(|range| *range.start())
        .ok_or_else(|| Error::other("invalid gallery edit layout"))
}

pub fn enqueue_compression(
    conn: &mut Connection,
    post_id: i64,
    media_id: i64,
    version: &str,
    settings: Compression,
) -> Result<bool> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&media, version)?;
    let index = media_position(&media, media_id)?;
    if !crate::media::merge::supports(&media[index]) {
        return Err(Error::invalid("Select a still JPEG, PNG or WebP image"));
    }
    if latest_job(&tx, post_id)?.is_some_and(|job| job.is_active()) {
        return Ok(false);
    }
    let payload = format!(
        "{media_id}:{version}:{}:{}",
        settings.quality(),
        settings.fits_telegram(),
    );
    let job_id = jobs::create(&tx, "compress_media", &payload, 1)?;
    jobs::attach_post(&tx, job_id, post_id)?;
    tx.commit()?;
    Ok(true)
}

pub fn compression_input(
    conn: &Connection,
    post_id: i64,
    media_id: i64,
    version: &str,
) -> Result<NewMedia> {
    let media = posts::media(conn, post_id)?;
    verify_gallery(&media, version)?;
    let index = media_position(&media, media_id)?;
    let retained = sources(conn, post_id)?;
    if retained.is_empty() {
        return Ok(NewMedia::from(&media[index]));
    }
    let merges = merges(conn, post_id)?;
    let source = source_position(retained.len(), &merges, index)?;
    Ok(merges
        .into_iter()
        .find(|merge| *merge.range.start() == source)
        .map_or_else(|| retained[source].clone(), |merge| merge.item))
}

pub fn finish_compression(
    conn: &mut Connection,
    store: &BlobStore,
    job_id: i64,
    intent: &Intent<'_>,
    rendition: Rendition<'_>,
) -> Result<String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_running(&tx, job_id)?;
    let media = posts::media(&tx, intent.post_id)?;
    verify_gallery(&media, intent.version)?;
    let index = media_position(&media, intent.first_id)?;
    let (output, old_bytes) = (rendition.bytes, media[index].bytes);
    if &*blake3::hash(output).to_hex() == media[index].blob_hash.as_str() {
        let message = "The gallery already has this compressed image".to_string();
        jobs::progress(&tx, job_id, 1, 1)?;
        jobs::set_state(&tx, job_id, "done", Some(&message))?;
        tx.commit()?;
        return Ok(message);
    }
    let sources = retain_sources(&tx, intent.post_id, &media)?;
    let first = source_position(sources.len(), &merges(&tx, intent.post_id)?, index)?;
    let output = register(&tx, store, rendition)?;
    tx.execute(
        "INSERT INTO media_compressions (post_id, first_ordinal, blob_hash, width, height)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (post_id, first_ordinal) DO UPDATE SET
            blob_hash = excluded.blob_hash, width = excluded.width, height = excluded.height",
        params![
            intent.post_id,
            first as i64,
            output.hash,
            output.width,
            output.height
        ],
    )?;
    rebuild(&tx, intent.post_id, &sources)?;
    let message = format!(
        "Saved {}×{} JPEG · {:.1} → {:.1} KiB",
        output.width.unwrap_or_default(),
        output.height.unwrap_or_default(),
        old_bytes as f64 / 1024.0,
        output.bytes as f64 / 1024.0,
    );
    jobs::progress(&tx, job_id, 1, 1)?;
    jobs::set_state(&tx, job_id, "done", Some(&message))?;
    tx.commit()?;
    Ok(message)
}

pub fn undo_compression(
    conn: &mut Connection,
    post_id: i64,
    media_id: i64,
    version: &str,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&media, version)?;
    let index = media_position(&media, media_id)?;
    let sources = sources(&tx, post_id)?;
    let missing = || Error::NotFound("This compression is no longer in the gallery".into());
    if sources.is_empty() {
        return Err(missing());
    }
    let first = source_position(sources.len(), &merges(&tx, post_id)?, index)? as i64;
    if delete_compressions(&tx, post_id, first, first)? == 0 {
        return Err(missing());
    }
    restore(&tx, post_id)?;
    tx.commit()?;
    Ok(())
}

pub struct Rendition<'a> {
    pub bytes: &'a [u8],
    pub ext: &'a str,
    pub thumbnail: &'a [u8],
}

fn register(conn: &Connection, store: &BlobStore, rendition: Rendition<'_>) -> Result<Ingested> {
    let output = store.ingest_bytes(rendition.bytes, rendition.ext)?;
    store.record(conn, &output)?;
    store.write_thumb_bytes(&output.hash, rendition.thumbnail)?;
    Ok(output)
}

fn verify_running(conn: &Connection, job_id: i64) -> Result<()> {
    if jobs::get(conn, job_id)?.is_none_or(|job| job.state != "running") {
        return Err(Error::invalid(
            "The gallery edit was interrupted. Reload the post and try again.",
        ));
    }
    Ok(())
}

fn retain_sources(conn: &Connection, post_id: i64, media: &[MediaItem]) -> Result<Vec<NewMedia>> {
    let mut retained = sources(conn, post_id)?;
    if retained.is_empty() {
        retained = media.iter().map(NewMedia::from).collect();
        save_sources(conn, post_id, &retained)?;
    }
    Ok(retained)
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
    let last_of_kind = |kind| -> Result<Option<i64>> {
        Ok(conn
            .query_row(kind, [post_id], |row| row.get(0))
            .optional()?)
    };
    let merge = last_of_kind(
        "SELECT id FROM jobs WHERE post_id = ?1 AND kind = 'merge_media' ORDER BY id DESC LIMIT 1",
    )?;
    let compression = last_of_kind(
        "SELECT id FROM jobs WHERE post_id = ?1 AND kind = 'compress_media' ORDER BY id DESC LIMIT 1",
    )?;
    merge
        .max(compression)
        .map(|id| jobs::get(conn, id))
        .transpose()
        .map(Option::flatten)
}

pub fn enqueue(
    conn: &mut Connection,
    post_id: i64,
    selected: &[i64],
    version: &str,
    compression: Option<Compression>,
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
    let mut payload = format!("{}:{}:{version}", media[*first].id, media[*last].id);
    if let Some(settings) = compression {
        payload.push_str(&format!(
            ":{}:{}",
            settings.quality(),
            settings.fits_telegram(),
        ));
    }
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
    rendition: Rendition<'_>,
) -> Result<String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_running(&tx, job_id)?;
    let post_id = intent.post_id;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&media, intent.version)?;
    let range = range(&media, intent.first_id, intent.last_id)?;
    let sources = retain_sources(&tx, post_id, &media)?;
    let slots = layout(sources.len(), &merges(&tx, post_id)?)?;
    if slots.len() != media.len() {
        return Err(Error::other("invalid gallery merge layout"));
    }
    let first = *slots[*range.start()].start() as i64;
    let last = *slots[*range.end()].end() as i64;
    let output = register(&tx, store, rendition)?;
    delete_compressions(&tx, post_id, first, last)?;
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
           AND NOT EXISTS (SELECT 1 FROM media_merges m WHERE m.blob_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media_compressions c WHERE c.blob_hash = b.hash)";

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
