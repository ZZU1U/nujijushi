//! Post/author/media persistence.

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::error::{Error, Result};
use crate::model::{Author, MediaItem, Post, PostCard, PostState, Rating};
use crate::source::NormalizedPost;

use super::{fmt_ts, get_bool, get_ts, get_ts_required, tags};

#[derive(Debug, Clone)]
pub struct NewMedia {
    pub blob_hash: String,
    pub ext: String,
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: i64,
    pub is_video: bool,
    pub poster_hash: Option<String>,
    /// Source-side "this item is sensitive" flag.
    pub sensitive: bool,
    /// This item's own extractor metadata (JSON), when the source wrote one.
    pub meta_json: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SentFilter {
    /// Every post, sent or not (the CLI default).
    #[default]
    Any,
    /// Hide posts that already went out on a channel — the web grid's default,
    /// because a curated archive is mostly "what is still to publish".
    NotSent,
    /// Only posts that already went out.
    Sent,
}

#[derive(Debug, Clone, Default)]
pub struct PostFilter {
    pub state: Option<PostState>,
    pub source: Option<String>,
    pub tag: Option<String>,
    pub nsfw: Option<bool>,
    pub author_id: Option<i64>,
    pub search: Option<String>,
    pub sent: SentFilter,
    /// Hide `rejected` posts — the web grid's default, because a rejected post
    /// is one the curator already said no to. A post with an explicit state
    /// filter ignores this, so `?state=rejected` still lists them.
    pub without_rejected: bool,
    pub limit: i64,
    pub offset: i64,
}

const POST_COLUMNS: &str = "
    p.id, p.source, p.source_id, p.url, p.title, p.description, p.rating, p.posted_at,
    p.imported_at, p.nsfw_override, p.state, p.notes,
    a.id AS author_id, a.source AS author_source, a.source_id AS author_source_id,
    a.name AS author_name, a.handle AS author_handle, a.url AS author_url,
    a.blocked AS author_blocked";

fn post_from_row(row: &Row<'_>) -> rusqlite::Result<Post> {
    let author_id: Option<i64> = row.get("author_id")?;
    let author = match author_id {
        Some(id) => Some(Author {
            id,
            source: row.get("author_source")?,
            source_id: row.get("author_source_id")?,
            name: row.get("author_name")?,
            handle: row.get("author_handle")?,
            url: row.get("author_url")?,
            blocked: get_bool(row, "author_blocked")?,
        }),
        None => None,
    };

    Ok(Post {
        id: row.get("id")?,
        source: row.get("source")?,
        source_id: row.get("source_id")?,
        url: row.get("url")?,
        author,
        title: row.get("title")?,
        description: row.get("description")?,
        rating: Rating::parse(&row.get::<_, String>("rating")?),
        posted_at: get_ts(row, "posted_at")?,
        imported_at: get_ts_required(row, "imported_at")?,
        nsfw_override: row.get::<_, Option<i64>>("nsfw_override")?.map(|v| v != 0),
        state: PostState::parse(&row.get::<_, String>("state")?),
        notes: row.get("notes")?,
        tags: Vec::new(),
        media: Vec::new(),
    })
}

fn media_from_row(row: &Row<'_>) -> rusqlite::Result<MediaItem> {
    Ok(MediaItem {
        id: row.get("id")?,
        post_id: row.get("post_id")?,
        ordinal: row.get("ordinal")?,
        blob_hash: row.get("blob_hash")?,
        ext: row.get("ext")?,
        mime: row.get("mime")?,
        width: row.get::<_, Option<i64>>("width")?.map(|v| v as u32),
        height: row.get::<_, Option<i64>>("height")?.map(|v| v as u32),
        bytes: row.get("bytes")?,
        is_video: get_bool(row, "is_video")?,
        poster_hash: row.get("poster_hash")?,
        sensitive: get_bool(row, "sensitive")?,
        meta_json: row.get("meta_json")?,
    })
}

/// Insert or refresh a post. Returns (post_id, created).
pub fn upsert(
    conn: &mut Connection,
    np: &NormalizedPost,
    media: &[NewMedia],
) -> Result<(i64, bool)> {
    super::transaction(conn, |conn| upsert_post(conn, np, media))
}

#[derive(Debug)]
pub struct StoredImport {
    pub post_id: Option<i64>,
    pub created: bool,
    pub suggestion: Option<super::reputation::Suggestion>,
}

/// Apply archive changes and import-time curation in the same commit. Watched
/// imports and matched guest suggestions leave an existing post untouched.
pub fn store_import(
    conn: &mut Connection,
    post: &NormalizedPost,
    media: &[NewMedia],
    submitted_by: Option<i64>,
    insert_only: bool,
    config: &crate::config::Config,
) -> Result<StoredImport> {
    use super::reputation;

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if insert_only && let Some(id) = find_by_source_id(&tx, &post.source, &post.source_id)? {
        return Ok(StoredImport {
            post_id: Some(id),
            created: false,
            suggestion: None,
        });
    }
    let guest = match submitted_by {
        Some(user) if reputation::is_guest(&tx, &config.access, user)? => Some(user),
        _ => None,
    };
    let rated_guest = guest.filter(|_| config.guest_reputation);
    if let Some(user) = rated_guest
        && let Some(matched) = reputation::match_suggestion(
            &tx,
            &config.access,
            &post.source,
            &post.source_id,
            media,
            user,
        )?
    {
        tx.commit()?;
        return Ok(StoredImport {
            post_id: matched.post_id,
            created: false,
            suggestion: Some(matched.status),
        });
    }
    let (id, created) = upsert_post(&tx, post, media)?;
    if created {
        tx.execute(
            "UPDATE posts SET operator_import = ?1 WHERE id = ?2",
            params![guest.is_none(), id],
        )?;
    }
    if let Some(user) = submitted_by {
        set_submitted_by(&tx, id, user)?;
    }
    let rules = tags::TagRules::load(&tx)?;
    let post_tags = tags::for_post(&tx, id)?;
    if let Some(hit) = rules.blocked_hit(&post_tags) {
        set_state(&tx, id, PostState::Rejected)?;
        set_notes(&tx, id, Some(&format!("auto-rejected: blocked tag {hit}")))?;
    }
    let suggestion = if let Some(user) = rated_guest {
        Some(reputation::register_new(&tx, id, user)?)
    } else {
        reputation::reattach_known(&tx, id, &post.source, &post.source_id, media)?;
        None
    };
    tx.commit()?;
    Ok(StoredImport {
        post_id: Some(id),
        created,
        suggestion,
    })
}

fn upsert_post(conn: &Connection, np: &NormalizedPost, media: &[NewMedia]) -> Result<(i64, bool)> {
    let now = Utc::now();
    let author_id = match &np.author {
        Some(author) => Some(upsert_author(conn, &np.source, author, now)?),
        None => None,
    };

    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM posts WHERE source = ?1 AND source_id = ?2",
            params![np.source, np.source_id],
            |row| row.get(0),
        )
        .optional()?;

