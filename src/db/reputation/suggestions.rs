//! Match guest imports to a stable artwork identity.

use rusqlite::{Connection, OptionalExtension, params};

use crate::config::AccessConfig;
use crate::db::{posts, users};
use crate::error::Result;
use crate::model::Post;

use super::publications::{legacy_publication, publication, save_delivery};
use super::record_event;

const REDISCOVERY: i64 = 200;
const PUBLISHED_DUPLICATE: i64 = -100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Suggestion {
    Pending,
    AlreadyArchived,
    Rediscovered,
    Published(Option<String>),
    Unconfirmed,
}

impl Suggestion {
    pub fn describe(&self) -> String {
        match self {
            Self::Pending => "Suggestion awaiting admin review.".into(),
            Self::AlreadyArchived => "Already in the archive or previously suggested.".into(),
            Self::Rediscovered => "Rediscovered an unpublished post from the archive.".into(),
            Self::Published(Some(url)) => format!("Already published: {url}"),
            Self::Published(None) => "Already published; no message link was recorded.".into(),
            Self::Unconfirmed => "Already in the archive; delivery is unconfirmed.".into(),
        }
    }
}

#[derive(Debug)]
pub struct MatchedSuggestion {
    pub post_id: Option<i64>,
    pub status: Suggestion,
}

fn canonical(source: &str, id: &str) -> String {
    if matches!(source, "telegram" | "telegram_forward")
        && let Some((channel, message)) = id.split_once('/')
        && let Ok(message) = message.parse::<i32>()
    {
        return format!("telegram:{}/{message}", channel.to_ascii_lowercase());
    }
    format!("{source}:{id}")
}

fn media_key<'a>(hashes: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut hashes: Vec<_> = hashes.into_iter().collect();
    hashes.sort_unstable();
    (!hashes.is_empty()).then(|| hashes.join(","))
}

pub(super) fn lookup_key(conn: &Connection, kind: &str, identity: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT subject_id FROM reputation_keys WHERE kind = ?1 AND identity = ?2",
            params![kind, identity],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn set_key(conn: &Connection, subject: i64, kind: &str, identity: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO reputation_keys (kind, identity, subject_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(kind, identity) DO NOTHING",
        params![kind, identity, subject],
    )?;
    Ok(())
}

fn subject_for_post(conn: &Connection, post_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT subject_id FROM reputation_posts WHERE post_id = ?1",
            [post_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn post_for_subject(conn: &Connection, subject: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT post_id FROM reputation_posts WHERE subject_id = ?1 ORDER BY post_id LIMIT 1",
            [subject],
            |row| row.get(0),
        )
        .optional()?)
}

fn attach_post(conn: &Connection, post: i64, subject: i64) -> Result<()> {
    conn.execute("INSERT INTO reputation_posts (post_id, subject_id) VALUES (?1, ?2) ON CONFLICT(post_id) DO NOTHING", params![post, subject])?;
    Ok(())
}

fn new_subject(conn: &Connection, reviewable: bool) -> Result<i64> {
    Ok(conn.query_row(
        "INSERT INTO reputation_subjects (reviewable) VALUES (?1) RETURNING id",
        [reviewable],
        |row| row.get(0),
    )?)
}

fn add_keys(
    conn: &Connection,
    subject: i64,
    source: &str,
    source_id: &str,
    key: Option<&str>,
) -> Result<()> {
    set_key(conn, subject, "source", &canonical(source, source_id))?;
    if let Some(key) = key {
        set_key(conn, subject, "media", key)?;
    }
    Ok(())
}

pub(super) fn attach_known(conn: &Connection, post: &Post, reviewable: bool) -> Result<i64> {
    let key = media_key(post.media.iter().map(|item| item.blob_hash.as_str()));
    let subject = subject_for_post(conn, post.id)?.or(lookup_key(
        conn,
        "source",
        &canonical(&post.source, &post.source_id),
    )?);
    let subject = match subject {
        Some(subject) => subject,
        None => match key
            .as_deref()
            .map(|key| lookup_key(conn, "media", key))
            .transpose()?
            .flatten()
        {
            Some(subject) if !reviewable => subject,
            _ => new_subject(conn, reviewable)?,
        },
    };
    if posts::find_by_source_id(conn, &post.source, &post.source_id)? == Some(post.id) {
        attach_post(conn, post.id, subject)?;
    }
    add_keys(conn, subject, &post.source, &post.source_id, key.as_deref())?;
    Ok(subject)
}

pub fn register_new(conn: &Connection, post_id: i64, user: i64) -> Result<Suggestion> {
    let subject = attach_known(conn, &posts::get(conn, post_id)?, true)?;
    Ok(record_suggestion(conn, subject, Some(post_id), user, false)?.status)
}

pub fn reattach_known(
    conn: &Connection,
    post_id: i64,
    source: &str,
    source_id: &str,
    media: &[posts::NewMedia],
) -> Result<()> {
    let source_subject = lookup_key(conn, "source", &canonical(source, source_id))?;
    let key = media_key(media.iter().map(|item| item.blob_hash.as_str()));
    let media_subject = key
        .as_deref()
        .map(|key| lookup_key(conn, "media", key))
        .transpose()?
        .flatten();
    if let Some(subject) = source_subject.or(media_subject) {
        attach_post(conn, post_id, subject)?;
        add_keys(conn, subject, source, source_id, key.as_deref())?;
    }
    Ok(())
}

fn matching_posts(
    conn: &Connection,
    source: &str,
    source_id: &str,
    key: Option<&str>,
) -> Result<Vec<i64>> {
    let mut matches = Vec::new();
    if let Some(post_id) = posts::find_by_source_id(conn, source, source_id)? {
        matches.push(post_id);
    }
    if let Some(key) = key {
        let hash = key.split(',').next().unwrap_or_default();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT m.post_id FROM media m WHERE m.blob_hash = ?1 AND ?2 = (
                 SELECT group_concat(blob_hash, ',' ORDER BY blob_hash) FROM media WHERE post_id = m.post_id
             ) ORDER BY m.post_id",
        )?;
        for id in stmt.query_map(params![hash, key], |row| row.get(0))? {
            let id = id?;
            if !matches.contains(&id) {
                matches.push(id);
            }
        }
    }
    Ok(matches)
}

