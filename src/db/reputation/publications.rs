//! Match and snapshot Telegram delivery records and message links.

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::posts;
use crate::error::Result;
use crate::model::Post;

use super::suggestions::{
    MatchedSuggestion, Suggestion, attach_known, lookup_key, post_for_subject, record_suggestion,
    set_key,
};

fn telegram_key(target: &str, message: i32) -> String {
    format!("{}:{message}", target.to_ascii_lowercase())
}

pub(super) struct LegacyDelivery {
    pub target: String,
    pub ids: Vec<i32>,
}

pub(crate) fn parse_message_ids(raw: &str) -> Vec<i32> {
    raw.split(',')
        .filter_map(|id| id.trim().parse::<i32>().ok())
        .filter(|&id| id > 0)
        .collect()
}

pub(super) fn legacy_publication(
    conn: &Connection,
    post_id: i64,
) -> Result<Option<LegacyDelivery>> {
    // Prefer an actual message ID over old manual confirmations without IDs.
    // Success audit rows can outlive their queue row; use the import timestamp
    // to guard against SQLite reusing a deleted post's numeric ID.
    Ok(conn
        .query_row(
            "SELECT target, ids FROM (
            SELECT COALESCE(q.delivery_target, c.tg_id) AS target, q.tg_message_ids AS ids, q.sent_at AS sent_at
            FROM queue q JOIN channels c ON c.id = q.channel_id
            WHERE q.post_id = ?1 AND q.state = 'sent'
            UNION
            SELECT c.tg_id, s.tg_message_ids, s.sent_at FROM sends s
            JOIN channels c ON c.id = s.channel_id JOIN posts p ON p.id = s.post_id
            WHERE s.post_id = ?1 AND s.ok AND s.sent_at >= p.imported_at
         ) ORDER BY COALESCE(ids, '') = '', sent_at DESC LIMIT 1",
            [post_id],
            |row| {
                Ok(LegacyDelivery {
                    target: row.get(0)?,
                    ids: parse_message_ids(&row.get::<_, Option<String>>(1)?.unwrap_or_default()),
                })
            },
        )
        .optional()?)
}

pub fn known_message(
    conn: &Connection,
    targets: &[String],
    message: i32,
    user: i64,
) -> Result<Option<MatchedSuggestion>> {
    for target in targets {
        if let Some(subject) = lookup_key(conn, "telegram", &telegram_key(target, message))? {
            return Ok(Some(record_suggestion(
                conn,
                subject,
                post_for_subject(conn, subject)?,
                user,
                false,
            )?));
        }
        if let Some(post_id) = unconfirmed_message(conn, target, message)? {
            return Ok(Some(MatchedSuggestion {
                post_id: Some(post_id),
                status: Suggestion::Unconfirmed,
            }));
        }
        let mut stmt = conn.prepare(
            "SELECT post_id, tg_message_ids FROM queue q JOIN channels c ON c.id = q.channel_id
             WHERE q.state = 'sent' AND lower(COALESCE(q.delivery_target, c.tg_id)) = lower(?1) AND q.tg_message_ids IS NOT NULL
             UNION
             SELECT p.id, s.tg_message_ids FROM sends s JOIN posts p ON p.id = s.post_id
             JOIN channels c ON c.id = s.channel_id WHERE s.ok AND s.sent_at >= p.imported_at
             AND lower(c.tg_id) = lower(?1) AND s.tg_message_ids IS NOT NULL",
        )?;
        let deliveries = stmt.query_map([target], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                parse_message_ids(&row.get::<_, String>(1)?),
            ))
        })?;
        for delivery in deliveries {
            let (post_id, ids) = delivery?;
            if ids.contains(&message) {
                let post = posts::get(conn, post_id)?;
                let subject = attach_known(conn, &post, false)?;
                save_delivery(conn, subject, target, None, None, &ids)?;
                return Ok(Some(record_suggestion(
                    conn,
                    subject,
                    Some(post_id),
                    user,
                    false,
                )?));
            }
        }
    }
    Ok(None)
}

