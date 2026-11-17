//! Queue mechanics: when to publish, which post, and how to keep it idempotent.

pub mod lease;
pub mod scheduler;
pub mod selection;

pub use scheduler::{is_active, is_due, local_day_start, next_active_start, next_due};
pub use selection::{Candidate, Selection};

use crate::error::Result;
use crate::model::{Channel, NsfwPolicy, PostState};
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq)]
pub enum EnqueueOutcome {
    Queued(i64),
    AlreadyQueued,
    Skipped(String),
}

impl EnqueueOutcome {
    pub fn describe(&self) -> String {
        match self {
            EnqueueOutcome::Queued(id) => format!("queued as #{id}"),
            EnqueueOutcome::AlreadyQueued => "already in the queue".to_string(),
            EnqueueOutcome::Skipped(reason) => format!("skipped: {reason}"),
        }
    }
}

/// Enqueue a post after applying the channel's policy. Skipped posts are not queued.
pub async fn enqueue_checked(
    state: &AppState,
    channel: &Channel,
    post_id: i64,
    priority: i64,
) -> Result<EnqueueOutcome> {
    let rules = state
        .db
        .call(|conn| crate::db::tags::TagRules::load(conn))
        .await?;
    let post = state
        .db
        .call(move |conn| crate::db::posts::get(conn, post_id))
        .await?;
    let decision = crate::nsfw::evaluate(&post, channel, &rules);
    if let crate::nsfw::Verdict::Reject(reason) = decision.verdict {
        return Ok(EnqueueOutcome::Skipped(reason));
    }
    // blocking check for channels that veto NSFW
    if decision.nsfw && channel.nsfw_policy == NsfwPolicy::Reject {
        return Ok(EnqueueOutcome::Skipped("channel rejects nsfw".to_string()));
    }
    if post.state == PostState::Rejected {
        return Ok(EnqueueOutcome::Skipped("post is rejected".to_string()));
    }

    let channel_id = channel.id;
    let inserted = state
        .db
        .call(move |conn| crate::db::queue::enqueue(conn, post_id, channel_id, priority))
        .await?;
    if !inserted {
        return Ok(EnqueueOutcome::AlreadyQueued);
    }
    let queue_id = {
        let channel_id = channel.id;
        state
            .db
            .call(move |conn| {
                conn.query_row(
                    "SELECT id FROM queue WHERE post_id = ?1 AND channel_id = ?2",
                    rusqlite::params![post_id, channel_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .await?
    };
    if post.state == PostState::New {
        let post_id = post.id;
        state
            .db
            .call(move |conn| crate::db::posts::set_state(conn, post_id, PostState::Approved))
            .await?;
    }
    Ok(EnqueueOutcome::Queued(queue_id))
}
