//! Send audit trail: every attempt lands in the `sends` table.

use crate::db::queue::{self as queue_db, SendRecord};
use crate::error::Result;
use crate::model::SendLog;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct SendAttempt<'a> {
    pub channel_id: i64,
    pub queue_id: i64,
    pub post_id: i64,
    pub caption: Option<&'a str>,
    pub message_ids: &'a [i32],
}

pub async fn record_success(state: &AppState, attempt: SendAttempt<'_>) -> Result<()> {
    let ids = if attempt.message_ids.is_empty() {
        None
    } else {
        Some(
            attempt
                .message_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
    };
    let caption = attempt.caption.map(|c| c.to_string());
    state
        .db
        .call(move |conn| {
            queue_db::log_send(
                conn,
                SendRecord {
                    channel_id: attempt.channel_id,
                    queue_id: attempt.queue_id,
                    post_id: attempt.post_id,
                    ok: true,
                    message_ids: ids.as_deref(),
                    caption: caption.as_deref(),
                    error: None,
                },
            )
        })
        .await
}

pub async fn record_failure(
    state: &AppState,
    channel_id: i64,
    queue_id: i64,
    post_id: i64,
    error: &str,
) -> Result<i64> {
    let error = error.to_string();
    state
        .db
        .call(move |conn| {
            queue_db::log_send(
                conn,
                SendRecord {
                    channel_id,
                    queue_id,
                    post_id,
                    ok: false,
                    message_ids: None,
                    caption: None,
                    error: Some(&error),
                },
            )?;
            crate::db::channels::record_failure(conn, channel_id, 3)
        })
        .await
}

pub async fn recent(state: &AppState, limit: i64) -> Result<Vec<SendLog>> {
    state
        .db
        .call(move |conn| queue_db::recent_sends(conn, limit))
        .await
}
