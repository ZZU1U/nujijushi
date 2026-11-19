//! Background import worker used by `nujijushi run` and the web UI.

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::db::jobs as job_db;
use crate::error::Result;
use crate::state::AppState;

/// Publish/upload jobs are quick and the user is watching them, so they get
/// their own claim instead of a slot of the (slow) import pool: with two long
/// gallery-dl runs, a queued "publish now" would otherwise wait for both.
const QUICK_KINDS: &[&str] = &[
    crate::jobs::KIND_PUBLISH_CHANNEL,
    crate::jobs::KIND_PUBLISH_ITEM,
    crate::jobs::KIND_STORAGE_DRAIN,
];
const QUICK_JOBS: i64 = 4;

pub struct JobWorker {
    state: AppState,
    limits: Arc<Semaphore>,
}

impl JobWorker {
    pub fn new(state: AppState) -> Self {
        let permits = state.cfg.max_concurrent_imports;
        Self {
            state,
            limits: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Claim and run every pending job, keeping imports within the limit.
    pub async fn drain(&self) -> Result<usize> {
        let free = self.limits.available_permits() as i64;
        let jobs = self
            .state
            .db
            .call(move |conn| {
                let mut claimed = job_db::claim_pending_of(conn, QUICK_KINDS, QUICK_JOBS)?;
                claimed.extend(job_db::claim_pending_of(
                    conn,
                    &[crate::jobs::KIND_IMPORT],
                    free,
                )?);
                Ok(claimed)
            })
            .await?;
        if jobs.is_empty() {
            // nothing to run, but storage mirrors may still be waiting
            if let Err(err) = crate::storage::drain(&self.state, 5).await {
                tracing::warn!("storage drain failed: {err}");
            }
            return Ok(0);
        }

        let mut set: JoinSet<()> = JoinSet::new();
        for job in &jobs {
            let state = self.state.clone();
            let job = job.clone();
            set.spawn(async move {
                if let Err(err) = crate::jobs::run_job(&state, &job).await {
                    tracing::warn!(job = job.id, "job failed: {err}");
                }
            });
        }
        while set.join_next().await.is_some() {}
        if let Err(err) = crate::storage::drain(&self.state, 10).await {
            tracing::warn!("storage drain failed: {err}");
        }
        Ok(jobs.len())
    }

    /// Poll loop; exits when the shutdown future resolves.
    pub async fn run<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                result = self.drain() => {
                    if let Err(err) = result {
                        tracing::error!("job worker: {err}");
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        Ok(())
    }
}

/// Fail jobs that a previous process left `pending`/`running`.
pub async fn reconcile(state: &AppState) -> Result<usize> {
    state.db.call(|conn| job_db::reconcile(conn)).await
}
