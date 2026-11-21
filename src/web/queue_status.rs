//! Lightweight status for a channel queue, shared with the initial page view.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::Connection;
use serde::Serialize;

use crate::db::{channels, jobs, queue};
use crate::error::Result;
use crate::model::{Channel, QueueState};

use super::{WebState, routes::WebResult};

pub(super) const VISIBLE_STATES: &[QueueState] = &[
    QueueState::Queued,
    QueueState::Held,
    QueueState::Unknown,
    QueueState::Failed,
    QueueState::Skipped,
    QueueState::Sending,
];
pub(super) const PAGE_LIMIT: i64 = 200;

#[derive(Serialize)]
pub(super) struct QueueActivity {
    pub revision: String,
    queued: i64,
    jobs: Vec<PublishActivity>,
    latest_attempt: Option<queue::DeliveryAttempt>,
}

#[derive(Serialize)]
struct PublishActivity {
    id: i64,
    queue_id: Option<i64>,
    kind: String,
    label: &'static str,
    state: String,
    message: Option<String>,
    progress: i64,
    total: i64,
}

pub(super) fn snapshot(conn: &Connection, channel: &Channel) -> Result<QueueActivity> {
    let items = queue::list(conn, channel.id, VISIBLE_STATES, PAGE_LIMIT)?;
    let rows: Vec<_> = items
        .iter()
        .map(|item| {
            (
                item.id,
                item.post_id,
                item.state.as_str(),
                item.pinned,
                item.priority,
                item.attempts,
                &item.last_error,
                item.added_at,
                &item.tg_message_ids,
            )
        })
        .collect();
    let jobs: Vec<_> = jobs::channel_publish_activity(conn, channel.id)?
        .into_iter()
        .map(|job| {
            let queue_id = (job.kind == crate::jobs::KIND_PUBLISH_ITEM)
                .then(|| job.payload.as_deref()?.parse().ok())
                .flatten();
            let label = match job.state.as_str() {
                "pending" => "Queued",
                "running" => "Running",
                "failed" => "Failed",
                _ => "Finished",
            };
            PublishActivity {
                id: job.id,
                queue_id,
                kind: job.kind,
                label,
                state: job.state,
                message: job.message,
                progress: job.progress,
                total: job.total,
            }
        })
        .collect();
    let latest_attempt = queue::latest_attempt(conn, channel.id)?;
    let queued = queue::count_state(conn, channel.id, QueueState::Queued)?;
    let inputs = serde_json::to_vec(&(
        rows,
        &jobs,
        &latest_attempt,
        queued,
        &channel.title,
        &channel.tg_id,
        channel.enabled,
        channel.paused,
        channel.next_due_at,
        channel.interval_minutes,
        &channel.active_from,
        &channel.active_to,
        &channel.tz,
        channel.daily_cap,
        channel.posts_per_slot,
    ))?;
    Ok(QueueActivity {
        revision: blake3::hash(&inputs).to_hex().to_string(),
        queued,
        jobs,
        latest_attempt,
    })
}

pub(super) async fn get(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Json<QueueActivity>> {
    let activity = ws
        .app
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            snapshot(&tx, &channels::get(&tx, id)?)
        })
        .await?;
    Ok(Json(activity))
}
