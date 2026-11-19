//! Background work used by `nujijushi run` and the web UI.

use std::time::Duration;

use tokio::task::JoinSet;

use crate::db::jobs as job_db;
use crate::error::Result;
use crate::state::AppState;

const QUICK_KINDS: &[&str] = &[
    crate::jobs::KIND_PUBLISH_CHANNEL,
    crate::jobs::KIND_PUBLISH_ITEM,
    crate::jobs::KIND_STORAGE_DRAIN,
];
const IMPORT_KINDS: &[&str] = &[crate::jobs::KIND_IMPORT, crate::jobs::KIND_WATCH_CHECK];
const TAG_SUGGESTIONS_LOCK: &str = "suggest-tags.lock";
const QUICK_JOBS: i64 = 4;
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct JobWorker {
    state: AppState,
}

impl JobWorker {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    async fn claim(&self, kinds: &'static [&'static str], limit: i64) -> Result<Vec<job_db::Job>> {
        self.state
            .db
            .call(move |conn| job_db::claim_pending_of(conn, kinds, limit))
            .await
    }

    async fn run_claimed(&self, jobs: Vec<job_db::Job>) -> usize {
        let count = jobs.len();
        let mut set = JoinSet::new();
        for job in jobs {
            let state = self.state.clone();
            set.spawn(async move {
                if let Err(err) = crate::jobs::run_job(&state, &job).await {
                    tracing::warn!(job = job.id, "job failed: {err}");
                }
            });
        }
        while let Some(result) = set.join_next().await {
            if let Err(err) = result {
                tracing::warn!("job task stopped: {err}");
            }
        }
        count
    }

    fn import_limit(&self) -> i64 {
        self.state.cfg.max_concurrent_imports as i64
    }

    async fn drain_mirrors(&self, limit: i64) {
        if let Err(err) = crate::storage::drain(&self.state, limit).await {
            tracing::warn!("storage drain failed: {err}");
        }
    }

    /// Run one pass of the worker, useful to callers that own the polling loop.
    pub async fn drain(&self) -> Result<usize> {
        let quick = self.claim(QUICK_KINDS, QUICK_JOBS).await?;
        let imports = self.claim(IMPORT_KINDS, self.import_limit()).await?;
        let (quick, imports, suggestions) = tokio::join!(
            self.run_claimed(quick),
            self.run_claimed(imports),
            self.drain_suggestions(),
        );
        let count = quick + imports + suggestions?;
        self.drain_mirrors(if count == 0 { 5 } else { 10 }).await;
        crate::duplicates::index_batch(&self.state).await?;
        Ok(count)
    }

    async fn drain_suggestions(&self) -> Result<usize> {
        if !self.state.cfg.tag_suggestions.enabled {
            return Ok(0);
        }
        let Some(_lock) = self.state.try_staging_lock(TAG_SUGGESTIONS_LOCK).await? else {
            return Ok(0);
        };
        let jobs = self
            .state
            .db
            .call(|conn| {
                let kind = crate::jobs::KIND_TAG_SUGGESTIONS;
                // Owning the archive lock means no live worker owns a running job.
                job_db::interrupt_kind(conn, kind)?;
                job_db::claim_pending_of(conn, &[kind], 1)
            })
            .await?;
        Ok(self.run_claimed(jobs).await)
    }

    async fn poll_suggestions(&self) {
        if !self.state.cfg.tag_suggestions.enabled {
            std::future::pending::<()>().await;
        }
        loop {
            if let Err(err) = self.drain_suggestions().await {
                tracing::warn!("tag suggestion worker: {err}");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn poll_jobs(&self, kinds: &'static [&'static str], limit: i64, mirrors: bool) {
        loop {
            match self.claim(kinds, limit).await {
                Ok(jobs) => {
                    let count = self.run_claimed(jobs).await;
                    if mirrors {
                        self.drain_mirrors(if count == 0 { 5 } else { 10 }).await;
                    }
                }
                Err(err) => tracing::error!("job worker: {err}"),
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn poll_duplicates(&self) {
        loop {
            if let Err(err) = crate::duplicates::index_batch(&self.state).await {
                tracing::warn!("duplicate indexing failed: {err}");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Poll each lane independently so slow source checks and CPU indexing
    /// cannot delay claiming a quick publish job.
    pub async fn run<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        tokio::select! {
            _ = shutdown => {},
            _ = self.poll_jobs(QUICK_KINDS, QUICK_JOBS, false) => {},
            _ = self.poll_jobs(IMPORT_KINDS, self.import_limit(), true) => {},
            _ = self.poll_suggestions() => {},
            _ = self.poll_duplicates() => {},
        }
        Ok(())
    }
}

/// Mark abandoned jobs interrupted. Pending watch checks can still run.
/// A running watch in another process holds its lock until it records the result.
pub async fn reconcile(state: &AppState) -> Result<usize> {
    let mut fixed = state
        .db
        .call(|conn| {
            job_db::reconcile_except(
                conn,
                &[
                    crate::jobs::KIND_WATCH_CHECK,
                    crate::jobs::KIND_TAG_SUGGESTIONS,
                ],
            )
        })
        .await?;
    if let Some(_lock) = state.try_staging_lock(TAG_SUGGESTIONS_LOCK).await? {
        fixed += state
            .db
            .call(|conn| job_db::interrupt_kind(conn, crate::jobs::KIND_TAG_SUGGESTIONS))
            .await?;
    }
    let running = state
        .db
        .call(|conn| job_db::running_of(conn, crate::jobs::KIND_WATCH_CHECK))
        .await?;
    for job in running {
        let watch_id = super::payload_i64(&job)?;
        if let Some(_lock) = super::watch::lock(state, watch_id).await? {
            fixed += state
                .db
                .call(move |conn| job_db::interrupt_running(conn, job.id))
                .await?;
        }
    }
    Ok(fixed)
}
