//! Background import worker used by `nujijushi run` and the web UI.

use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::db::jobs as job_db;
use crate::error::Result;
use crate::state::AppState;

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

    /// Claim and run every pending job, respecting the concurrency limit.
    pub async fn drain(&self) -> Result<usize> {
        let free = self.limits.available_permits() as i64;
        if free <= 0 {
            return Ok(0);
        }
        let jobs = self
            .state
            .db
            .call(move |conn| job_db::claim_pending(conn, free))
            .await?;
        if jobs.is_empty() {
            // no imports to run, but storage mirrors may still be waiting
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
                    tracing::warn!(job = job.id, "import job failed: {err}");
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
