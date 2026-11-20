//! Queue persistence, including the lease protocol that keeps publishing idempotent.

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::error::{Error, Result};
use crate::model::{PostCard, QueueItem, QueueState};

use super::{fmt_ts, get_bool, get_ts, get_ts_required, posts};

const COLUMNS: &str = "id, post_id, channel_id, state, priority, pinned, lease_token, lease_at,
    attempts, last_error, sent_at, tg_message_ids, added_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<QueueItem> {
    Ok(QueueItem {
        id: row.get("id")?,
        post_id: row.get("post_id")?,
        channel_id: row.get("channel_id")?,
        state: QueueState::parse(&row.get::<_, String>("state")?),
        priority: row.get("priority")?,
        pinned: get_bool(row, "pinned")?,
        lease_token: row.get("lease_token")?,
        lease_at: get_ts(row, "lease_at")?,
        attempts: row.get("attempts")?,
        last_error: row.get("last_error")?,
        sent_at: get_ts(row, "sent_at")?,
        tg_message_ids: row.get("tg_message_ids")?,
        added_at: get_ts_required(row, "added_at")?,
    })
}

#[derive(Debug, Clone)]
pub struct QueueEntry {
    pub item: QueueItem,
    pub card: PostCard,
}

/// Add a post to a channel queue. Returns false when it was already there.
pub fn enqueue(conn: &Connection, post_id: i64, channel_id: i64, priority: i64) -> Result<bool> {
    Ok(enqueue_new(conn, post_id, channel_id, priority)?.is_some())
}

pub fn enqueue_new(
    conn: &Connection,
    post_id: i64,
    channel_id: i64,
    priority: i64,
) -> Result<Option<i64>> {
    enqueue_with_state(conn, post_id, channel_id, priority, QueueState::Queued)
}

pub(crate) fn enqueue_with_state(
    conn: &Connection,
    post_id: i64,
    channel_id: i64,
    priority: i64,
    state: QueueState,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "INSERT INTO queue (post_id, channel_id, state, priority, pinned, added_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT (post_id, channel_id) DO NOTHING
         RETURNING id",
            params![
                post_id,
                channel_id,
                state.as_str(),
                priority,
                fmt_ts(Utc::now())
            ],
            |row| row.get(0),
        )
        .optional()?)
}

/// Move a reviewed post into the publishable queue. The caller checks policy
/// and enables the destination in the same transaction.
pub fn release_held(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'queued' WHERE id = ?1 AND state = 'held'",
        [id],
    )?;
    Ok(())
}

pub fn has_confirmed_delivery(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM queue WHERE state = 'sent')
             OR EXISTS(SELECT 1 FROM sends WHERE ok = 1)",
        [],
        |row| row.get(0),
    )?)
}

pub fn count_successful_sends(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM sends WHERE ok = 1", [], |row| {
            row.get(0)
        })?,
    )
}

pub fn count_all(conn: &Connection, state: QueueState) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM queue WHERE state = ?1",
        [state.as_str()],
        |row| row.get(0),
    )?)
}

pub fn get(conn: &Connection, id: i64) -> Result<QueueItem> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM queue WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("queue item {id}")))
}

pub fn list(
    conn: &Connection,
    channel_id: i64,
    states: &[QueueState],
    limit: i64,
) -> Result<Vec<QueueItem>> {
    let mut sql = format!("SELECT {COLUMNS} FROM queue WHERE channel_id = ?1");
    let mut values: Vec<Value> = vec![Value::Integer(channel_id)];
    if !states.is_empty() {
        let placeholders = states.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        sql.push_str(&format!(" AND state IN ({placeholders})"));
        for state in states {
            values.push(Value::Text(state.as_str().into()));
        }
    }
    sql.push_str(&format!(
        " ORDER BY pinned DESC, priority DESC, added_at ASC LIMIT {}",
        if limit > 0 { limit } else { -1 }
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values), from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Queue items joined with their post cards, in publish order.
pub fn entries(
    conn: &Connection,
    channel_id: i64,
    states: &[QueueState],
    limit: i64,
) -> Result<Vec<QueueEntry>> {
    let items = list(conn, channel_id, states, limit)?;
    let ids: Vec<i64> = items.iter().map(|i| i.post_id).collect();
    let cards = posts::cards_by_ids(conn, &ids)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let Some(card) = cards.iter().find(|c| c.id == item.post_id) {
            out.push(QueueEntry {
                item: item.clone(),
                card: card.clone(),
            });
        }
    }
    Ok(out)
}

/// Candidates for the next publish, in priority order.
pub fn candidates(conn: &Connection, channel_id: i64) -> Result<Vec<QueueItem>> {
    let mut items = list(conn, channel_id, &[QueueState::Queued], 0)?;
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.priority.cmp(&a.priority))
            .then(a.added_at.cmp(&b.added_at))
    });
    Ok(items)
}