    let post_id = match existing {
        Some(id) => {
            conn.execute(
                "UPDATE posts SET url = COALESCE(?1, url), author_id = COALESCE(?2, author_id),
                    title = COALESCE(?3, title), description = COALESCE(?4, description),
                    rating = ?5, posted_at = COALESCE(?6, posted_at), updated_at = ?7,
                    raw_json = COALESCE(?8, raw_json), notes = COALESCE(?10, notes)
                 WHERE id = ?9",
                params![
                    np.url,
                    author_id,
                    np.title,
                    np.description,
                    np.rating.as_str(),
                    np.posted_at.map(fmt_ts),
                    fmt_ts(now),
                    np.raw_json,
                    id,
                    np.notes
                ],
            )?;
            id
        }
        None => {
            conn.execute(
                "INSERT INTO posts (source, source_id, url, author_id, title, description, rating,
                    posted_at, imported_at, updated_at, raw_json, notes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11)",
                params![
                    np.source,
                    np.source_id,
                    np.url,
                    author_id,
                    np.title,
                    np.description,
                    np.rating.as_str(),
                    np.posted_at.map(fmt_ts),
                    fmt_ts(now),
                    np.raw_json,
                    np.notes
                ],
            )?;
            conn.last_insert_rowid()
        }
    };

    tags::refresh_source(conn, post_id, &np.tags)?;

    if !media.is_empty() {
        conn.execute("DELETE FROM media WHERE post_id = ?1", [post_id])?;
        for (ordinal, item) in media.iter().enumerate() {
            // self-heal the blob catalogue: media rows are FK-bound to it
            conn.execute(
                "INSERT INTO blobs (hash, ext, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (hash) DO NOTHING",
                params![item.blob_hash, item.ext, item.bytes, fmt_ts(Utc::now())],
            )?;
            if let Some(poster) = &item.poster_hash {
                conn.execute(
                    "INSERT INTO blobs (hash, ext, bytes, created_at) VALUES (?1, 'jpg', 0, ?2)
                     ON CONFLICT (hash) DO NOTHING",
                    params![poster, fmt_ts(Utc::now())],
                )?;
            }
            conn.execute(
                "INSERT INTO media (post_id, ordinal, blob_hash, mime, width, height, is_video, poster_hash, sensitive, meta_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    post_id,
                    ordinal as i64,
                    item.blob_hash,
                    item.mime,
                    item.width.map(|v| v as i64),
                    item.height.map(|v| v as i64),
                    if item.is_video { 1 } else { 0 },
                    item.poster_hash,
                    if item.sensitive { 1 } else { 0 },
                    item.meta_json
                ],
            )?;
        }
    }

    Ok((post_id, existing.is_none()))
}

