//! The lease protocol that keeps publishing idempotent across restarts.

use chrono::{Duration, Utc};

use crate::db::queue as queue_db;
use crate::error::Result;
use crate::state::AppState;

/// A `sending` row older than this is treated as crashed.
pub const STALE_LEASE_SECS: i64 = 600;

pub fn new_token() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Take the lease for a queue item. `None` means another worker won the race.
pub async fn acquire(state: &AppState, queue_id: i64) -> Result<Option<String>> {
    let token = new_token();
    let claimed = state
        .db
        .call({
            let token = token.clone();
            move |conn| queue_db::acquire(conn, queue_id, &token)
        })
        .await?;
    Ok(claimed.then_some(token))
}

/// Move crashed `sending` rows to `unknown` - they are never re-sent silently.
pub async fn reconcile_stale(state: &AppState) -> Result<usize> {
    let cutoff = Utc::now() - Duration::seconds(STALE_LEASE_SECS);
    state
        .db
        .call(move |conn| queue_db::reconcile_stale(conn, cutoff))
        .await
}

pub async fn release(state: &AppState, queue_id: i64) -> Result<()> {
    state
        .db
        .call(move |conn| queue_db::release(conn, queue_id))
        .await
}
