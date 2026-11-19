//! Import orchestration: source -> blob store -> database, plus job bookkeeping.

pub mod worker;

use std::path::{Path, PathBuf};

use crate::blob::Ingested;
use crate::db::posts::NewMedia;
use crate::db::{jobs as job_db, posts as post_db, tags as tag_db};
use crate::error::{Error, Result};
use crate::model::PostState;
use crate::source::{NormalizedPost, gallerydl, telegram};
use crate::state::AppState;

#[derive(Debug, Clone, Default)]
pub struct ImportOutcome {
    pub post_ids: Vec<i64>,
    pub created: usize,
    pub updated: usize,
    pub files: usize,
    pub skipped: Vec<(String, String)>,
}

impl ImportOutcome {
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} post(s): {} new, {} updated, {} file(s)",
            self.post_ids.len(),
            self.created,
            self.updated,
            self.files
        );
        if !self.skipped.is_empty() {
            out.push_str(&format!(", {} skipped", self.skipped.len()));
        }
        out
    }
}

/// Where a bot import can stage its temporary copies: the submitter's own chat.
pub fn copy_chat_for(state: &AppState, submitted_by: Option<i64>) -> Option<String> {
    submitted_by
        .map(|id| id.to_string())
        .or_else(|| state.cfg.access.admins.first().map(|id| id.to_string()))
}

/// Import one target (URL, t.me link or directory) into the archive.
pub async fn import_target(
    state: &AppState,
    target: &str,
    submitted_by: Option<i64>,
) -> Result<ImportOutcome> {
    import_target_tracked(state, target, submitted_by, None).await
}

/// Same, but reports per-post progress into `job_id` (the worker's path, so the
/// UI's progress bar moves during a batch instead of jumping 0 -> done).
pub async fn import_target_tracked(
    state: &AppState,
    target: &str,
    submitted_by: Option<i64>,
    job_id: Option<i64>,
) -> Result<ImportOutcome> {
    let copy_chat = copy_chat_for(state, submitted_by);
    let posts = crate::source::fetch_target(state, target, copy_chat.as_deref()).await?;
    let outcome = import_posts(state, posts.clone(), submitted_by, job_id).await?;

    if target.starts_with("http") && !telegram::is_telegram_link(target) {
        gallerydl::cleanup_staging(&posts);
    }
    telegram::cleanup(
        &posts
            .iter()
            .flat_map(|post| post.files.clone())
            .collect::<Vec<_>>(),
    );

    if outcome.post_ids.is_empty() {
        let detail = outcome
            .skipped
            .first()
            .map(|(_, err)| err.clone())
            .unwrap_or_else(|| "no posts returned".to_string());
        return Err(Error::other(format!("import of {target} failed: {detail}")));
    }
    Ok(outcome)
}

/// Store already-fetched posts (used by imports and by forwarded Telegram posts).
pub async fn import_posts(
    state: &AppState,
    posts: Vec<NormalizedPost>,
    submitted_by: Option<i64>,
    job_id: Option<i64>,
) -> Result<ImportOutcome> {
    let mut outcome = ImportOutcome::default();
    let total = posts.len() as i64;
    for (index, post) in posts.iter().enumerate() {
        match store_post(state, post, submitted_by).await {
            Ok((post_id, created)) => {
                outcome.post_ids.push(post_id);
                outcome.files += post.files.len();
                if created {
                    outcome.created += 1;
                } else {
                    outcome.updated += 1;
                }
            }
            Err(err) => outcome
                .skipped
                .push((post.source_id.clone(), err.to_string())),
        }
        // move the UI's progress bar while a batch is still running
        if let Some(id) = job_id {
            let done = index as i64 + 1;
            if let Err(err) = state
                .db
                .call(move |conn| job_db::progress(conn, id, done, total))
                .await
            {
                tracing::debug!("cannot record import progress: {err}");
            }
        }
    }

    // mirror to the configured storage sinks; the worker uploads them
    if state.cfg.storage.on_import
        && state.cfg.storage.any_enabled()
        && !outcome.post_ids.is_empty()
    {
        match crate::storage::enqueue(state, &outcome.post_ids).await {
            Ok(queued) if queued > 0 => tracing::info!(queued, "queued for external storage"),
            Ok(_) => {}
            Err(err) => tracing::warn!("cannot queue storage mirrors: {err}"),
        }
    }
    Ok(outcome)
}

