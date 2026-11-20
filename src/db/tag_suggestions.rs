//! Review batches and the archive's character-to-franchise catalog.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use super::{jobs, posts, tags};
use crate::error::{Error, Result};
use crate::model::{AssociationOrigin, Post, SuggestionKind};

pub struct Input {
    pub post: Post,
    pub metadata: Option<String>,
    pub fingerprint: String,
}

pub fn input(conn: &Connection, post_id: i64) -> Result<Input> {
    let post = posts::get(conn, post_id)?;
    let (metadata, _) = posts::diagnostics(conn, post_id)?;
    let fingerprint = fingerprint(&post, metadata.as_deref());
    Ok(Input {
        post,
        metadata,
        fingerprint,
    })
}

fn fingerprint(post: &Post, metadata: Option<&str>) -> String {
    fn field(hash: &mut blake3::Hasher, value: &str) {
        hash.update(&(value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    let mut hash = blake3::Hasher::new();
    field(&mut hash, metadata.unwrap_or_default());
    for media in &post.media {
        hash.update(&media.ordinal.to_le_bytes());
        hash.update(&[u8::from(media.is_video)]);
        field(&mut hash, &media.blob_hash);
        field(&mut hash, &media.ext);
        field(&mut hash, media.meta_json.as_deref().unwrap_or_default());
    }
    hash.finalize().to_hex().to_string()
}

pub fn enqueue(conn: &mut Connection, post_id: i64, kind: &str) -> Result<(i64, bool)> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let post = posts::get(&tx, post_id)?;
    if post.media.iter().all(|item| item.is_video) {
        return Err(Error::invalid("This post has no images to analyze"));
    }
    let payload = post_id.to_string();
    if let Some(id) = jobs::find_active(&tx, kind, &payload)? {
        return Ok((id, false));
    }
    let job_id = jobs::create(&tx, kind, &payload, 0)?;
    jobs::attach_post(&tx, job_id, post_id)?;
    tx.execute(
        "DELETE FROM post_tag_suggestion_batches WHERE post_id = ?1",
        [post_id],
    )?;
    tx.execute(
        "INSERT INTO post_tag_suggestion_batches (post_id, job_id) VALUES (?1, ?2)",
        params![post_id, job_id],
    )?;
    tx.commit()?;
    Ok((job_id, true))
}

fn batch(conn: &Connection, post_id: i64) -> Result<Option<(i64, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT job_id, fingerprint FROM post_tag_suggestion_batches WHERE post_id = ?1",
            [post_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

pub fn latest_job(conn: &Connection, post_id: i64) -> Result<Option<jobs::Job>> {
    let Some((job_id, _)) = batch(conn, post_id)? else {
        return Ok(None);
    };
    jobs::get(conn, job_id)
}

#[derive(Debug)]
pub struct Candidate {
    pub kind: SuggestionKind,
    pub tag: String,
    pub character_tag_id: Option<i64>,
    pub evidence: &'static str,
    pub low_confidence: bool,
    pub wiki_title: Option<String>,
    pub images: Vec<i64>,
}

#[derive(Debug, Serialize)]
pub struct Suggestion {
    pub id: i64,
    pub kind: SuggestionKind,
    pub tag: String,
    pub character_tag_id: Option<i64>,
    pub evidence: String,
    pub wiki_title: Option<String>,
    pub images: Vec<i64>,
}

pub struct Review {
    pub job: jobs::Job,
    pub stale: bool,
    pub suggestions: Vec<Suggestion>,
}

pub fn review(conn: &Connection, post: &Post) -> Result<Option<Review>> {
    let Some((job_id, saved_fingerprint)) = batch(conn, post.id)? else {
        return Ok(None);
    };
    let Some(job) = jobs::get(conn, job_id)? else {
        return Ok(None);
    };
    let mut review = Review {
        job,
        stale: false,
        suggestions: Vec::new(),
    };
    if let Some(saved) = saved_fingerprint {
        let (metadata, _) = posts::diagnostics(conn, post.id)?;
        review.stale = saved != fingerprint(post, metadata.as_deref());
        if !review.stale {
            review.suggestions = suggestions(conn, post.id, job_id)?;
        }
    }
    Ok(Some(review))
}

fn suggestions(conn: &Connection, post_id: i64, job_id: i64) -> Result<Vec<Suggestion>> {
    let filter = TagFilter::load(conn, post_id)?;
    let mut statement = conn.prepare(
        "SELECT id, kind, tag, character_tag_id, evidence, wiki_title
         FROM post_tag_suggestions WHERE job_id = ?1 ORDER BY id",
    )?;
    let rows = statement.query_map([job_id], |row| {
        Ok(Suggestion {
            id: row.get(0)?,
            kind: SuggestionKind::parse(&row.get::<_, String>(1)?),
            tag: row.get(2)?,
            character_tag_id: row.get(3)?,
            evidence: row.get(4)?,
            wiki_title: row.get(5)?,
            images: Vec::new(),
        })
    })?;
    let mut items = Vec::new();
    let mut shown = HashSet::new();
    let mut images = conn.prepare(
        "SELECT position FROM post_tag_suggestion_images WHERE suggestion_id = ?1 ORDER BY position",
    )?;
    for row in rows {
        let mut item = row?;
        if filter
            .available(&item.tag)
            .is_some_and(|tag| shown.insert(tag))
        {
            item.images = images
                .query_map([item.id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            items.push(item);
        }
    }
    Ok(items)
}

struct TagFilter {
    aliases: HashMap<String, String>,
    excluded: HashSet<String>,
}

impl TagFilter {
    fn load(conn: &Connection, post_id: i64) -> Result<Self> {
        let aliases = tags::load_aliases(conn)?;
        let mut excluded: HashSet<_> = tags::for_post(conn, post_id)?
            .into_iter()
            .chain(
                tags::removals(conn, post_id)?
                    .into_iter()
                    .map(|tag| tag.name),
            )
            .collect();
        let resolved = tags::apply_aliases(excluded.iter().cloned().collect(), &aliases);
        excluded.extend(resolved);
        Ok(Self { aliases, excluded })
    }

    fn available(&self, raw: &str) -> Option<String> {
        let normalized = tags::normalize(raw)?;
        let tag = tags::apply_aliases(vec![normalized], &self.aliases).pop()?;
        (!self.excluded.contains(&tag)).then_some(tag)
    }
}

pub fn finish(
    conn: &mut Connection,
    post_id: i64,
    job_id: i64,
    fingerprint: &str,
    candidates: &[Candidate],
    scanned: usize,
    skipped: usize,
) -> Result<String> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if batch(&tx, post_id)?.map(|(id, _)| id) != Some(job_id)
        || jobs::get(&tx, job_id)?.is_none_or(|job| job.state != "running")
        || input(&tx, post_id)?.fingerprint != fingerprint
    {
        return Err(Error::invalid(
            "Post changed during analysis. Suggest tags again.",
        ));
    }
    let filter = TagFilter::load(&tx, post_id)?;
    let mut added = HashSet::new();
    let mut characters = 0;
    let mut franchises = 0;
    for candidate in candidates {
        let (count, limit) = match candidate.kind {
            SuggestionKind::Character => {
                let limit = if candidate.low_confidence { 3 } else { 5 };
                (&mut characters, limit)
            }
            SuggestionKind::Franchise => (&mut franchises, 3),
        };
        if *count >= limit
            || !filter
                .available(&candidate.tag)
                .is_some_and(|tag| added.insert(tag))
        {
            continue;
        }
        tx.execute(
            "INSERT INTO post_tag_suggestions (job_id, kind, tag, character_tag_id, evidence, wiki_title)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![job_id, candidate.kind.as_str(), candidate.tag, candidate.character_tag_id,
                    candidate.evidence, candidate.wiki_title],
        )?;
        let suggestion_id = tx.last_insert_rowid();
        for position in &candidate.images {
            tx.execute(
                "INSERT INTO post_tag_suggestion_images (suggestion_id, position) VALUES (?1, ?2)",
                params![suggestion_id, position],
            )?;
        }
        *count += 1;
    }
    tx.execute(
        "UPDATE post_tag_suggestion_batches SET fingerprint = ?1 WHERE post_id = ?2",
        params![fingerprint, post_id],
    )?;
    let count = characters + franchises;
    let summary = if count == 0 && scanned == 0 {
        "No readable still images found".to_string()
    } else if count == 0 {
        "No new tags found".to_string()
    } else {
        format!("{count} suggested tag(s)")
    };
    let summary = format!("{summary} · {scanned} analyzed · {skipped} skipped");
    jobs::progress(
        &tx,
        job_id,
        (scanned + skipped) as i64,
        (scanned + skipped) as i64,
    )?;
    jobs::set_state(&tx, job_id, "done", Some(&summary))?;
    tx.commit()?;
    Ok(summary)
}

pub fn resolve(
    conn: &mut Connection,
    post_id: i64,
    job_id: i64,
    suggestion_id: i64,
    edited: Option<&str>,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = input(&tx, post_id)?;
    if batch(&tx, post_id)? != Some((job_id, Some(current.fingerprint))) {
        return Err(Error::invalid(
            "These suggestions are stale. Reload the post or suggest tags again.",
        ));
    }
    let changed = tx.execute(
        "DELETE FROM post_tag_suggestions WHERE id = ?1 AND job_id = ?2",
        params![suggestion_id, job_id],
    )?;
    if changed == 0 {
        return Err(Error::invalid(
            "This suggestion has already been reviewed. Reload the post.",
        ));
    }
    if let Some(raw) = edited {
        if TagFilter::load(&tx, post_id)?.available(raw).is_none() {
            return Err(Error::invalid(
                "Enter a valid tag that is neither active nor saved as removed",
            ));
        }
        tags::add_manual(&tx, post_id, &[raw.to_owned()])?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug)]
pub struct Association {
    pub character_tag_id: i64,
    pub franchise: Option<String>,
    pub origin: AssociationOrigin,
    pub wiki_title: Option<String>,
}

fn association(row: &rusqlite::Row<'_>) -> rusqlite::Result<Association> {
    Ok(Association {
        character_tag_id: row.get(0)?,
        franchise: row.get(1)?,
        origin: AssociationOrigin::parse(&row.get::<_, String>(2)?),
        wiki_title: row.get(3)?,
    })
}

pub fn franchise(conn: &Connection, id: i64) -> Result<Option<Association>> {
    let cutoff = (Utc::now() - chrono::Duration::days(7)).timestamp();
    Ok(conn
        .query_row(
            "SELECT character_tag_id, franchise, origin, wiki_title FROM tag_franchise_associations
         WHERE character_tag_id = ?1 AND (franchise IS NOT NULL OR checked_at > ?2)
         ORDER BY CASE origin WHEN 'manual' THEN 0 WHEN 'starter' THEN 1 ELSE 2 END LIMIT 1",
            params![id, cutoff],
            association,
        )
        .optional()?)
}

pub fn cache_lookup(conn: &Connection, id: i64, matched: Option<(&str, &str)>) -> Result<()> {
    let (franchise, wiki_title) = matched.unzip();
    conn.execute(
        "INSERT INTO tag_franchise_associations (character_tag_id, origin, franchise, wiki_title, checked_at)
         VALUES (?1, 'lookup', ?2, ?3, ?4)
         ON CONFLICT (character_tag_id, origin) DO UPDATE SET franchise = excluded.franchise,
         wiki_title = excluded.wiki_title, checked_at = excluded.checked_at",
        params![id, franchise, wiki_title, Utc::now().timestamp()],
    )?;
    Ok(())
}

pub fn set_mapping(conn: &Connection, id: i64, raw: &str) -> Result<()> {
    if id <= 0 {
        return Err(Error::invalid("character ID must be positive"));
    }
    let tag = tags::normalize(raw).ok_or_else(|| Error::invalid("invalid franchise tag"))?;
    conn.execute(
        "INSERT INTO tag_franchise_associations (character_tag_id, origin, franchise)
         VALUES (?1, 'manual', ?2)
         ON CONFLICT (character_tag_id, origin) DO UPDATE SET franchise = excluded.franchise",
        params![id, tag],
    )?;
    Ok(())
}

pub fn remove_mapping(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM tag_franchise_associations WHERE character_tag_id = ?1 AND origin = 'manual'",
        [id],
    )? != 0)
}

pub fn mappings(conn: &Connection) -> Result<Vec<Association>> {
    let mut statement = conn.prepare(
        "SELECT character_tag_id, franchise, origin, wiki_title FROM tag_franchise_associations
         WHERE franchise IS NOT NULL
         ORDER BY character_tag_id, CASE origin WHEN 'manual' THEN 0 WHEN 'starter' THEN 1 ELSE 2 END",
    )?;
    let mut rows = statement
        .query_map([], association)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.dedup_by_key(|row| row.character_tag_id);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_take_priority_and_cached_misses_expire() {
        let db = super::super::Db::open_in_memory().unwrap();
        db.with(|conn| {
            cache_lookup(conn, 3861, Some(("cached_series", "wiki")))?;
            assert_eq!(
                franchise(conn, 3861)?.unwrap().franchise.as_deref(),
                Some("pokemon")
            );
            set_mapping(conn, 3861, "New Name")?;
            assert_eq!(
                franchise(conn, 3861)?.unwrap().franchise.as_deref(),
                Some("new_name")
            );
            assert!(remove_mapping(conn, 3861)?);
            assert_eq!(
                franchise(conn, 3861)?.unwrap().origin,
                AssociationOrigin::Starter
            );
            cache_lookup(conn, 42, None)?;
            assert!(franchise(conn, 42)?.unwrap().franchise.is_none());
            conn.execute(
                "UPDATE tag_franchise_associations SET checked_at = 0 WHERE character_tag_id = 42",
                [],
            )?;
            assert!(franchise(conn, 42)?.is_none());
            Ok(())
        })
        .unwrap();
    }
}