/// Run inside the same write transaction as the eventual import. Concurrent
/// first suggestions therefore share the same archived post and verdict.
pub fn match_suggestion(
    conn: &Connection,
    access: &AccessConfig,
    source: &str,
    source_id: &str,
    media: &[posts::NewMedia],
    user: i64,
) -> Result<Option<MatchedSuggestion>> {
    let key = media_key(media.iter().map(|item| item.blob_hash.as_str()));
    let matches = matching_posts(conn, source, source_id, key.as_deref())?;
    let mut subjects = Vec::new();
    if let Some(subject) = lookup_key(conn, "source", &canonical(source, source_id))? {
        subjects.push(subject);
    }
    if let Some(key) = &key
        && let Some(subject) = lookup_key(conn, "media", key)?
        && !subjects.contains(&subject)
    {
        subjects.push(subject);
    }
    for post_id in &matches {
        if let Some(subject) = subject_for_post(conn, *post_id)?
            && !subjects.contains(&subject)
        {
            subjects.push(subject);
        }
    }
    for &post_id in &matches {
        if let Some(subject) = subject_for_post(conn, post_id)?
            && publication(conn, subject)?.is_some()
        {
            continue;
        }
        if let Some(delivery) = legacy_publication(conn, post_id)? {
            let post = posts::get(conn, post_id)?;
            let subject = attach_known(conn, &post, false)?;
            save_delivery(conn, subject, &delivery.target, None, None, &delivery.ids)?;
            if !subjects.contains(&subject) {
                subjects.push(subject);
            }
        }
    }
    let mut published_subject = None;
    for &subject in &subjects {
        if let Some(link) = publication(conn, subject)? {
            if link.is_some() {
                published_subject = Some(subject);
                break;
            }
            published_subject.get_or_insert(subject);
        }
    }
    let subject = match published_subject.or_else(|| subjects.first().copied()) {
        Some(subject) => subject,
        None => match matches.first() {
            Some(&post_id) => attach_known(conn, &posts::get(conn, post_id)?, false)?,
            None => return Ok(None),
        },
    };
    // Keep a verified alias for future retries, even if the archived post is removed.
    add_keys(conn, subject, source, source_id, key.as_deref())?;
    for &post in &matches {
        attach_post(conn, post, subject)?;
    }
    let post_id = post_for_subject(conn, subject)?;
    let rediscoverable = match post_id {
        Some(id) => operator_added(conn, access, id)?,
        None => false,
    };
    Ok(Some(record_suggestion(
        conn,
        subject,
        post_id,
        user,
        rediscoverable,
    )?))
}

fn operator_added(conn: &Connection, access: &AccessConfig, id: i64) -> Result<bool> {
    let (operator, submitter): (Option<bool>, Option<i64>) = conn.query_row(
        "SELECT operator_import, submitted_by FROM posts WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(match (operator, submitter) {
        (Some(flag), _) => flag,
        (None, None) => true,
        (None, Some(user)) => users::is_admin(conn, access, user)?,
    })
}

pub(super) fn record_suggestion(
    conn: &Connection,
    subject: i64,
    post_id: Option<i64>,
    user: i64,
    rediscoverable: bool,
) -> Result<MatchedSuggestion> {
    let pending: bool = conn.query_row(
        "SELECT reviewable AND verdict_points IS NULL FROM reputation_subjects WHERE id = ?1",
        [subject],
        |row| row.get(0),
    )?;
    let published = publication(conn, subject)?;
    let unconfirmed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM reputation_posts p JOIN queue q ON q.post_id = p.post_id
         WHERE p.subject_id = ?1 AND q.state IN ('sending', 'unknown'))",
        [subject],
        |row| row.get(0),
    )?;
    let pending = pending && published.is_none() && !unconfirmed;
    let is_first = conn.execute(
        "INSERT INTO reputation_suggestions (subject_id, user_id, review_member) VALUES (?1, ?2, ?3)
         ON CONFLICT(subject_id, user_id) DO NOTHING",
        params![subject, user, pending],
    )? != 0;
    let status = if let Some(url) = published {
        if is_first && let Some(url) = &url {
            record_event(
                conn,
                user,
                PUBLISHED_DUPLICATE,
                "Your suggestion was already published.",
                Some(url),
            )?;
        }
        Suggestion::Published(url)
    } else if unconfirmed {
        Suggestion::Unconfirmed
    } else if pending {
        Suggestion::Pending
    } else if is_first
        && rediscoverable
        && conn.execute(
            "UPDATE reputation_subjects SET rediscovered = 1 WHERE id = ?1 AND NOT rediscovered",
            [subject],
        )? != 0
    {
        record_event(
            conn,
            user,
            REDISCOVERY,
            "You rediscovered an unpublished post.",
            None,
        )?;
        Suggestion::Rediscovered
    } else {
        Suggestion::AlreadyArchived
    };
    Ok(MatchedSuggestion { post_id, status })
}
