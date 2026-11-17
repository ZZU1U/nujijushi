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
    let copy_chat = copy_chat_for(state, submitted_by);
    let posts = crate::source::fetch_target(state, target, copy_chat.as_deref()).await?;
    let outcome = import_posts(state, posts.clone(), submitted_by).await?;

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
) -> Result<ImportOutcome> {
    let mut outcome = ImportOutcome::default();
    for post in &posts {
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
        .call(move |conn| job_db::create_for(conn, "import", &target, 0, submitted_by))
        .await
}

/// Run a job that is already marked `running`, then tell the submitter how it went.
pub async fn run_job(state: &AppState, job: &job_db::Job) -> Result<ImportOutcome> {
    let target = job.payload.clone().unwrap_or_default();
    state
        .db
        .call({
            let id = job.id;
            move |conn| job_db::set_state(conn, id, "running", Some("importing"))
        })
        .await?;

    let result = import_target(state, &target, job.submitted_by).await;
    match &result {
        Ok(outcome) => {
            let message = outcome.summary();
            state
                .db
                .call({
                    let id = job.id;
                    let message = message.clone();
                    let total = outcome.post_ids.len() as i64;
                    let first_post = outcome.post_ids.first().copied();
                    move |conn| {
                        job_db::progress(conn, id, total, total)?;
                        if let Some(post_id) = first_post {
                            job_db::attach_post(conn, id, post_id)?;
                        }
                        job_db::set_state(conn, id, "done", Some(&message))
                    }
                })
                .await?;
            if let Some(user) = job.submitted_by {
                let text = format!(
                    "✅ <b>imported</b>\n{}\n<code>{}</code>",
                    htmlescape::encode_minimal(&message),
                    htmlescape::encode_minimal(&target)
                );
                notify(state, user, &text).await;
            }
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
            if let Some(user) = job.submitted_by {
                let text = format!(
                    "⚠️ <b>import failed</b>\n{}\n<code>{}</code>",
                    htmlescape::encode_minimal(&message),
                    htmlescape::encode_minimal(&target)
                );
                notify(state, user, &text).await;
            }
        }
    }
    result
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
    run_job(state, &job).await
}