/// Queue rows for one post: `(queue_id, channel_id, state)`.
///
/// Answers "is this post queued, and where" without loading the whole queue: the
/// post page only needs its own rows.
pub fn for_post(conn: &Connection, post_id: i64) -> Result<Vec<(i64, i64, QueueState)>> {
    let mut stmt = conn.prepare(
        "SELECT id, channel_id, state FROM queue WHERE post_id = ?1 ORDER BY channel_id, id",
    )?;
    let rows = stmt.query_map([post_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            QueueState::parse(&row.get::<_, String>(2)?),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(serde::Serialize)]
pub struct UnconfirmedDelivery {
    pub id: i64,
    pub channel_id: i64,
    pub post_id: i64,
    pub error: Option<String>,
    pub channel: String,
    pub messages: Vec<String>,
    pub public_channel: Option<String>,
}

pub fn unconfirmed(conn: &Connection, channel_id: Option<i64>) -> Result<Vec<UnconfirmedDelivery>> {
    let mut stmt = conn.prepare(
        "SELECT q.id, q.channel_id, q.post_id, q.last_error, c.title, q.tg_message_ids,
                COALESCE(q.delivery_target, c.tg_id), q.delivery_chat_id, q.delivery_username
         FROM queue q JOIN channels c ON c.id = q.channel_id
         WHERE q.state = 'unknown' AND (?1 IS NULL OR q.channel_id = ?1) ORDER BY q.id DESC LIMIT 200",
    )?;
    Ok(stmt
        .query_map([channel_id], |row| {
            let target: String = row.get(6)?;
            let chat: Option<i64> = row.get(7)?;
            let username: Option<String> = row.get(8)?;
            let username = username
                .as_deref()
                .or_else(|| target.strip_prefix('@'))
                .filter(|name| {
                    !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                });
            let chat = chat.or_else(|| target.parse().ok());
            let ids: Option<String> = row.get(5)?;
            let messages = super::reputation::parse_message_ids(ids.as_deref().unwrap_or_default())
                .into_iter()
                .filter_map(|id| super::reputation::message_link(chat, username, id))
                .collect();
            Ok(UnconfirmedDelivery {
                id: row.get(0)?,
                channel_id: row.get(1)?,
                post_id: row.get(2)?,
                error: row.get(3)?,
                channel: row.get(4)?,
                messages,
                public_channel: username.map(|name| format!("https://t.me/{name}")),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Channel and queue state badges for the posts on the current grid page.
pub fn card_states(conn: &Connection, ids: &[i64]) -> Result<Vec<(i64, String, QueueState)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let slots = vec!["?"; ids.len()].join(", ");
    let mut statement = conn.prepare(&format!(
        "SELECT q.post_id, c.alias, q.state FROM queue q
         JOIN channels c ON c.id = q.channel_id WHERE q.post_id IN ({slots}) ORDER BY c.alias"
    ))?;
    Ok(statement
        .query_map(params_from_iter(ids), |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                QueueState::parse(&row.get::<_, String>(2)?),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Take the lease for a queue item. False when another worker got there first.
pub fn acquire(conn: &Connection, id: i64, token: &str) -> Result<bool> {
    acquire_for_target(conn, id, token, None)
}

pub fn acquire_for_target(
    conn: &Connection,
    id: i64,
    token: &str,
    target: Option<&str>,
) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE queue SET state = 'sending', lease_token = ?1, lease_at = ?2,
            attempts = attempts + 1, delivery_target = ?3, delivery_chat_id = NULL, delivery_username = NULL
         WHERE id = ?4 AND state = 'queued'",
        params![token, fmt_ts(Utc::now()), target, id],
    )?;
    Ok(updated > 0)
}

pub fn mark_partial(
    conn: &Connection,
    id: i64,
    error: &str,
    ids: &str,
    chat: Option<i64>,
    username: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'unknown', last_error = ?1, tg_message_ids = ?2,
            delivery_chat_id = ?3, delivery_username = ?4, lease_token = NULL, lease_at = NULL WHERE id = ?5",
        params![error, ids, chat, username, id],
    )?;
    Ok(())
}

pub fn mark_sent(
    conn: &Connection,
    id: i64,
    message_ids: &str,
    sent_at: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'sent', sent_at = ?1, tg_message_ids = ?2, lease_token = NULL,
            lease_at = NULL, last_error = NULL
         WHERE id = ?3",
        params![fmt_ts(sent_at), message_ids, id],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, id: i64, error: &str) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'failed', last_error = ?1, lease_token = NULL, lease_at = NULL
         WHERE id = ?2",
        params![error, id],
    )?;
    Ok(())
}

/// Hand a leased item back without consuming an attempt.
pub fn release(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'queued', lease_token = NULL, lease_at = NULL,
            attempts = MAX(attempts - 1, 0)
         WHERE id = ?1 AND state = 'sending'",
        [id],
    )?;
    Ok(())
}

pub fn skip(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'skipped', lease_token = NULL, lease_at = NULL WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Put a finished item back in the queue (retry / re-run).
pub fn requeue(conn: &Connection, id: i64) -> Result<()> {
    let updated = conn.execute(
        "UPDATE queue SET state = 'queued', lease_token = NULL, lease_at = NULL, last_error = NULL,
            sent_at = NULL, tg_message_ids = NULL
         WHERE id = ?1 AND state <> 'held'",
        [id],
    )?;
    if updated == 0 && get(conn, id).is_ok_and(|item| item.state == QueueState::Held) {
        return Err(Error::invalid("confirm the held post from its preview"));
    }
    Ok(())
}

pub fn set_pinned(conn: &Connection, id: i64, pinned: bool) -> Result<()> {
    conn.execute(
        "UPDATE queue SET pinned = ?1 WHERE id = ?2",
        params![pinned as i64, id],
    )?;
    Ok(())
}

pub fn set_priority(conn: &Connection, id: i64, priority: i64) -> Result<()> {
    conn.execute(
        "UPDATE queue SET priority = ?1 WHERE id = ?2",
        params![priority, id],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM queue WHERE id = ?1", [id])? > 0)
}

/// A `sending` row older than `cutoff` means the process died mid-publish.
/// Those become `unknown`: never silently re-sent.
pub fn reconcile_stale(conn: &Connection, cutoff: DateTime<Utc>) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE queue SET state = 'unknown', lease_token = NULL,
            last_error = COALESCE(last_error, 'interrupted while sending; delivery unconfirmed')
         WHERE state = 'sending' AND lease_at IS NOT NULL AND lease_at < ?1",
        [fmt_ts(cutoff)],
    )?)
}

/// Resolve an `unknown` item by hand.
pub fn resolve_unknown(
    conn: &mut Connection,
    id: i64,
    delivered: bool,
    message_ids: Option<&str>,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let item = get(&tx, id)?;
    if item.state != QueueState::Unknown {
        return Err(Error::invalid("only an unconfirmed send can be resolved"));
    }
    if delivered {
        let mut ids = super::reputation::parse_message_ids(message_ids.unwrap_or_default());
        for id in
            super::reputation::parse_message_ids(item.tg_message_ids.as_deref().unwrap_or_default())
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let recorded_ids =
            (!ids.is_empty()).then(|| ids.iter().map(i32::to_string).collect::<Vec<_>>().join(","));
        tx.execute(
            "UPDATE queue SET state = 'sent', sent_at = COALESCE(sent_at, ?1),
                tg_message_ids = COALESCE(?2, tg_message_ids), lease_token = NULL, lease_at = NULL
             WHERE id = ?3",
            params![fmt_ts(Utc::now()), recorded_ids, id],
        )?;
        let post = posts::get(&tx, item.post_id)?;
        let (target, chat, username): (String, Option<i64>, Option<String>) = tx.query_row(
            "SELECT COALESCE(q.delivery_target, c.tg_id), q.delivery_chat_id, q.delivery_username
             FROM queue q JOIN channels c ON c.id = q.channel_id WHERE q.id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        super::reputation::record_publication(
            &tx,
            &post,
            &target,
            chat,
            username.as_deref(),
            &ids,
        )?;
    } else {
        tx.execute(
            "UPDATE queue SET state = 'queued', lease_token = NULL, lease_at = NULL, last_error = NULL, tg_message_ids = NULL
             WHERE id = ?1",
            [id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn count_state(conn: &Connection, channel_id: i64, state: QueueState) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM queue WHERE channel_id = ?1 AND state = ?2",
        params![channel_id, state.as_str()],
        |row| row.get(0),
    )?)
}

/// Queue states that mean "Telegram probably has this": a `sending` row may have
/// delivered before the process died, and a stale lease becomes `unknown` and is
/// never re-sent automatically. Defined once, so every "did it go out?" query
/// (post cards, user stats, dashboards) answers the same way.
pub const DELIVERED_STATES: &str = "'sent', 'sending', 'unknown'";

/// `(pushed, sent)` channel slots belonging to this user's own imports: how many
/// times their posts were pushed, and how many of those already went out.
pub fn counts_submitted(conn: &Connection, tg_id: i64) -> Result<(i64, i64)> {
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(CASE WHEN q.state IN ({DELIVERED_STATES}) THEN 1 ELSE 0 END), 0)
           FROM queue q JOIN posts p ON p.id = q.post_id
          WHERE p.submitted_by = ?1"
    );
    Ok(conn.query_row(&sql, [tg_id], |row| Ok((row.get(0)?, row.get(1)?)))?)
}
pub fn counts(conn: &Connection, channel_id: i64) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT state, COUNT(*) FROM queue WHERE channel_id = ?1 GROUP BY state ORDER BY state",
    )?;
    let rows = stmt.query_map([channel_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Authors of the last `limit` successful sends in a channel (artist cooldown).
pub fn recent_authors(conn: &Connection, channel_id: i64, limit: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT p.author_id FROM queue q JOIN posts p ON p.id = q.post_id
         WHERE q.channel_id = ?1 AND q.state = 'sent' AND p.author_id IS NOT NULL
         ORDER BY q.sent_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![channel_id, limit], |row| row.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn sends_since(conn: &Connection, channel_id: i64, since: DateTime<Utc>) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM sends WHERE channel_id = ?1 AND ok = 1 AND sent_at >= ?2",
        params![channel_id, fmt_ts(since)],
        |row| row.get(0),
    )?)
}

/// One row in the send audit log.
#[derive(Debug, Clone, Copy)]
pub struct SendRecord<'a> {
    pub channel_id: i64,
    pub queue_id: i64,
    pub post_id: i64,
    pub ok: bool,
    pub message_ids: Option<&'a str>,
    pub caption: Option<&'a str>,
    pub error: Option<&'a str>,
}

pub fn log_send(conn: &Connection, record: SendRecord<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO sends (channel_id, queue_id, post_id, sent_at, ok, tg_message_ids, caption, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            record.channel_id,
            record.queue_id,
            record.post_id,
            fmt_ts(Utc::now()),
            record.ok as i64,
            record.message_ids,
            record.caption,
            record.error
        ],
    )?;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct DeliveryAttempt {
    pub id: i64,
    pub post_id: Option<i64>,
    pub state: &'static str,
    pub error: Option<String>,
}

pub fn latest_attempt(conn: &Connection, channel_id: i64) -> Result<Option<DeliveryAttempt>> {
    Ok(conn
        .query_row(
            "SELECT s.id, s.post_id, s.ok, s.error, q.state FROM sends s
         LEFT JOIN queue q ON q.id = s.queue_id AND q.channel_id = s.channel_id
         WHERE s.channel_id = ?1 ORDER BY s.id DESC LIMIT 1",
            [channel_id],
            |row| {
                let queue_state: Option<String> = row.get(4)?;
                let state = if get_bool(row, "ok")? {
                    "Sent"
                } else if queue_state.as_deref() == Some("unknown") {
                    "Delivery unconfirmed"
                } else {
                    "Failed"
                };
                Ok(DeliveryAttempt {
                    id: row.get(0)?,
                    post_id: row.get(1)?,
                    state,
                    error: row.get(3)?,
                })
            },
        )
        .optional()?)
}

pub fn recent_sends(conn: &Connection, limit: i64) -> Result<Vec<crate::model::SendLog>> {
    let mut stmt = conn.prepare(
        "SELECT id, channel_id, post_id, sent_at, ok, tg_message_ids, caption, error
         FROM sends ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| {
        Ok(crate::model::SendLog {
            id: row.get("id")?,
            channel_id: row.get("channel_id")?,
            post_id: row.get("post_id")?,
            sent_at: get_ts_required(row, "sent_at")?,
            ok: get_bool(row, "ok")?,
            tg_message_ids: row.get("tg_message_ids")?,
            caption: row.get("caption")?,
            error: row.get("error")?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Tags of the last `limit` sent posts in a channel (feed variety heuristic).
pub fn recent_tags(conn: &Connection, channel_id: i64, limit: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.name FROM queue q
         JOIN active_post_tags pt ON pt.post_id = q.post_id
         JOIN tags t ON t.id = pt.tag_id
         WHERE q.channel_id = ?1 AND q.state = 'sent'
           AND q.post_id IN (
             SELECT post_id FROM queue
             WHERE channel_id = ?1 AND state = 'sent'
             ORDER BY sent_at DESC LIMIT ?2
           )",
    )?;
    let rows = stmt.query_map(params![channel_id, limit], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Mark a queued item skipped with a human reason (policy veto at send time).
pub fn skip_with_reason(conn: &Connection, id: i64, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE queue SET state = 'skipped', last_error = ?1, lease_token = NULL, lease_at = NULL
         WHERE id = ?2 AND state = 'queued'",
        params![reason, id],
    )?;
    Ok(())
}