fn upsert_author(
    conn: &Connection,
    source: &str,
    author: &crate::source::PostAuthor,
    now: DateTime<Utc>,
) -> Result<i64> {
    let id: i64 = conn.query_row(
        "INSERT INTO authors (source, source_id, name, handle, url, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (source, source_id) DO UPDATE SET
            name = COALESCE(excluded.name, authors.name),
            handle = COALESCE(excluded.handle, authors.handle),
            url = COALESCE(excluded.url, authors.url)
         RETURNING id",
        params![
            source,
            author.source_id,
            author.name,
            author.handle,
            author.url,
            fmt_ts(now)
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn get(conn: &Connection, id: i64) -> Result<Post> {
    let sql = format!(
        "SELECT {POST_COLUMNS} FROM posts p LEFT JOIN authors a ON a.id = p.author_id WHERE p.id = ?1"
    );
    let mut post = conn
        .query_row(&sql, [id], post_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("post {id}")))?;

    post.tags = tags::for_post(conn, id)?;

    let mut stmt = conn.prepare(
        "SELECT m.id, m.post_id, m.ordinal, m.blob_hash, m.mime, m.width, m.height, m.is_video,
                m.poster_hash, m.sensitive, m.meta_json, b.ext, b.bytes
         FROM media m JOIN blobs b ON b.hash = m.blob_hash
         WHERE m.post_id = ?1 ORDER BY m.ordinal",
    )?;
    let rows = stmt.query_map([id], media_from_row)?;
    for row in rows {
        post.media.push(row?);
    }
    Ok(post)
}

pub fn find_by_source_id(conn: &Connection, source: &str, source_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM posts WHERE source = ?1 AND source_id = ?2",
            params![source, source_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Post-card projection. A function rather than a const because the rules a card
/// depends on live elsewhere and must not be copied: what counts as nsfw
/// (`Rating::nsfw_sql`), what counts as delivered (`db::queue::DELIVERED_STATES`)
/// and how a submitter is named (`users::label`).
fn card_select() -> String {
    format!(
        "
    SELECT p.id, p.source, p.source_id, p.rating, p.state,
           {author} AS author_name,
           p.imported_at, p.description,
           COALESCE(p.nsfw_override, CASE WHEN {nsfw} THEN 1 ELSE 0 END) AS nsfw,
           (SELECT COUNT(*) FROM media m WHERE m.post_id = p.id) AS media_count,
           (SELECT COALESCE(m2.poster_hash, m2.blob_hash) FROM media m2 WHERE m2.post_id = p.id
             ORDER BY m2.ordinal LIMIT 1) AS thumb_hash,
           (SELECT COUNT(*) FROM queue q WHERE q.post_id = p.id) AS pushed,
           (SELECT COUNT(*) FROM queue q2 WHERE q2.post_id = p.id
             AND q2.state IN ({delivered})) AS delivered,
           (SELECT GROUP_CONCAT(ch.alias, ',') FROM queue q3
             JOIN channels ch ON ch.id = q3.channel_id WHERE q3.post_id = p.id) AS channels,
           p.submitted_by,
           {submitter} AS submitter,
           CASE WHEN {submitter_admin} THEN 1 ELSE 0 END AS submitter_admin
    FROM posts p LEFT JOIN authors a ON a.id = p.author_id
         LEFT JOIN users u ON u.tg_id = p.submitted_by",
        author = crate::model::author_label_sql("a"),
        nsfw = Rating::nsfw_sql("p.rating"),
        delivered = crate::db::queue::DELIVERED_STATES,
        // a user we have no row for still counts as "not an admin": better a
        // badge too many than a guest post nobody notices
        submitter = submitter_sql("p"),
        submitter_admin = "u.role = 'admin'",
    )
}

/// How a submitter is named: `@handle`, else first name, else the raw id.
/// One expression, shared by the card projection and the detail page.
fn submitter_sql(prefix: &str) -> String {
    format!(
        "CASE
                 WHEN u.username IS NOT NULL THEN '@' || u.username
                 WHEN u.first_name IS NOT NULL THEN u.first_name
                 ELSE CAST({prefix}.submitted_by AS TEXT) END"
    )
}

/// `(tg_id, label, is_admin)` of the Telegram user who submitted this post.
///
/// `None` when the post did not come through the bot. The label uses the same
/// expression as the card grid, so both pages name the user identically.
pub fn submitter(conn: &Connection, post_id: i64) -> Result<Option<(i64, String, bool)>> {
    let sql = format!(
        "SELECT p.submitted_by, {} AS submitter, COALESCE(u.role = 'admin', 0) AS admin
           FROM posts p LEFT JOIN users u ON u.tg_id = p.submitted_by
          WHERE p.id = ?1 AND p.submitted_by IS NOT NULL",
        submitter_sql("p")
    );
    Ok(conn
        .query_row(&sql, [post_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })
        .optional()?)
}

fn card_from_row(row: &Row<'_>) -> rusqlite::Result<PostCard> {
    let description: Option<String> = row.get("description")?;
    let submitted_by: Option<i64> = row.get("submitted_by")?;
    // a post with no submitter is not a guest submission: the `LEFT JOIN` leaves
    // `u.role` NULL there, which must not read as "someone who is not an admin"
    let submitter_admin = row.get::<_, i64>("submitter_admin")? != 0;
    Ok(PostCard {
        id: row.get("id")?,
        source: row.get("source")?,
        source_id: row.get("source_id")?,
        rating: Rating::parse(&row.get::<_, String>("rating")?),
        state: PostState::parse(&row.get::<_, String>("state")?),
        author_name: row.get("author_name")?,
        imported_at: get_ts_required(row, "imported_at")?,
        media_count: row.get("media_count")?,
        thumb_hash: row.get("thumb_hash")?,
        pushed: row.get("pushed")?,
        delivered: row.get("delivered")?,
        channels: row.get("channels")?,
        nsfw: row.get::<_, i64>("nsfw")? != 0,
        submitted_by,
        submitter: row.get("submitter")?,
        submitter_guest: submitted_by.is_some() && !submitter_admin,
        snippet: description.map(|d| d.lines().next().unwrap_or_default().to_string()),
    })
}

/// `(WHERE …, bound values)` for a card filter; every clause only touches `posts p`,
/// so `count_cards` can reuse it without the joins.
fn filter_sql(filter: &PostFilter) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();

    // "already sent" is one rule, shared with the queue: DELIVERED_STATES
    match filter.sent {
        SentFilter::Any => {}
        SentFilter::NotSent => clauses.push(format!(
            "p.id NOT IN (SELECT q.post_id FROM queue q WHERE q.state IN ({}))",
            crate::db::queue::DELIVERED_STATES
        )),
        SentFilter::Sent => clauses.push(format!(
            "p.id IN (SELECT q.post_id FROM queue q WHERE q.state IN ({}))",
            crate::db::queue::DELIVERED_STATES
        )),
    }

    if filter.without_rejected && filter.state.is_none() {
        clauses.push("p.state != 'rejected'".into());
    }
    if let Some(state) = filter.state {
        clauses.push("p.state = ?".into());
        values.push(Value::Text(state.as_str().into()));
    }
    if let Some(source) = &filter.source {
        clauses.push("p.source = ?".into());
        values.push(Value::Text(source.clone()));
    }
    if let Some(tag) = &filter.tag
        && let Some(norm) = tags::normalize(tag)
    {
        clauses.push(
            "p.id IN (SELECT pt.post_id FROM active_post_tags pt JOIN tags t ON t.id = pt.tag_id WHERE t.name = ?)"
                .into(),
        );
        values.push(Value::Text(norm));
    }
    if let Some(author_id) = filter.author_id {
        clauses.push("p.author_id = ?".into());
        values.push(Value::Integer(author_id));
    }
    if let Some(nsfw) = filter.nsfw {
        clauses.push(format!(
            "COALESCE(p.nsfw_override, CASE WHEN {} THEN 1 ELSE 0 END) = ?",
            Rating::nsfw_sql("p.rating")
        ));
        values.push(Value::Integer(if nsfw { 1 } else { 0 }));
    }
    if let Some(search) = &filter.search
        && !search.trim().is_empty()
    {
        clauses.push("(p.title LIKE ? OR p.description LIKE ? OR p.source_id = ?)".into());
        let like = format!("%{}%", search.trim());
        values.push(Value::Text(like.clone()));
        values.push(Value::Text(like));
        values.push(Value::Text(search.trim().to_string()));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, values)
}

pub fn cards(conn: &Connection, filter: &PostFilter) -> Result<Vec<PostCard>> {
    let (where_sql, values) = filter_sql(filter);
    let limit = if filter.limit > 0 { filter.limit } else { 60 };
    let sql = format!(
        "{}{where_sql} ORDER BY p.imported_at DESC, p.id DESC LIMIT {limit} OFFSET {}",
        card_select(),
        filter.offset.max(0)
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), card_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// How many posts match the filter (the pager total); `limit`/`offset` are ignored.
pub fn count_cards(conn: &Connection, filter: &PostFilter) -> Result<i64> {
    let (where_sql, values) = filter_sql(filter);
    let sql = format!("SELECT COUNT(*) FROM posts p{where_sql}");
    Ok(conn.query_row(&sql, params_from_iter(values), |row| row.get(0))?)
}

/// Posts imported through this Telegram user (`posts.submitted_by`).
pub fn count_submitted(conn: &Connection, tg_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM posts WHERE submitted_by = ?1",
        [tg_id],
        |row| row.get(0),
    )?)
}
/// `(raw_json, submitted_by)` for one post — what `show --raw` reports.
pub fn diagnostics(conn: &Connection, id: i64) -> Result<(Option<String>, Option<i64>)> {
    Ok(conn.query_row(
        "SELECT raw_json, submitted_by FROM posts WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

pub fn count(conn: &Connection, state: Option<PostState>) -> Result<i64> {
    let n = match state {
        Some(state) => conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE state = ?1",
            [state.as_str()],
            |row| row.get(0),
        )?,
        None => conn.query_row("SELECT COUNT(*) FROM posts", [], |row| row.get(0))?,
    };
    Ok(n)
}

pub fn set_state(conn: &Connection, id: i64, state: PostState) -> Result<()> {
    conn.execute(
        "UPDATE posts SET state = ?1, updated_at = ?2 WHERE id = ?3",
        params![state.as_str(), fmt_ts(Utc::now()), id],
    )?;
    Ok(())
}

pub fn set_nsfw_override(conn: &Connection, id: i64, value: Option<bool>) -> Result<()> {
    conn.execute(
        "UPDATE posts SET nsfw_override = ?1, updated_at = ?2 WHERE id = ?3",
        params![value.map(|v| if v { 1 } else { 0 }), fmt_ts(Utc::now()), id],
    )?;
    Ok(())
}

pub fn set_rating(conn: &Connection, id: i64, rating: Rating) -> Result<()> {
    conn.execute(
        "UPDATE posts SET rating = ?1, updated_at = ?2 WHERE id = ?3",
        params![rating.as_str(), fmt_ts(Utc::now()), id],
    )?;
    Ok(())
}

/// Remember who submitted the post (Telegram user id).
pub fn set_submitted_by(conn: &Connection, id: i64, tg_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE posts SET submitted_by = ?1 WHERE id = ?2 AND submitted_by IS NULL",
        params![tg_id, id],
    )?;
    Ok(())
}

pub fn set_notes(conn: &Connection, id: i64, notes: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE posts SET notes = ?1, updated_at = ?2 WHERE id = ?3",
        params![notes, fmt_ts(Utc::now()), id],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM posts WHERE id = ?1", [id])? > 0)
}

pub fn all_tags(conn: &Connection, limit: i64) -> Result<Vec<(String, i64)>> {
    tags::most_used(conn, limit)
}

/// Distinct sources present in the archive (for UI filters).
pub fn sources(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT source FROM posts ORDER BY source")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Fetch cards for a set of ids in one query (order not preserved).
pub fn cards_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<PostCard>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("{} WHERE p.id IN ({placeholders})", card_select());
    let values: Vec<Value> = ids.iter().map(|id| Value::Integer(*id)).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), card_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Newest approved post that is not queued for this channel yet.
pub fn next_unenqueued(
    conn: &Connection,
    channel_id: i64,
    approved_only: bool,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT p.id FROM posts p
             WHERE (p.state = 'approved' OR (NOT ?2 AND p.state = 'new'))
               AND p.nsfw_override IS NOT 0
               AND NOT EXISTS (
                   SELECT 1 FROM queue q WHERE q.post_id = p.id AND q.channel_id = ?1
               )
             ORDER BY p.imported_at DESC, p.id DESC LIMIT 1",
            params![channel_id, approved_only],
            |row| row.get(0),
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_sql_feeds_both_cards_and_count() {
        assert_eq!(filter_sql(&PostFilter::default()).0, "");

        let filter = PostFilter {
            state: Some(PostState::Approved),
            search: Some("cat".into()),
            ..Default::default()
        };
        let (sql, values) = filter_sql(&filter);
        assert!(sql.starts_with(" WHERE p.state = ?"), "{sql}");
        assert!(sql.contains("p.title LIKE ?"), "{sql}");
        // state + two LIKE patterns + the exact source_id match
        assert_eq!(values.len(), 4);

        // a tag filter is normalised before it reaches SQL
        let tagged = PostFilter {
            tag: Some("Night Sky".into()),
            ..Default::default()
        };
        let (sql, values) = filter_sql(&tagged);
        assert!(sql.contains("FROM active_post_tags"), "{sql}");
        assert_eq!(values.len(), 1);
    }

    /// The grid's "not rejected" default is one SQL clause, and an explicit state
    /// filter wins over it, so `?state=rejected` can never contradict itself.
    #[test]
    fn a_rejected_post_is_hidden_only_when_no_state_was_asked_for() {
        let hidden = PostFilter {
            without_rejected: true,
            ..Default::default()
        };
        let (sql, values) = filter_sql(&hidden);
        assert_eq!(sql, " WHERE p.state != 'rejected'", "{sql}");
        assert!(values.is_empty(), "it needs no parameter: {values:?}");

        for state in [PostState::Rejected, PostState::New] {
            let explicit = PostFilter {
                without_rejected: true,
                state: Some(state),
                ..Default::default()
            };
            let (sql, values) = filter_sql(&explicit);
            assert_eq!(sql, " WHERE p.state = ?", "{sql}");
            assert_eq!(values.len(), 1, "{values:?}");
        }

        // `?state=all` and the CLI drop the flag entirely
        let (sql, _) = filter_sql(&PostFilter::default());
        assert_eq!(sql, "", "{sql}");
    }

    #[test]
    fn the_sql_nsfw_rule_agrees_with_is_nsfw() {
        let db = crate::db::Db::open_in_memory().unwrap();
        for rating in Rating::all() {
            // the rule must hold for a literal rating, exactly as it does in a query
            let expr = Rating::nsfw_sql(&format!("'{}'", rating.as_str()));
            let sql = format!("SELECT CASE WHEN {expr} THEN 1 ELSE 0 END");
            let flag: i64 = db
                .with(|conn| Ok(conn.query_row(&sql, [], |row| row.get(0))?))
                .unwrap();
            assert_eq!(flag != 0, rating.is_nsfw(), "rating {rating}");
        }
    }

    #[test]
    fn a_sensitive_item_and_its_own_metadata_survive_the_round_trip() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let post = crate::source::NormalizedPost::new("twitter", "42");
        let media = vec![NewMedia {
            blob_hash: "hash-1".into(),
            ext: "png".into(),
            mime: None,
            width: Some(100),
            height: Some(100),
            bytes: 7,
            is_video: false,
            poster_hash: None,
            sensitive: true,
            meta_json: Some(r#"{"media_id":7,"sensitive":true}"#.into()),
        }];
        let id = db
            .with(|conn| {
                let (id, _) = upsert(conn, &post, &media)?;
                Ok(id)
            })
            .unwrap();
        let stored = db.with(|conn| get(conn, id)).unwrap();
        assert_eq!(stored.media.len(), 1);
        assert!(stored.media[0].sensitive, "the per-item flag is persisted");
        assert_eq!(
            stored.media[0].meta_json.as_deref(),
            Some(r#"{"media_id":7,"sensitive":true}"#),
            "the item's own extractor metadata is persisted"
        );
    }

    fn a_post(db: &crate::db::Db, label: &str) -> i64 {
        db.with(|conn| {
            let mut post = crate::source::NormalizedPost::new("pixiv", label);
            post.title = Some(format!("post {label}"));
            let (id, _) = upsert(conn, &post, &[])?;
            Ok(id)
        })
        .unwrap()
    }

    #[test]
    fn the_sent_filter_hides_what_already_went_out() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let delivered = a_post(&db, "1");
        let waiting = a_post(&db, "2");
        let channel_id = db
            .with(|conn| crate::db::channels::insert(conn, &crate::model::Channel::new("t", "@t")))
            .unwrap();
        db.with(|conn| {
            crate::db::queue::enqueue(conn, delivered, channel_id, 0)?;
            let queue_id: i64 = conn.query_row(
                "SELECT id FROM queue WHERE post_id = ?1",
                [delivered],
                |row| row.get(0),
            )?;
            crate::db::queue::mark_sent(conn, queue_id, "1", chrono::Utc::now())
        })
        .unwrap();

        let ids = |filter: PostFilter| {
            db.with(|conn| cards(conn, &filter))
                .unwrap()
                .iter()
                .map(|card| card.id)
                .collect::<Vec<_>>()
        };
        // the default (web) view hides what is already out, not what is queued
        let hidden = PostFilter {
            sent: SentFilter::NotSent,
            ..Default::default()
        };
        assert_eq!(ids(hidden.clone()), vec![waiting]);
        assert_eq!(db.with(|conn| count_cards(conn, &hidden)).unwrap(), 1);

        let only = PostFilter {
            sent: SentFilter::Sent,
            ..Default::default()
        };
        assert_eq!(ids(only), vec![delivered]);
        // `Any` is the CLI's view: everything
        assert_eq!(ids(PostFilter::default()).len(), 2);
    }

    #[test]
    fn cards_name_the_submitter_and_flag_guests() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let post_id = a_post(&db, "1");
        db.with(|conn| {
            conn.execute(
                "INSERT INTO users (tg_id, username, first_name, role, created_at)
                 VALUES (42, 'fan', NULL, 'guest', '2026-01-01T00:00:00Z')",
                [],
            )?;
            set_submitted_by(conn, post_id, 42)
        })
        .unwrap();

        let card = db
            .with(|conn| cards(conn, &PostFilter::default()))
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(card.submitted_by, Some(42));
        assert_eq!(
            card.submitter.as_deref(),
            Some("@fan"),
            "the handle is shown"
        );
        assert!(card.submitter_guest, "a guest is flagged");

        // promoting the user clears the flag: same post, different role
        db.with(|conn| {
            conn.execute("UPDATE users SET role = 'admin' WHERE tg_id = 42", [])?;
            Ok(())
        })
        .unwrap();
        let card = db
            .with(|conn| cards(conn, &PostFilter::default()))
            .unwrap()
            .pop()
            .unwrap();
        assert!(!card.submitter_guest, "an admin submission is not flagged");

        // the detail page names the user the same way
        let (tg_id, label, admin) = db
            .with(|conn| submitter(conn, post_id))
            .unwrap()
            .expect("a submitter");
        assert_eq!((tg_id, label.as_str(), admin), (42, "@fan", true));
        // a post that did not come through the bot is not a guest submission:
        // the LEFT JOIN leaves `users.role` NULL, which once read as "guest"
        let plain = a_post(&db, "2");
        assert_eq!(db.with(|conn| submitter(conn, plain)).unwrap(), None);
        let plain_card = db
            .with(|conn| cards(conn, &PostFilter::default()))
            .unwrap()
            .into_iter()
            .find(|card| card.id == plain)
            .expect("the plain post");
        assert!(
            !plain_card.submitter_guest,
            "no submitter is not the same as a guest submitter"
        );
        assert_eq!(plain_card.submitter, None);
    }
}
