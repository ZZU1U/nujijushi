//! Queue mechanics: when to publish, which post, and how to keep it idempotent.

pub mod lease;
pub mod scheduler;
pub mod selection;

pub use scheduler::{is_active, is_due, local_day_start, next_active_start, next_due};
pub use selection::{Candidate, Selection};

use crate::error::Result;
use crate::model::{Channel, PostState};
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

/// Enqueue a post as an operator (web or CLI).
pub async fn enqueue_checked(
    state: &AppState,
    channel: &Channel,
    post_id: i64,
    priority: i64,
) -> Result<EnqueueOutcome> {
    enqueue_as(state, channel, post_id, priority, true).await
}

/// Keep the policy decision, queue row, and implicit approval in one write.
pub async fn enqueue_as(
    state: &AppState,
    channel: &Channel,
    post_id: i64,
    priority: i64,
    admin: bool,
) -> Result<EnqueueOutcome> {
    let channel = channel.clone();
    let reputation = state.cfg.guest_reputation;
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let rules = crate::db::tags::TagRules::load(&tx)?;
            let post = crate::db::posts::get(&tx, post_id)?;
            let decision = crate::nsfw::evaluate(&post, &channel, &rules);
            if let crate::nsfw::Verdict::Reject(reason) = decision.verdict {
                return Ok(EnqueueOutcome::Skipped(reason));
            }
            if post.state == PostState::Rejected {
                return Ok(EnqueueOutcome::Skipped("post is rejected".into()));
            }
            if reputation && !admin && post.state != PostState::Approved {
                return Ok(EnqueueOutcome::Skipped(
                    "an admin must approve the post first".into(),
                ));
            }
            let Some(queue_id) = crate::db::queue::enqueue_new(&tx, post_id, channel.id, priority)?
            else {
                return Ok(EnqueueOutcome::AlreadyQueued);
            };
            if post.state == PostState::New {
                crate::db::reputation::review(
                    &tx,
                    post_id,
                    PostState::Approved,
                    reputation,
                    admin,
                    None,
                )?;
            }
            tx.commit()?;
            Ok(EnqueueOutcome::Queued(queue_id))
        })
        .await
}

/// What a batch enqueue did, per post, so each interface can phrase it its own
/// way (CLI per line, web in one flash, bot for a single item).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct EnqueueSummary {
    pub outcomes: Vec<(i64, EnqueueOutcome)>,
    /// Posts that could not even be looked at (database or transport errors).
    pub failed: Vec<(i64, String)>,
}

impl EnqueueSummary {
    /// `(queued, already, skipped)`.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut queued = 0;
        let mut already = 0;
        let mut skipped = 0;
        for (_, outcome) in &self.outcomes {
            match outcome {
                EnqueueOutcome::Queued(_) => queued += 1,
                EnqueueOutcome::AlreadyQueued => already += 1,
                EnqueueOutcome::Skipped(_) => skipped += 1,
            }
        }
        (queued, already, skipped)
    }

    /// Distinct skip reasons, in the order they first appeared.
    pub fn skip_reasons(&self) -> Vec<String> {
        let mut reasons: Vec<String> = Vec::new();
        for (_, outcome) in &self.outcomes {
            if let EnqueueOutcome::Skipped(reason) = outcome
                && !reasons.contains(reason)
            {
                reasons.push(reason.clone());
            }
        }
        reasons
    }

    /// "3 queued, 1 already in the queue, 2 skipped, 1 failed".
    pub fn describe(&self) -> String {
        let (queued, already, skipped) = self.counts();
        let mut parts: Vec<String> = Vec::new();
        if queued > 0 {
            parts.push(format!("{queued} queued"));
        }
        if already > 0 {
            parts.push(format!("{already} already in the queue"));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped"));
        }
        if !self.failed.is_empty() {
            parts.push(format!("{} failed", self.failed.len()));
        }
        if parts.is_empty() {
            "nothing to do".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Enqueue a batch through the same gate as a single post, collecting failures
/// instead of aborting the batch on the first one.
pub async fn enqueue_many(
    state: &AppState,
    channel: &Channel,
    post_ids: &[i64],
    priority: i64,
) -> EnqueueSummary {
    let mut summary = EnqueueSummary::default();
    for post_id in post_ids {
        match enqueue_checked(state, channel, *post_id, priority).await {
            Ok(outcome) => summary.outcomes.push((*post_id, outcome)),
            Err(err) => {
                tracing::warn!(post = post_id, "enqueue failed: {err}");
                summary.failed.push((*post_id, err.to_string()));
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_describes_every_outcome_once() {
        let summary = EnqueueSummary {
            outcomes: vec![
                (1, EnqueueOutcome::Queued(9)),
                (2, EnqueueOutcome::Queued(10)),
                (3, EnqueueOutcome::AlreadyQueued),
                (4, EnqueueOutcome::Skipped("blocked tag: gore".into())),
                (5, EnqueueOutcome::Skipped("blocked tag: gore".into())),
            ],
            failed: vec![(6, "db is closed".into())],
        };
        assert_eq!(summary.counts(), (2, 1, 2));
        assert_eq!(
            summary.skip_reasons(),
            vec!["blocked tag: gore".to_string()],
            "reasons are de-duplicated"
        );
        assert_eq!(
            summary.describe(),
            "2 queued, 1 already in the queue, 2 skipped, 1 failed"
        );
        assert_eq!(EnqueueSummary::default().describe(), "nothing to do");
    }
}
