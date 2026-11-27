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
const FLUSH_KINDS: &[&str] = &[crate::jobs::KIND_FLUSH_CHANNEL];
const CLEANUP_KINDS: &[&str] = &[crate::jobs::KIND_STORAGE_CLEANUP];
const MEDIA_KINDS: &[&str] = &[
    crate::jobs::KIND_MERGE_MEDIA,
    crate::jobs::KIND_COMPRESS_MEDIA,
];
const TAG_SUGGESTIONS_LOCK: &str = "suggest-tags.lock";
const QUICK_JOBS: i64 = 4;
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(10);

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
        let media = self.claim(MEDIA_KINDS, 1).await?;
        let flushes = self.claim(FLUSH_KINDS, 1).await?;
        let cleanup = self.claim(CLEANUP_KINDS, 1).await?;
        let (quick, imports, media, flushes, cleanup, suggestions) = tokio::join!(
            self.run_claimed(quick),
            self.run_claimed(imports),
            self.run_claimed(media),
            self.run_claimed(flushes),
            self.run_claimed(cleanup),
            self.drain_suggestions(),
        );
        let count = quick + imports + media + flushes + cleanup + suggestions?;
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

    async fn poll_media(&self) {
        loop {
            match self.claim(MEDIA_KINDS, 1).await {
                Ok(jobs) if !jobs.is_empty() => {
                    self.run_claimed(jobs).await;
                    continue;
                }
                Ok(_) => {}
                Err(err) => tracing::error!("media worker: {err}"),
            }
            tokio::select! {
                _ = self.state.media_jobs.notified() => {},
                _ = tokio::time::sleep(POLL_INTERVAL) => {},
            }
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

    async fn poll_watches(&self) {
        loop {
            if let Err(err) = self
                .state
                .db
                .call(|conn| {
                    crate::db::watches::enqueue_due(
                        conn,
                        chrono::Utc::now(),
                        crate::jobs::KIND_WATCH_CHECK,
                    )
                })
                .await
            {
                tracing::warn!("watch scheduling failed: {err}");
            }
            tokio::time::sleep(WATCH_POLL_INTERVAL).await;
        }
    }

    /// Poll each lane independently so slow source checks and CPU indexing
    /// cannot delay claiming a quick publish job.
    pub async fn run<F>(&self, shutdown: F) -> Result<()>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        // A worker started by `serve` or webhook-mode `bot` must repair the
        // previous process too, not only the one started by `run`.
        reconcile_startup(&self.state).await?;
        tokio::select! {
            _ = shutdown => {},
            _ = self.poll_jobs(QUICK_KINDS, QUICK_JOBS, false) => {},
            _ = self.poll_jobs(IMPORT_KINDS, self.import_limit(), true) => {},
            _ = self.poll_jobs(FLUSH_KINDS, 1, false) => {},
            _ = self.poll_jobs(CLEANUP_KINDS, 1, false) => {},
            _ = self.poll_media() => {},
            _ = self.poll_suggestions() => {},
            _ = self.poll_duplicates() => {},
            _ = self.poll_watches() => {},
        }
        Ok(())
    }
}

/// Startup pass for every serving mode: fail abandoned jobs and turn crashed
/// sends into `unknown` (they are never re-sent automatically).
pub async fn reconcile_startup(state: &AppState) -> Result<()> {
    let sends = crate::queue::lease::reconcile_stale(state).await?;
    let jobs = reconcile(state).await?;
    if sends > 0 || jobs > 0 {
        tracing::warn!("reconciled {sends} interrupted send(s) and {jobs} interrupted job(s)");
    }
    Ok(())
}

/// Mark abandoned jobs interrupted. Pending watch checks and flushes can still run.
pub async fn reconcile(state: &AppState) -> Result<usize> {
    let mut fixed = state
        .db
        .call(|conn| {
            job_db::reconcile_except(
                conn,
                &[
                    crate::jobs::KIND_WATCH_CHECK,
                    crate::jobs::KIND_TAG_SUGGESTIONS,
                    crate::jobs::KIND_FLUSH_CHANNEL,
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
    let running = state
        .db
        .call(|conn| job_db::running_of(conn, crate::jobs::KIND_FLUSH_CHANNEL))
        .await?;
    for job in running {
        let channel_id = super::payload_i64(&job)?;
        if let Some(_lock) = super::flush_job_lock(state, channel_id).await? {
            fixed += state
                .db
                .call(move |conn| job_db::interrupt_running(conn, job.id))
                .await?;
        }
    }
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_enqueue_wakes_the_worker_without_waiting_for_disk_poll() {
        let directory = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::load_from_home(directory.path().to_path_buf()).unwrap();
        let state = AppState::open(cfg, None).unwrap();
        let worker = JobWorker::new(state.clone());
        let task = tokio::spawn(async move { worker.poll_media().await });
        // A deliberately invalid request settles as failed as soon as the lane wakes.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let id = state
            .db
            .call(|conn| job_db::create(conn, crate::jobs::KIND_MERGE_MEDIA, "", 2))
            .await
            .unwrap();
        state.media_jobs.notify_one();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let job = state
                    .db
                    .call(move |conn| job_db::get(conn, id))
                    .await
                    .unwrap()
                    .unwrap();
                if job.state == "failed" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn startup_reconciles_a_crashed_send_but_not_a_live_one() {
        use crate::db::{channels, posts, queue};
        use crate::model::{Channel, QueueState};
        use crate::source::NormalizedPost;

        let directory = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::load_from_home(directory.path().to_path_buf()).unwrap();
        let state = AppState::open(cfg, None).unwrap();
        let (stale_id, live_id) = state
            .db
            .call(|conn| {
                let channel_id = channels::insert(conn, &Channel::new("Test", "@test_channel"))?;
                let mut ids = Vec::new();
                for label in ["1", "2"] {
                    let post = NormalizedPost::new("pixiv", label);
                    let (post_id, _) = posts::upsert(conn, &post, &[])?;
                    queue::enqueue(conn, post_id, channel_id, 0)?;
                    ids.push(queue::for_post(conn, post_id)?[0].0);
                }
                assert!(queue::acquire(conn, ids[0], "dead")?);
                assert!(queue::acquire(conn, ids[1], "live")?);
                conn.execute(
                    "UPDATE queue SET lease_at = ?1 WHERE id = ?2",
                    rusqlite::params![
                        crate::db::fmt_ts(chrono::Utc::now() - chrono::Duration::hours(1)),
                        ids[0]
                    ],
                )?;
                Ok((ids[0], ids[1]))
            })
            .await
            .unwrap();

        // This is what every serving mode now runs at startup.
        reconcile_startup(&state).await.unwrap();

        let (stale, live) = state
            .db
            .call(move |conn| {
                Ok((
                    queue::get(conn, stale_id)?.state,
                    queue::get(conn, live_id)?.state,
                ))
            })
            .await
            .unwrap();
        assert_eq!(stale, QueueState::Unknown);
        assert_eq!(live, QueueState::Sending, "a live lease is not interrupted");
    }
}
