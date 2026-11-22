//! Admin pages for watched feeds and duplicate review.

use std::sync::Arc;

use axum::{
    Form, Json,
    extract::{Path, Query, State},
    response::Response,
};
use serde::Deserialize;

use crate::db::{self, duplicates, watches};
use crate::error::{Error, Result};
use crate::jobs;
use crate::source::watch::Feed;

use super::admin::action_result;
use super::{
    WebState,
    data::{
        DuplicateImage, DuplicateMatch, DuplicatePair, DuplicatePost, DuplicateQueue,
        DuplicatesPage, WatchView, WatchesPage,
    },
    forms::PostFilter,
    routes::{WebResult, check_csrf, job_view},
};

pub async fn watches_page(State(ws): State<Arc<WebState>>) -> WebResult<Json<WatchesPage>> {
    let now = chrono::Utc::now();
    let watches = ws
        .app
        .db
        .call(move |conn| {
            watches::list(conn)?
                .into_iter()
                .map(|watch| {
                    let job = watch
                        .last_job_id
                        .map(|id| db::jobs::get(conn, id))
                        .transpose()?
                        .flatten();
                    Ok(WatchView {
                        id: watch.id,
                        name: watch.name,
                        url: watch.url,
                        initial_batch: watch.initial_batch,
                        enabled: watch.enabled,
                        job: job.as_ref().map(|job| job_view(job, now)),
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(WatchesPage { watches }))
}

#[derive(Deserialize)]
pub struct WatchForm {
    #[serde(default)]
    csrf: String,
    name: String,
    url: String,
    #[serde(default, deserialize_with = "super::forms::empty_as_none")]
    initial_batch: Option<u16>,
}

pub async fn create_watch(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<WatchForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let name = form.name.trim().to_string();
    let initial_batch = form.initial_batch.unwrap_or(10);
    if name.is_empty() || name.chars().count() > 100 || !(1..=100).contains(&initial_batch) {
        return Err(Error::invalid(
            "Enter a name of 1–100 characters and a batch size from 1 to 100",
        )
        .into());
    }
    let feed = Feed::parse(&form.url)?;
    let created = ws
        .app
        .db
        .call(move |conn| watches::create(conn, &name, &feed.url, initial_batch))
        .await?;
    Ok(action_result(
        "/watches",
        if created {
            "Watch added"
        } else {
            "This feed is already watched"
        },
    ))
}

#[derive(Deserialize)]
pub struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

pub async fn check_watch(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<CsrfForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let (job, queued) = jobs::create_watch_check_job(&ws.app, id).await?;
    let message = if queued {
        "Watch check queued"
    } else {
        "A check is already active"
    };
    Ok(super::admin::accepted(
        "/watches",
        message,
        queued.then_some(job).into_iter().collect(),
    ))
}

#[derive(Deserialize)]
pub struct WatchToggle {
    #[serde(default)]
    csrf: String,
    action: WatchAction,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WatchAction {
    Pause,
    Resume,
}

pub async fn toggle_watch(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<WatchToggle>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let enabled = matches!(form.action, WatchAction::Resume);
    ws.app
        .db
        .call(move |conn| watches::set_enabled(conn, id, enabled))
        .await?;
    Ok(action_result(
        "/watches",
        if enabled {
            "Watch resumed"
        } else {
            "Watch paused"
        },
    ))
}

pub async fn remove_watch(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<CsrfForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let removed = ws
        .app
        .db
        .call(move |conn| watches::remove(conn, id, jobs::KIND_WATCH_CHECK))
        .await?;
    Ok(action_result(
        "/watches",
        if removed {
            "Watch removed"
        } else {
            "Watch missing or a check is active"
        },
    ))
}

pub async fn duplicates_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Json<DuplicatesPage>> {
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1).saturating_mul(duplicates::PAGE_SIZE);
    let blobs = ws.app.blobs.clone();
    let (pairs, has_next, progress) = ws
        .app
        .db
        .call(move |conn| {
            let snapshot = conn.transaction()?;
            let keys = duplicates::pairs(&snapshot, offset)?;
            let has_next = keys.len() > duplicates::PAGE_SIZE as usize;
            let channels = db::channels::list(&snapshot)?;
            let mut pairs = Vec::new();
            for (left, right) in keys.into_iter().take(duplicates::PAGE_SIZE as usize) {
                let evidence: Vec<_> = duplicates::evidence(&snapshot, left, right)?
                    .into_iter()
                    .map(|item| DuplicateMatch {
                        left: DuplicateImage {
                            thumb: blobs.thumb_exists(&item.left_hash),
                            hash: item.left_hash,
                            position: item.left_position,
                        },
                        right: DuplicateImage {
                            thumb: blobs.thumb_exists(&item.right_hash),
                            hash: item.right_hash,
                            position: item.right_position,
                        },
                        exact: item.exact,
                    })
                    .collect();
                pairs.push(DuplicatePair {
                    left: post_side(&snapshot, left, &channels)?,
                    right: post_side(&snapshot, right, &channels)?,
                    matches: evidence,
                });
            }
            Ok((pairs, has_next, duplicates::progress(&snapshot)?))
        })
        .await?;
    Ok(Json(DuplicatesPage {
        pairs,
        previous: (page > 1).then_some(page - 1),
        next: has_next.then(|| page.saturating_add(1)),
        indexing: progress.pending > 0,
        pending: progress.pending,
        failed: progress.failed,
    }))
}

#[derive(Deserialize)]
pub struct RetryDuplicatesForm {
    #[serde(default)]
    csrf: String,
}

pub async fn retry_duplicates(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<RetryDuplicatesForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let scheduled = ws
        .app
        .db
        .call(|conn| duplicates::retry_failed(conn))
        .await?;
    Ok(action_result(
        "/duplicates",
        if scheduled > 0 {
            "Failed visual checks queued for retry"
        } else {
            "No visual checks waiting to retry"
        },
    ))
}

fn post_side(
    conn: &rusqlite::Connection,
    id: i64,
    channels: &[crate::model::Channel],
) -> Result<DuplicatePost> {
    let post = db::posts::get(conn, id)?;
    let url = post.url.as_deref().filter(|value| {
        url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
    });
    let statuses: Vec<_> = db::queue::for_post(conn, id)?
        .into_iter()
        .map(|(_, channel_id, state)| {
            let alias = channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .map(|channel| channel.alias.clone());
            DuplicateQueue {
                alias,
                state: state.as_str(),
                channel_id,
            }
        })
        .collect();
    Ok(DuplicatePost {
        id,
        artist: post.author_label(),
        title: post.title,
        source: post.source,
        url: url.map(str::to_owned),
        state: post.state.as_str(),
        queues: statuses,
    })
}

#[derive(Deserialize)]
pub struct DismissForm {
    #[serde(default)]
    csrf: String,
    left: i64,
    right: i64,
}

pub async fn dismiss(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<DismissForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let dismissed = ws
        .app
        .db
        .call(move |conn| duplicates::dismiss(conn, form.left, form.right))
        .await?;
    Ok(action_result(
        "/duplicates",
        if dismissed {
            "Pair dismissed"
        } else {
            "Pair already dismissed or unavailable"
        },
    ))
}
