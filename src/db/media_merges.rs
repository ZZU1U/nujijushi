//! The retained source gallery and its visible image edits.

use std::collections::{HashMap, HashSet};

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
    members: Vec<usize>,
    item: NewMedia,
}

fn merges(conn: &Connection, post_id: i64) -> Result<Vec<Merge>> {
    let mut statement = conn.prepare(
        "SELECT m.id, m.first_ordinal, m.blob_hash, m.width, m.height, b.ext, b.bytes
         FROM media_merges m JOIN blobs b ON b.hash = m.blob_hash
         WHERE m.post_id = ?1 ORDER BY m.first_ordinal",
    )?;
    let rows = statement.query_map([post_id], |row| {
        Ok(Merge {
            id: row.get(0)?,
            members: Vec::new(),
            item: NewMedia {
                blob_hash: row.get(2)?,
                width: row.get(3)?,
                height: row.get(4)?,
                ext: row.get(5)?,
                bytes: row.get(6)?,
                mime: Some(
                    if row.get_ref(5)?.as_str()? == "jpg" {
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
    })?;
    let mut membership = conn.prepare(
        "SELECT source_ordinal FROM media_merge_members WHERE merge_id = ?1 ORDER BY source_ordinal",
    )?;
    let mut out = Vec::new();
    for row in rows {
        let mut merge = row?;
        merge.members = membership
            .query_map([merge.id], |row| Ok(row.get::<_, u32>(0)? as usize))?
            .collect::<rusqlite::Result<_>>()?;
        out.push(merge);
    }
    Ok(out)
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

// A merged image takes the position of its first selected source image.
fn layout(source_count: usize, merges: &[Merge]) -> Result<Vec<Vec<usize>>> {
    let mut starts = HashMap::new();
    let mut covered = HashSet::new();
    for merge in merges {
        let [first, _, ..] = merge.members.as_slice() else {
            return Err(Error::other("invalid gallery merge membership"));
        };
        for &member in &merge.members {
            if member >= source_count || !covered.insert(member) {
                return Err(Error::other("overlapping or missing gallery merge member"));
            }
        }
        starts.insert(*first, &merge.members);
    }
    Ok((0..source_count)
        .filter_map(|index| {
            starts
                .get(&index)
                .map(|members| (*members).clone())
                .or_else(|| (!covered.contains(&index)).then(|| vec![index]))
        })
        .collect())
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
    for members in slots {
        let start = members[0];
        let mut item = match merged.next_if(|merge| merge.members[0] == start) {
            Some(merge) => merge.item,
            None => sources[start].clone(),
        };
        item.sensitive = members.iter().any(|&index| sources[index].sensitive);
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
        update_mirror_visibility(conn, post_id)?;
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
    let new_positions: HashMap<_, _> = source_keys(imported)
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();
    let remap: Vec<_> = source_keys(&previous)
        .into_iter()
        .map(|key| new_positions.get(&key).copied())
        .collect();

    let saved_merges = merges(conn, post_id)?;
    let mut saved_compressions = compressions(conn, post_id)?;
    let mut kept_merges = Vec::new();
    let mut kept_compressions = Vec::new();
    let mut reset = false;
    for mut merge in saved_merges {
        let compression = saved_compressions.remove(&merge.members[0]);
        let members: Option<Vec<_>> = merge.members.iter().map(|&index| remap[index]).collect();
        let Some(mut members) = members else {
            reset = true;
            continue;
        };
        members.sort_unstable();
        if let Some(item) = compression {
            kept_compressions.push((members[0], item));
        }
        merge.members = members;
        kept_merges.push(merge);
    }
    for (index, item) in saved_compressions {
        if let Some(new_index) = remap[index] {
            kept_compressions.push((new_index, item));
        } else {
            reset = true;
        }
    }

    conn.execute("DELETE FROM media_merges WHERE post_id = ?1", [post_id])?;
    conn.execute(
        "DELETE FROM media_compressions WHERE post_id = ?1",
        [post_id],
    )?;
    if kept_merges.is_empty() && kept_compressions.is_empty() {
        conn.execute(
            "DELETE FROM media_merge_sources WHERE post_id = ?1",
            [post_id],
        )?;
        posts::replace_media(conn, post_id, imported)?;
    } else {
        save_sources(conn, post_id, imported)?;
        for merge in kept_merges {
            save_merge(
                conn,
                post_id,
                Some(merge.id),
                &merge.members,
                &merge.item.blob_hash,
                merge.item.width,
                merge.item.height,
            )?;
        }
        for (first, item) in kept_compressions {
            save_compression(
                conn,
                post_id,
                first,
                &item.blob_hash,
                item.width,
                item.height,
            )?;
        }
        rebuild(conn, post_id, imported)?;
    }
    update_mirror_visibility(conn, post_id)?;
    Ok(reset)
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

pub fn edit_version(conn: &Connection, post_id: i64, media: &[MediaItem]) -> Result<String> {
    let version = gallery_version(media);
    let hidden = hidden_positions(conn, post_id, media)?;
    if hidden.is_empty() {
        return Ok(version);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(version.as_bytes());
    for index in hidden {
        hash.update(&(index as u64).to_le_bytes());
    }
    Ok(hash.finalize().to_hex().to_string())
}

fn verify_gallery(
    conn: &Connection,
    post_id: i64,
    media: &[MediaItem],
    version: &str,
) -> Result<()> {
    if edit_version(conn, post_id, media)? != version {
        Err(Error::invalid(
            "The gallery changed. Reload the post and select the panels again.",
        ))
    } else {
        Ok(())
    }
}

pub fn undo_ids(conn: &Connection, post_id: i64) -> Result<Vec<(usize, i64)>> {
    let merges = merges(conn, post_id)?;
    if merges.is_empty() {
        return Ok(Vec::new());
    }
    let slots = layout(sources(conn, post_id)?.len(), &merges)?;
    Ok(slots
        .iter()
        .enumerate()
        .filter_map(|(position, members)| {
            merges
                .iter()
                .find(|merge| merge.members == *members)
                .map(|merge| (position, merge.id))
        })
        .collect())
}

fn source_keys(sources: &[NewMedia]) -> Vec<(&str, i64)> {
    let mut occurrences = HashMap::new();
    sources
        .iter()
        .map(|item| {
            let key = item.blob_hash.as_str();
            let count = occurrences.entry(key).or_insert(0);
            let occurrence = *count;
            *count += 1;
            (key, occurrence)
        })
        .collect()
}

fn gallery_sources(conn: &Connection, post_id: i64, media: &[MediaItem]) -> Result<Vec<NewMedia>> {
    let mut original = sources(conn, post_id)?;
    if original.is_empty() {
        original = media.iter().map(NewMedia::from).collect();
    }
    Ok(original)
}

pub fn hidden_positions(
    conn: &Connection,
    post_id: i64,
    media: &[MediaItem],
) -> Result<Vec<usize>> {
    let mut statement =
        conn.prepare("SELECT blob_hash, occurrence FROM media_hidden_sources WHERE post_id = ?1")?;
    let hidden: HashSet<(String, i64)> = statement
        .query_map([post_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if hidden.is_empty() {
        return Ok(Vec::new());
    }
    let original = gallery_sources(conn, post_id, media)?;
    let keys = source_keys(&original);
    let hidden: HashSet<(&str, i64)> = hidden
        .iter()
        .map(|(hash, count)| (hash.as_str(), *count))
        .collect();
    let slots = layout(original.len(), &merges(conn, post_id)?)?;
    Ok(slots
        .iter()
        .enumerate()
        .filter(|(_, members)| {
            members
                .iter()
                .any(|&ordinal| hidden.contains(&keys[ordinal]))
        })
        .map(|(index, _)| index)
        .collect())
}

pub fn outgoing(conn: &Connection, mut post: crate::model::Post) -> Result<crate::model::Post> {
    let hidden = hidden_positions(conn, post.id, &post.media)?;
    let mut position = 0;
    post.media.retain(|_| {
        let visible = hidden.binary_search(&position).is_err();
        position += 1;
        visible
    });
    Ok(post)
}

pub fn outgoing_post(conn: &Connection, post_id: i64) -> Result<crate::model::Post> {
    outgoing(conn, posts::get(conn, post_id)?)
}

pub fn publish_snapshot(
    conn: &mut Connection,
    post_id: i64,
) -> Result<(crate::model::Post, String)> {
    let tx = conn.transaction()?;
    let post = posts::get(&tx, post_id)?;
    let revision = edit_version(&tx, post_id, &post.media)?;
    let outgoing = outgoing(&tx, post)?;
    tx.commit()?;
    Ok((outgoing, revision))
}

pub fn current_revision(conn: &Connection, post_id: i64) -> Result<String> {
    edit_version(conn, post_id, &posts::media(conn, post_id)?)
}

pub fn set_hidden(
    conn: &mut Connection,
    post_id: i64,
    media_id: i64,
    version: &str,
    hidden: bool,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&tx, post_id, &media, version)?;
    if latest_job(&tx, post_id)?.is_some_and(|job| job.is_active()) {
        return Err(Error::invalid("Wait for the gallery edit to finish"));
    }
    if super::queue::for_post(&tx, post_id)?
        .iter()
        .any(|(_, _, state)| *state == crate::model::QueueState::Sending)
    {
        return Err(Error::invalid(
            "Wait for the current send to finish before changing visibility",
        ));
    }
    let index = media_position(&media, media_id)?;
    let original = gallery_sources(&tx, post_id, &media)?;
    let slots = layout(original.len(), &merges(&tx, post_id)?)?;
    let keys = source_keys(&original);
    let members = slots
        .get(index)
        .ok_or_else(|| Error::other("invalid gallery edit layout"))?;
    for &member in members {
        let (hash, occurrence) = keys[member];
        let sql = if hidden {
            "INSERT OR IGNORE INTO media_hidden_sources (post_id, blob_hash, occurrence) VALUES (?1, ?2, ?3)"
        } else {
            "DELETE FROM media_hidden_sources WHERE post_id = ?1 AND blob_hash = ?2 AND occurrence = ?3"
        };
        tx.execute(sql, params![post_id, hash, occurrence])?;
    }
    update_mirror_visibility(&tx, post_id)?;
    tx.commit()?;
    Ok(())
}

fn update_mirror_visibility(conn: &Connection, post_id: i64) -> Result<()> {
    if super::mirrors::has_blocked(conn, post_id)?
        && !outgoing_post(conn, post_id)?.media.is_empty()
    {
        super::mirrors::unblock(conn, post_id)?;
    }
    Ok(())
}

pub fn undo(conn: &mut Connection, post_id: i64, merge_id: i64, version: &str) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_gallery(&tx, post_id, &posts::media(&tx, post_id)?, version)?;
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
        .filter_map(|(position, range)| compressed.contains_key(&range[0]).then_some(position))
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
        .map(|members| members[0])
        .ok_or_else(|| Error::other("invalid gallery edit layout"))
}

pub fn enqueue_compression(
    conn: &mut Connection,
    post_id: i64,
    media_id: Option<i64>,
    version: &str,
    settings: Compression,
) -> Result<Option<i64>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&tx, post_id, &media, version)?;
    let (selection, total) = if let Some(id) = media_id {
        let index = media_position(&media, id)?;
        if !crate::media::merge::supports(&media[index]) {
            return Err(Error::invalid("Select a still JPEG, PNG or WebP image"));
        }
        (id.to_string(), 1)
    } else {
        if !media.iter().any(crate::media::merge::supports) {
            return Err(Error::invalid(
                "This post has no supported images to compress",
            ));
        }
        ("all".into(), media.len() as i64)
    };
    if latest_job(&tx, post_id)?.is_some_and(|job| job.is_active()) {
        return Ok(None);
    }
    let payload = format!(
        "{selection}:{version}:{}:{}",
        settings.quality(),
        settings.fits_telegram(),
    );
    let job_id = jobs::create(&tx, "compress_media", &payload, total)?;
    jobs::attach_post(&tx, job_id, post_id)?;
    tx.commit()?;
    Ok(Some(job_id))
}

pub fn compression_input(
    conn: &Connection,
    post_id: i64,
    media_id: i64,
    version: &str,
) -> Result<NewMedia> {
    let media = posts::media(conn, post_id)?;
    verify_gallery(conn, post_id, &media, version)?;
    let index = media_position(&media, media_id)?;
    compression_sources(conn, post_id, &media)?
        .into_iter()
        .nth(index)
        .ok_or_else(|| Error::invalid("Invalid gallery layout"))
}

fn compression_sources(
    conn: &Connection,
    post_id: i64,
    media: &[MediaItem],
) -> Result<Vec<NewMedia>> {
    let original = gallery_sources(conn, post_id, media)?;
    let merges = merges(conn, post_id)?;
    let slots = layout(original.len(), &merges)?;
    let mut merges = merges.into_iter().peekable();
    Ok(slots
        .into_iter()
        .map(|slot| {
            let first = slot[0];
            merges
                .next_if(|merge| merge.members[0] == first)
                .map_or_else(|| original[first].clone(), |merge| merge.item)
        })
        .collect())
}

pub(crate) fn batch_compression_input(
    conn: &mut Connection,
    post_id: i64,
    version: &str,
) -> Result<Vec<Option<NewMedia>>> {
    let tx = conn.transaction()?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&tx, post_id, &media, version)?;
    let sources = compression_sources(&tx, post_id, &media)?;
    if sources.len() != media.len() {
        return Err(Error::invalid("Invalid gallery layout"));
    }
    Ok(media
        .iter()
        .zip(sources)
        .map(|(item, source)| crate::media::merge::supports(item).then_some(source))
        .collect())
}

pub(crate) fn finish_batch_compression(
    conn: &mut Connection,
    store: &BlobStore,
    job_id: i64,
    post_id: i64,
    version: &str,
    outputs: &[Option<(std::path::PathBuf, std::path::PathBuf)>],
) -> Result<String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_running(&tx, job_id)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&tx, post_id, &media, version)?;
    if outputs.len() != media.len() {
        return Err(Error::invalid("The gallery changed. Reload the post."));
    }
    if super::queue::for_post(&tx, post_id)?
        .iter()
        .any(|(_, _, state)| *state == crate::model::QueueState::Sending)
    {
        return Err(Error::invalid(
            "Wait for the current send to finish before compressing the gallery",
        ));
    }
    let mut saved = 0;
    let mut retained = Vec::new();
    let mut slots = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        let Some((path, thumb_path)) = output else {
            continue;
        };
        let bytes = std::fs::read(path)?;
        if blake3::hash(&bytes).to_hex().as_str() == media[index].blob_hash {
            continue;
        }
        if retained.is_empty() {
            retained = retain_sources(&tx, post_id, &media)?;
            slots = layout(retained.len(), &merges(&tx, post_id)?)?;
        }
        let output = register(
            &tx,
            store,
            Rendition {
                bytes: &bytes,
                ext: "jpg",
                thumbnail: &std::fs::read(thumb_path)?,
            },
        )?;
        save_compression(
            &tx,
            post_id,
            slots[index][0],
            &output.hash,
            output.width,
            output.height,
        )?;
        saved += 1;
    }
    if saved > 0 {
        rebuild(&tx, post_id, &retained)?;
    }
    let total = outputs.len();
    let message = format!("{saved} compressed · {} skipped", total - saved);
    jobs::progress(&tx, job_id, total as i64, total as i64)?;
    jobs::set_state(&tx, job_id, "done", Some(&message))?;
    tx.commit()?;
    Ok(message)
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
    verify_gallery(&tx, intent.post_id, &media, intent.version)?;
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
    save_compression(
        &tx,
        intent.post_id,
        first,
        &output.hash,
        output.width,
        output.height,
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
    verify_gallery(&tx, post_id, &media, version)?;
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

/// The active media already contains the final gallery. Rebase hidden keys
/// onto it before dropping the retained edit history.
pub(super) fn compact(conn: &Connection, post_id: i64) -> Result<()> {
    let media = posts::media(conn, post_id)?;
    let hidden = hidden_positions(conn, post_id, &media)?;
    let items: Vec<_> = media.iter().map(NewMedia::from).collect();
    let keys = source_keys(&items);
    conn.execute(
        "DELETE FROM media_hidden_sources WHERE post_id = ?1",
        [post_id],
    )?;
    for position in hidden {
        let (hash, occurrence) = keys[position];
        conn.execute(
            "INSERT INTO media_hidden_sources (post_id, blob_hash, occurrence) VALUES (?1, ?2, ?3)",
            params![post_id, hash, occurrence],
        )?;
    }
    conn.execute(
        "DELETE FROM media_compressions WHERE post_id = ?1",
        [post_id],
    )?;
    conn.execute("DELETE FROM media_merges WHERE post_id = ?1", [post_id])?;
    conn.execute(
        "DELETE FROM media_merge_sources WHERE post_id = ?1",
        [post_id],
    )?;
    Ok(())
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

fn selected_positions(media: &[MediaItem], selected: &[i64]) -> Result<Vec<usize>> {
    let positions: Vec<_> = media
        .iter()
        .enumerate()
        .filter_map(|(position, item)| selected.contains(&item.id).then_some(position))
        .collect();
    if positions.len() != selected.len() || positions.len() < 2 {
        return Err(Error::invalid(
            "Select at least two images from the current gallery",
        ));
    }
    if !positions
        .iter()
        .all(|&index| crate::media::merge::supports(&media[index]))
    {
        return Err(Error::invalid("Select still JPEG, PNG or WebP images"));
    }
    Ok(positions)
}

fn merge_positions(
    conn: &Connection,
    post_id: i64,
    media: &[MediaItem],
    ids: &[i64],
) -> Result<Vec<usize>> {
    let positions = selected_positions(media, ids)?;
    let hidden = hidden_positions(conn, post_id, media)?;
    if positions
        .iter()
        .any(|index| hidden.binary_search(index).is_ok())
    {
        return Err(Error::invalid("Restore hidden images before merging them"));
    }
    Ok(positions)
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
) -> Result<Option<i64>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let media = posts::media(&tx, post_id)?;
    verify_gallery(&tx, post_id, &media, version)?;
    let indices = merge_positions(&tx, post_id, &media, selected)?;
    if latest_job(&tx, post_id)?.is_some_and(|job| job.is_active()) {
        return Ok(None);
    }
    let ids = indices
        .iter()
        .map(|&index| media[index].id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut payload = format!("{ids}:{version}");
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
    Ok(Some(job_id))
}

pub struct Intent<'a> {
    pub post_id: i64,
    pub first_id: i64,
    pub last_id: i64,
    pub version: &'a str,
    pub selected: Option<&'a [i64]>,
}

fn intent_positions(
    conn: &Connection,
    media: &[MediaItem],
    intent: &Intent<'_>,
) -> Result<Vec<usize>> {
    if let Some(ids) = intent.selected {
        return merge_positions(conn, intent.post_id, media, ids);
    }
    // Previously queued jobs stored both endpoints of a contiguous selection.
    let first = media_position(media, intent.first_id)?;
    let last = media_position(media, intent.last_id)?;
    if first >= last {
        return Err(Error::invalid(
            "Select at least two images from the current gallery",
        ));
    }
    let ids: Vec<_> = media[first..=last].iter().map(|item| item.id).collect();
    merge_positions(conn, intent.post_id, media, &ids)
}

pub fn panels(conn: &Connection, intent: &Intent<'_>) -> Result<Vec<MediaItem>> {
    let media = posts::media(conn, intent.post_id)?;
    verify_gallery(conn, intent.post_id, &media, intent.version)?;
    Ok(intent_positions(conn, &media, intent)?
        .into_iter()
        .map(|index| media[index].clone())
        .collect())
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
    verify_gallery(&tx, intent.post_id, &media, intent.version)?;
    let positions = intent_positions(&tx, &media, intent)?;
    let sources = retain_sources(&tx, post_id, &media)?;
    let slots = layout(sources.len(), &merges(&tx, post_id)?)?;
    if slots.len() != media.len() {
        return Err(Error::other("invalid gallery merge layout"));
    }
    let mut members: Vec<_> = positions
        .iter()
        .flat_map(|&index| slots[index].iter().copied())
        .collect();
    members.sort_unstable();
    let output = register(&tx, store, rendition)?;
    for &index in &positions {
        let first = slots[index][0] as i64;
        delete_compressions(&tx, post_id, first, first)?;
        tx.execute(
            "DELETE FROM media_merges WHERE post_id = ?1 AND first_ordinal = ?2",
            params![post_id, first],
        )?;
    }
    save_merge(
        &tx,
        post_id,
        None,
        &members,
        &output.hash,
        output.width,
        output.height,
    )?;
    rebuild(&tx, post_id, &sources)?;
    let message = format!("Merged {} images", positions.len());
    jobs::set_state(&tx, job_id, "done", Some(&message))?;
    tx.commit()?;
    Ok(message)
}

fn save_merge(
    conn: &Connection,
    post_id: i64,
    id: Option<i64>,
    members: &[usize],
    hash: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<()> {
    let merge_id: i64 = conn.query_row(
        "INSERT INTO media_merges (id, post_id, first_ordinal, last_ordinal, blob_hash, width, height)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
        params![id, post_id, members[0] as i64, members[members.len() - 1] as i64, hash, width, height],
        |row| row.get(0),
    )?;
    let mut insert =
        conn.prepare("INSERT INTO media_merge_members (merge_id, source_ordinal) VALUES (?1, ?2)")?;
    for &member in members {
        insert.execute(params![merge_id, member as i64])?;
    }
    Ok(())
}

fn save_compression(
    conn: &Connection,
    post_id: i64,
    first: usize,
    hash: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO media_compressions (post_id, first_ordinal, blob_hash, width, height)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (post_id, first_ordinal) DO UPDATE SET
            blob_hash = excluded.blob_hash, width = excluded.width, height = excluded.height",
        params![post_id, first as i64, hash, width, height],
    )?;
    Ok(())
}

const ORPHAN_BLOBS: &str = "FROM blobs b
         WHERE NOT EXISTS (SELECT 1 FROM media m WHERE m.blob_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media m WHERE m.poster_hash = b.hash)
           AND NOT EXISTS (SELECT 1 FROM media_merge_sources s WHERE s.blob_hash = b.hash
               AND s.post_id NOT IN (SELECT value FROM json_each(?1)))
           AND NOT EXISTS (SELECT 1 FROM media_merge_sources s WHERE s.poster_hash = b.hash
               AND s.post_id NOT IN (SELECT value FROM json_each(?1)))
           AND NOT EXISTS (SELECT 1 FROM media_merges m WHERE m.blob_hash = b.hash
               AND m.post_id NOT IN (SELECT value FROM json_each(?1)))
           AND NOT EXISTS (SELECT 1 FROM media_compressions c WHERE c.blob_hash = b.hash
               AND c.post_id NOT IN (SELECT value FROM json_each(?1)))";

pub(crate) fn projected_orphan_blobs(
    conn: &Connection,
    compacted: &[i64],
) -> Result<Vec<(String, String)>> {
    let mut statement = conn.prepare(&format!(
        "SELECT b.hash, b.ext {ORPHAN_BLOBS} ORDER BY b.hash"
    ))?;
    let ids = serde_json::to_string(compacted)?;
    Ok(statement
        .query_map([ids], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn orphan_blob_totals(conn: &Connection) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*), COALESCE(SUM(b.bytes), 0) {ORPHAN_BLOBS}"),
        ["[]"],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

pub(crate) fn known_blob_hashes(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT hash FROM blobs")?;
    Ok(stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn forget_blob(conn: &Connection, hash: &str) -> Result<()> {
    conn.execute("DELETE FROM blobs WHERE hash = ?1", [hash])?;
    Ok(())
}