async fn store_post(
    state: &AppState,
    post: &NormalizedPost,
    submitted_by: Option<i64>,
) -> Result<(i64, bool)> {
    let mut media: Vec<NewMedia> = Vec::new();
    for file in &post.files {
        let (ingested, thumb) = ingest(state, &file.path).await?;
        state
            .db
            .call({
                let store = state.blobs.clone();
                let ingested = ingested.clone();
                move |conn| store.record(conn, &ingested)
            })
            .await?;

        let poster_hash = if ingested.is_video {
            poster_for(state, &file.path, &ingested.hash).await?
        } else {
            thumb.map(|_| ingested.hash.clone())
        };

        media.push(NewMedia {
            blob_hash: ingested.hash.clone(),
            ext: ingested.ext.clone(),
            mime: ingested.mime(),
            width: ingested.width,
            height: ingested.height,
            bytes: ingested.bytes,
            is_video: ingested.is_video,
            poster_hash: if ingested.is_video { poster_hash } else { None },
            sensitive: file.sensitive,
            meta_json: file.meta.clone(),
        });
    }

    let post = post.clone();
    let media_for_db = media.clone();
    let (post_id, created) = state
        .db
        .call(move |conn| post_db::upsert(conn, &post, &media_for_db))
        .await?;

    if let Some(tg_id) = submitted_by {
        state
            .db
            .call(move |conn| post_db::set_submitted_by(conn, post_id, tg_id))
            .await?;
    }

    // hard blocks are applied at import time so they never reach a queue
    let rules = state.db.call(|conn| tag_db::TagRules::load(conn)).await?;
    let tags = state
        .db
        .call(move |conn| tag_db::for_post(conn, post_id))
        .await?;
    if let Some(hit) = rules.blocked_hit(&tags).cloned() {
        let note = format!("auto-rejected: blocked tag {hit}");
        state
            .db
            .call(move |conn| {
                post_db::set_state(conn, post_id, PostState::Rejected)?;
                post_db::set_notes(conn, post_id, Some(&note))
            })
            .await?;
    }

    Ok((post_id, created))
}

async fn ingest(state: &AppState, path: &Path) -> Result<(Ingested, Option<PathBuf>)> {
    let store = state.blobs.clone();
    let path = path.to_path_buf();
    let max_bytes = state.cfg.max_upload_bytes;
    tokio::task::spawn_blocking(move || {
        let ingested = store.ingest_file(&path, None)?;
        if ingested.bytes as u64 > max_bytes {
            tracing::warn!(
                file = %path.display(),
                "file exceeds the bot upload limit and will not be published by the Bot API"
            );
        }
        let thumb = if ingested.is_video {
            None
        } else {
            let bytes = std::fs::read(&path)?;
            store.write_thumb(&ingested.hash, &bytes, 512)?
        };
        Ok::<(Ingested, Option<PathBuf>), Error>((ingested, thumb))
    })
    .await?
}

async fn poster_for(state: &AppState, video: &Path, hash: &str) -> Result<Option<String>> {
    let out = state.cfg.staging_dir().join(format!("poster-{hash}.jpg"));
    if !crate::media::video::extract_poster(&state.cfg, video, &out).await? {
        return Ok(None);
    }
    let (ingested, _) = ingest(state, &out).await?;
    state
        .db
        .call({
            let store = state.blobs.clone();
            let ingested = ingested.clone();
            move |conn| store.record(conn, &ingested)
        })
        .await?;
    let _ = std::fs::remove_file(&out);
    Ok(Some(ingested.hash))
}

