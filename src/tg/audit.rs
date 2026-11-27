//! Send audit trail: every attempt lands in the `sends` table.

use crate::db::queue::{self as queue_db, SendRecord};
use crate::error::Result;
use crate::model::SendLog;
use crate::state::AppState;

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
                    destination: None,
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