fn unconfirmed_message(conn: &Connection, target: &str, message: i32) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT q.post_id, q.tg_message_ids FROM queue q JOIN channels c ON c.id = q.channel_id
         WHERE q.state = 'unknown' AND q.tg_message_ids IS NOT NULL
           AND (lower(COALESCE(q.delivery_target, c.tg_id)) = lower(?1)
                OR CAST(q.delivery_chat_id AS TEXT) = ?1 OR lower('@' || q.delivery_username) = lower(?1))",
    )?;
    let rows = stmt.query_map([target], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            parse_message_ids(&row.get::<_, String>(1)?),
        ))
    })?;
    for row in rows {
        let (post_id, ids) = row?;
        if ids.contains(&message) {
            return Ok(Some(post_id));
        }
    }
    Ok(None)
}

pub fn record_publication(
    conn: &Connection,
    post: &Post,
    target: &str,
    chat: Option<i64>,
    username: Option<&str>,
    ids: &[i32],
) -> Result<()> {
    let subject = attach_known(conn, post, false)?;
    save_delivery(conn, subject, target, chat, username, ids)
}

pub(super) fn save_delivery(
    conn: &Connection,
    subject: i64,
    target: &str,
    chat: Option<i64>,
    username: Option<&str>,
    ids: &[i32],
) -> Result<()> {
    let chat = chat.or_else(|| target.parse().ok());
    let username = username.or_else(|| target.strip_prefix('@'));
    for id in ids
        .iter()
        .rev()
        .copied()
        .filter(|&id| id > 0)
        .map(Some)
        .chain(ids.is_empty().then_some(None))
    {
        let inserted = conn.execute(
            "INSERT INTO reputation_publications (subject_id, target, chat_id, username, message_id)
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE NOT EXISTS (
                SELECT 1 FROM reputation_publications WHERE subject_id = ?1 AND target = ?2
                    AND chat_id IS ?3 AND username IS ?4 AND message_id IS ?5
             )",
            params![subject, target, chat, username, id],
        )?;
        if inserted != 0
            && let Some(id) = id
        {
            set_key(conn, subject, "telegram", &telegram_key(target, id))?;
            if let Some(chat) = chat {
                set_key(
                    conn,
                    subject,
                    "telegram",
                    &telegram_key(&chat.to_string(), id),
                )?;
            }
            if let Some(username) = username {
                set_key(
                    conn,
                    subject,
                    "telegram",
                    &telegram_key(&format!("@{username}"), id),
                )?;
            }
        }
    }
    Ok(())
}

pub(super) fn publication(conn: &Connection, subject: i64) -> Result<Option<Option<String>>> {
    let mut stmt = conn.prepare(
        "SELECT chat_id, username, message_id FROM reputation_publications WHERE subject_id = ?1 ORDER BY id DESC",
    )?;
    let mut rows = stmt.query([subject])?;
    let mut found = false;
    while let Some(row) = rows.next()? {
        found = true;
        let chat: Option<i64> = row.get(0)?;
        let username: Option<String> = row.get(1)?;
        let id: Option<i32> = row.get(2)?;
        if let Some(id) = id
            && let Some(link) = message_link(chat, username.as_deref(), id)
        {
            return Ok(Some(Some(link)));
        }
    }
    Ok(found.then_some(None))
}

pub(crate) fn message_link(chat: Option<i64>, username: Option<&str>, id: i32) -> Option<String> {
    if let Some(username) = username.filter(|name| {
        !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
    }) {
        return Some(format!("https://t.me/{username}/{id}"));
    }
    let chat = chat?.to_string();
    let internal = chat.strip_prefix("-100").filter(|id| !id.is_empty())?;
    Some(format!("https://t.me/c/{internal}/{id}"))
}