/// Job kinds. The UI queues one row per action and the worker dispatches on this
/// string, so the worker never learns about HTTP and no request waits on a
/// Telegram upload.
pub const KIND_IMPORT: &str = "import";
pub const KIND_PUBLISH_CHANNEL: &str = "publish_channel";
pub const KIND_PUBLISH_ITEM: &str = "publish_item";
pub const KIND_STORAGE_DRAIN: &str = "storage_drain";

/// Create the job row for a target (CLI / web UI).
pub async fn create_job(state: &AppState, target: &str) -> Result<i64> {
    create_job_for(state, target, None).await
}

/// Create a job row on behalf of a Telegram user (the bot's path).
pub async fn create_job_for(
    state: &AppState,
    target: &str,
    submitted_by: Option<i64>,
) -> Result<i64> {
    let target = target.to_string();
    state
        .db
        .call(move |conn| job_db::create_for(conn, KIND_IMPORT, &target, 0, submitted_by))
        .await
}

/// Queue "publish the next post of this channel", unless one is already queued.
pub async fn create_publish_channel_job(state: &AppState, channel_id: i64) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_PUBLISH_CHANNEL, channel_id.to_string()).await
}

/// Queue "publish exactly this queue item", unless one is already queued.
pub async fn create_publish_item_job(state: &AppState, queue_id: i64) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_PUBLISH_ITEM, queue_id.to_string()).await
}

/// Queue a storage drain (uploads whatever mirrors are waiting).
pub async fn create_drain_job(state: &AppState, limit: usize) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_STORAGE_DRAIN, limit.to_string()).await
}

/// Create a job unless the same kind + payload is already pending or running.
async fn enqueue_unique(
    state: &AppState,
    kind: &'static str,
    payload: String,
) -> Result<Option<i64>> {
    state
        .db
        .call(move |conn| {
            if job_db::find_active(conn, kind, &payload)?.is_some() {
                return Ok(None);
            }
            Ok(Some(job_db::create_for(conn, kind, &payload, 0, None)?))
        })
        .await
}

/// What a finished job reports back.
#[derive(Debug, Clone)]
pub enum JobReport {
    Import(ImportOutcome),
    /// Any other kind: one line for the job row, plus the post it touched.
    Done {
        message: String,
        post_id: Option<i64>,
    },
}

impl JobReport {
    pub fn message(&self) -> String {
        match self {
            JobReport::Import(outcome) => outcome.summary(),
            JobReport::Done { message, .. } => message.clone(),
        }
    }

    pub fn post_ids(&self) -> &[i64] {
        match self {
            JobReport::Import(outcome) => &outcome.post_ids,
            JobReport::Done { .. } => &[],
        }
    }
}

/// Run a job that is already marked `running`, then record how it went.
pub async fn run_job(state: &AppState, job: &job_db::Job) -> Result<JobReport> {
    let label = match job.kind.as_str() {
        KIND_IMPORT => "importing",
        KIND_PUBLISH_CHANNEL | KIND_PUBLISH_ITEM => "publishing",
        KIND_STORAGE_DRAIN => "uploading mirrors",
        other => return Err(Error::invalid(format!("unknown job kind: {other}"))),
    };
    state
        .db
        .call({
            let id = job.id;
            move |conn| job_db::set_state(conn, id, "running", Some(label))
        })
        .await?;

    let result = run_kind(state, job).await;
    match &result {
        Ok(report) => {
            let message = report.message();
            let (total, post_id) = match report {
                JobReport::Import(outcome) => (
                    outcome.post_ids.len() as i64,
                    outcome.post_ids.first().copied(),
                ),
                JobReport::Done { post_id, .. } => (1, *post_id),
            };
            state
                .db
                .call({
                    let id = job.id;
                    let message = message.clone();
                    move |conn| {
                        job_db::progress(conn, id, total, total)?;
                        if let Some(post_id) = post_id {
                            job_db::attach_post(conn, id, post_id)?;
                        }
                        job_db::set_state(conn, id, "done", Some(&message))
                    }
                })
                .await?;
            notify_submitter(state, job, true, &message).await;
        }
        Err(err) => {
            let message = err.to_string();
            state
                .db
                .call({
                    let id = job.id;
                    let message = message.clone();
                    move |conn| job_db::set_state(conn, id, "failed", Some(&message))
                })
                .await?;
            notify_submitter(state, job, false, &message).await;
        }
    }
    result
}

