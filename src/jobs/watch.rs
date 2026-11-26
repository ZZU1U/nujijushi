//! Execute watched-source checks through the archive import flow.

use std::fs::File;

use chrono::Utc;

use crate::db::{jobs, posts, watches};
use crate::error::{Error, Result};
use crate::source::{self, watch::Feed};
use crate::state::AppState;

use super::{JobReport, enqueue_import_mirrors, store_post};

#[cfg(test)]
#[path = "watch/pixiv_tests.rs"]
mod pixiv_tests;

pub async fn lock(state: &AppState, watch_id: i64) -> Result<Option<File>> {
    state
        .try_staging_lock(&format!("watch-{watch_id}.lock"))
        .await
}

pub async fn check(state: &AppState, watch_id: i64, job_id: i64) -> Result<JobReport> {
    let started = Utc::now();
    let watch = state
        .db
        .call(move |conn| watches::start_check(conn, watch_id, started))
        .await?;
    let feed = Feed::parse(&watch.url)?;
    let native_creator = feed.native_pixiv_creator(&state.cfg);
    let native_pixiv = native_creator.is_some();
    let initial = usize::from(watch.initial_batch);
    let limit = if watch.checkpoint.is_none() {
        i64::from(watch.initial_batch)
    } else {
        25
    };
    let mut errors = Vec::new();
    if let Some(creator) = native_creator {
        let seen = if watch.pixiv_baselined {
            Some(
                state
                    .db
                    .call(move |conn| watches::pixiv_seen(conn, watch_id))
                    .await?,
            )
        } else {
            None
        };
        match source::pixiv::discover_watch(
            &state.cfg,
            creator,
            initial,
            watch.checkpoint,
            seen.as_ref(),
        )
        .await
        {
            Ok(scan) => {
                state
                    .db
                    .call(move |conn| {
                        watches::record_pixiv_scan(conn, &watch, &scan, seen.as_ref(), started)
                    })
                    .await?
            }
            Err(err) => errors.push(err.to_string()),
        }
    } else {
        match feed
            .discover_gallery_dl(&state.cfg, initial, watch.checkpoint)
            .await
        {
            Ok(items) => {
                state
                    .db
                    .call(move |conn| watches::record_scan(conn, &watch, &items, started))
                    .await?
            }
            Err(err) => errors.push(err.to_string()),
        }
    }

    let pending = state
        .db
        .call(move |conn| watches::pending(conn, watch_id, limit, native_pixiv))
        .await?;
    let total = pending.len() as i64;
    let mut added = 0;
    let mut skipped = 0;
    let mut failed = 0;
    state
        .db
        .call(move |conn| jobs::progress(conn, job_id, 0, total))
        .await?;
    for (index, (id, url)) in pending.into_iter().enumerate() {
        let outcome = import_one(state, feed.source(), &id, &url).await;
        let handled = outcome.is_ok();
        match outcome {
            Ok(true) => added += 1,
            Ok(false) => skipped += 1,
            Err(err) => {
                failed += 1;
                if errors.len() < 3 {
                    errors.push(format!("post {id}: {err}"));
                }
            }
        }
        state
            .db
            .call(move |conn| {
                crate::db::transaction(conn, |conn| {
                    watches::attempted(conn, watch_id, &id, job_id, handled)?;
                    jobs::progress(conn, job_id, index as i64 + 1, total)
                })
            })
            .await?;
    }

    let (remaining, deferred) = state
        .db
        .call(move |conn| {
            let deferred = if native_pixiv {
                watches::pixiv_deferred_count(conn, watch_id)?
            } else {
                0
            };
            Ok((watches::pending_count(conn, watch_id)? - deferred, deferred))
        })
        .await?;
    let mut summary = format!(
        "{added} added · {skipped} skipped (already archived) · {failed} failed · {remaining} pending"
    );
    if deferred > 0 {
        summary.push_str(&format!(" · {deferred} deferred"));
    }
    if errors.is_empty() {
        Ok(JobReport::Batch {
            message: summary,
            attempted: total,
        })
    } else {
        Err(Error::other(format!("{summary}. {}", errors.join(" · "))))
    }
}

async fn import_one(state: &AppState, source: &'static str, id: &str, url: &str) -> Result<bool> {
    let source_id = id.to_string();
    if state
        .db
        .call(move |conn| posts::find_by_source_id(conn, source, &source_id))
        .await?
        .is_some()
    {
        return Ok(false);
    }

    let downloaded = source::fetch_watched(&state.cfg, url).await?;
    let result = match downloaded.as_slice() {
        [post] if post.source == source && post.source_id == id && !post.files.is_empty() => {
            store_post(state, post, None, true).await
        }
        _ => Err(Error::other("download did not return the expected post")),
    };
    source::cleanup_staging(&state.cfg, &downloaded);
    let stored = result?;
    if stored.created
        && let Some(post_id) = stored.post_id
    {
        enqueue_import_mirrors(state, &[post_id]).await;
    }
    Ok(stored.created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn reconciliation_respects_a_live_watch_and_preserves_pending_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        let state = AppState::open(cfg.clone(), None).unwrap();
        let other = AppState::open(cfg, None).unwrap();
        state
            .db
            .with(|conn| {
                watches::create(conn, "One", "https://www.pixiv.net/users/1/artworks", 10)?;
                watches::create(conn, "Two", "https://www.pixiv.net/users/2/artworks", 10)?;
                Ok(())
            })
            .unwrap();
        let (first, second) = tokio::join!(
            crate::jobs::create_watch_check_job(&state, 1),
            crate::jobs::create_watch_check_job(&other, 1),
        );
        let (id, created) = first.unwrap();
        let (other_id, other_created) = second.unwrap();
        assert_eq!(id, other_id);
        assert_ne!(created, other_created);

        let (pending, _) = crate::jobs::create_watch_check_job(&state, 2)
            .await
            .unwrap();
        state
            .db
            .with(|conn| jobs::set_state(conn, id, "running", None))
            .unwrap();
        let held = lock(&state, 1).await.unwrap().unwrap();
        assert_eq!(crate::jobs::worker::reconcile(&other).await.unwrap(), 0);
        drop(held);
        assert_eq!(crate::jobs::worker::reconcile(&other).await.unwrap(), 1);
        state
            .db
            .with(|conn| {
                assert_eq!(jobs::get(conn, id)?.unwrap().state, "failed");
                assert_eq!(jobs::get(conn, pending)?.unwrap().state, "pending");
                Ok(())
            })
            .unwrap();
        assert!(
            crate::jobs::create_watch_check_job(&other, 1)
                .await
                .unwrap()
                .1
        );
    }
}