/// Dispatch one job kind; `run_job` owns the bookkeeping around it.
async fn run_kind(state: &AppState, job: &job_db::Job) -> Result<JobReport> {
    match job.kind.as_str() {
        KIND_IMPORT => {
            let target = job.payload.clone().unwrap_or_default();
            let outcome =
                import_target_tracked(state, &target, job.submitted_by, Some(job.id)).await?;
            Ok(JobReport::Import(outcome))
        }
        KIND_PUBLISH_CHANNEL => {
            let channel_id = payload_i64(job)?;
            let channel = state
                .db
                .call(move |conn| crate::db::channels::get(conn, channel_id))
                .await?;
            let outcome = crate::tg::publish::publish_channel(state, &channel, false, true).await?;
            Ok(JobReport::Done {
                message: outcome.describe(),
                post_id: published_post(&outcome),
            })
        }
        KIND_PUBLISH_ITEM => {
            let queue_id = payload_i64(job)?;
            let outcome = crate::tg::publish::publish_item(state, queue_id, false).await?;
            Ok(JobReport::Done {
                message: outcome.describe(),
                post_id: published_post(&outcome),
            })
        }
        KIND_STORAGE_DRAIN => {
            let limit = job
                .payload
                .as_deref()
                .and_then(|raw| raw.parse::<i64>().ok())
                .unwrap_or(25);
            let uploaded = crate::storage::drain(state, limit).await?;
            Ok(JobReport::Done {
                message: format!("{uploaded} mirror(s) uploaded"),
                post_id: None,
            })
        }
        other => Err(Error::invalid(format!("unknown job kind: {other}"))),
    }
}

fn payload_i64(job: &job_db::Job) -> Result<i64> {
    job.payload
        .as_deref()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .ok_or_else(|| {
            Error::invalid(format!(
                "job #{} ({}) has no numeric payload",
                job.id, job.kind
            ))
        })
}

/// The post a publish outcome delivered, if it delivered one.
fn published_post(outcome: &crate::tg::publish::PublishOutcome) -> Option<i64> {
    match outcome {
        crate::tg::publish::PublishOutcome::Published { post_id, .. } => Some(*post_id),
        _ => None,
    }
}

/// Tell the Telegram user who asked; web and CLI jobs have no submitter.
async fn notify_submitter(state: &AppState, job: &job_db::Job, ok: bool, message: &str) {
    let Some(user) = job.submitted_by else {
        return;
    };
    let target = job.payload.clone().unwrap_or_default();
    let text = if ok {
        format!(
            "✅ <b>imported</b>\n{}\n<code>{}</code>",
            htmlescape::encode_minimal(message),
            htmlescape::encode_minimal(&target)
        )
    } else {
        format!(
            "⚠️ <b>import failed</b>\n{}\n<code>{}</code>",
            htmlescape::encode_minimal(message),
            htmlescape::encode_minimal(&target)
        )
    };
    notify(state, user, &text).await;
}

async fn notify(state: &AppState, chat_id: i64, html: &str) {
    if let Some(transport) = &state.transport
        && let Err(err) =
            crate::bot::reply::send(transport.as_ref(), &chat_id.to_string(), html).await
    {
        tracing::warn!("cannot notify user {chat_id}: {err}");
    }
}

/// Import and keep the job history in sync (the CLI path).
pub async fn import_and_record(state: &AppState, target: &str) -> Result<ImportOutcome> {
    let job_id = create_job(state, target).await?;
    let job = state
        .db
        .call(move |conn| job_db::get(conn, job_id))
        .await?
        .ok_or_else(|| crate::error::Error::NotFound(format!("job {job_id}")))?;
    match run_job(state, &job).await? {
        JobReport::Import(outcome) => Ok(outcome),
        JobReport::Done { .. } => Err(Error::other("expected an import job")),
    }
}
