//! Import orchestration: source -> blob store -> database, plus job bookkeeping.

mod media_merge;
pub mod tag_suggestions;
mod watch;
pub mod worker;

use std::path::{Path, PathBuf};

use crate::blob::Ingested;
use crate::db::posts::{NewMedia, StoredImport};
use crate::db::reputation::{self, MatchedSuggestion, Suggestion};
use crate::db::{jobs as job_db, posts as post_db};
use crate::error::{Error, Result};
use crate::source::{NormalizedPost, gallerydl, telegram};
use crate::state::AppState;

#[derive(Debug, Clone, Default)]
pub struct ImportOutcome {
    pub post_ids: Vec<i64>,
    pub created: usize,
    pub updated: usize,
    pub files: usize,
    pub skipped: Vec<(String, String)>,
    pub suggestions: Vec<Suggestion>,
    pub gallery_edits_reset: usize,
}

impl ImportOutcome {
    /// One short line for a job row, a bot reply or the CLI.
    ///
    /// A job usually holds one target, so "1 post(s): 1 new, 0 updated, 3
    /// file(s)" was mostly zeros. What changed is what matters:
    /// `new post · 3 files`, `12 posts (10 new, 2 updated) · 34 files`.
    pub fn summary(&self) -> String {
        let gallery_edits_reset = (self.gallery_edits_reset > 0).then(|| {
            format!(
                "Reset gallery edits on {} after the source gallery changed",
                plural(self.gallery_edits_reset, "post")
            )
        });
        if !self.suggestions.is_empty() {
            let mut lines = Vec::new();
            for suggestion in &self.suggestions {
                let description = suggestion.describe();
                if !lines.contains(&description) {
                    lines.push(description);
                }
            }
            if !self.skipped.is_empty() {
                lines.push(format!("{} skipped", self.skipped.len()));
            }
            lines.extend(gallery_edits_reset);
            return lines.join("\n");
        }
        let posts = self.post_ids.len();
        let mut out = if posts == 1 {
            if self.created == 1 {
                "new post".to_string()
            } else {
                "updated post".to_string()
            }
        } else {
            format!(
                "{posts} posts ({} new, {} updated)",
                self.created, self.updated
            )
        };
        out.push_str(&format!(" · {}", plural(self.files, "file")));
        if !self.skipped.is_empty() {
            out.push_str(&format!(" · {} skipped", self.skipped.len()));
        }
        if let Some(note) = gallery_edits_reset {
            out.push_str(&format!(" · {note}"));
        }
        out
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
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
    if let (Some(user), Some(link)) = (submitted_by, telegram::parse_link(target)) {
        let targets = vec![link.channel];
        if let Some(matched) =
            known_telegram_submission(state, user, &targets, link.message_id).await?
        {
            return Ok(suggestion_outcome(matched));
        }
        if state.cfg.guest_reputation
            && let Some(transport) = &state.transport
            && let Ok(Ok(info)) = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                transport.get_chat(&targets[0]),
            )
            .await
        {
            let mut aliases = vec![info.id.to_string()];
            if let Some(name) = info.username {
                aliases.push(format!("@{name}"));
            }
            aliases.retain(|alias| !alias.eq_ignore_ascii_case(&targets[0]));
            if let Some(matched) =
                known_telegram_submission(state, user, &aliases, link.message_id).await?
            {
                return Ok(suggestion_outcome(matched));
            }
        }
    }
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

    if outcome.post_ids.is_empty() && outcome.suggestions.is_empty() {
        let detail = outcome
            .skipped
            .first()
            .map(|(_, err)| err.clone())
            .unwrap_or_else(|| "no posts returned".to_string());
        return Err(Error::other(format!("import of {target} failed: {detail}")));
    }
    Ok(outcome)
}

/// A link or forward of one of our own publications can be identified without
/// downloading it, including when the guest cannot access the destination.
pub async fn known_telegram_submission(
    state: &AppState,
    user: i64,
    targets: &[String],
    message: i32,
) -> Result<Option<MatchedSuggestion>> {
    if !state.cfg.guest_reputation || message <= 0 || targets.is_empty() {
        return Ok(None);
    }
    let cfg = state.cfg.clone();
    let targets = targets.to_vec();
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let matched = if reputation::is_guest(&tx, &cfg.access, user)? {
                reputation::known_message(&tx, &targets, message, user)?
            } else {
                None
            };
            tx.commit()?;
            Ok(matched)
        })
        .await
}

fn suggestion_outcome(matched: MatchedSuggestion) -> ImportOutcome {
    ImportOutcome {
        post_ids: matched.post_id.into_iter().collect(),
        suggestions: vec![matched.status],
        ..ImportOutcome::default()
    }
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
    let mut mirrored = Vec::new();
    for (index, post) in posts.iter().enumerate() {
        match store_post(state, post, submitted_by, false).await {
            Ok(stored) => {
                outcome.post_ids.extend(stored.post_id);
                if stored.suggestion.is_none() || stored.created {
                    outcome.files += post.files.len();
                    mirrored.extend(stored.post_id);
                    if stored.created {
                        outcome.created += 1;
                    } else {
                        outcome.updated += 1;
                    }
                }
                outcome.suggestions.extend(stored.suggestion);
                outcome.gallery_edits_reset += usize::from(stored.gallery_edits_reset);
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

    enqueue_import_mirrors(state, &mirrored).await;
    Ok(outcome)
}

async fn enqueue_import_mirrors(state: &AppState, post_ids: &[i64]) {
    if state.cfg.storage.on_import && state.cfg.storage.any_enabled() && !post_ids.is_empty() {
        match crate::storage::enqueue(state, post_ids).await {
            Ok(queued) if queued > 0 => tracing::info!(queued, "queued for external storage"),
            Ok(_) => {}
            Err(err) => tracing::warn!("cannot queue storage mirrors: {err}"),
        }
    }
}

async fn store_post(
    state: &AppState,
    post: &NormalizedPost,
    submitted_by: Option<i64>,
    insert_only: bool,
) -> Result<StoredImport> {
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
    let cfg = state.cfg.clone();
    state
        .db
        .call(move |conn| {
            post_db::store_import(conn, &post, &media, submitted_by, insert_only, &cfg)
        })
        .await
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
pub const KIND_FLUSH_CHANNEL: &str = "flush_channel";
pub const KIND_STORAGE_DRAIN: &str = "storage_drain";
pub const KIND_WATCH_CHECK: &str = "watch_check";
pub const KIND_TAG_SUGGESTIONS: &str = "suggest_tags";
pub const KIND_MERGE_MEDIA: &str = "merge_media";
pub const KIND_COMPRESS_MEDIA: &str = "compress_media";

pub fn is_gallery_edit(kind: &str) -> bool {
    matches!(kind, KIND_MERGE_MEDIA | KIND_COMPRESS_MEDIA)
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
        .call(move |conn| job_db::create_for(conn, KIND_IMPORT, &target, 0, submitted_by))
        .await
}

/// Queue an import unless the same target is already waiting or running (the
/// `/jobs` retry button: a double click must not start two imports).
pub async fn create_import_job_unique(state: &AppState, target: &str) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_IMPORT, target.to_string()).await
}

/// Queue "publish the next post of this channel", unless one is already queued.
pub async fn create_publish_channel_job(state: &AppState, channel_id: i64) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_PUBLISH_CHANNEL, channel_id.to_string()).await
}

/// Queue "publish exactly this queue item", unless one is already queued.
pub async fn create_publish_item_job(state: &AppState, queue_id: i64) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_PUBLISH_ITEM, queue_id.to_string()).await
}

pub async fn create_flush_channel_job(state: &AppState, channel_id: i64) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_FLUSH_CHANNEL, channel_id.to_string()).await
}

async fn flush_job_lock(state: &AppState, channel_id: i64) -> Result<Option<std::fs::File>> {
    state
        .try_staging_lock(&format!("flush-job-{channel_id}.lock"))
        .await
}

/// Queue a storage drain (uploads whatever mirrors are waiting).
pub async fn create_drain_job(state: &AppState, limit: usize) -> Result<Option<i64>> {
    enqueue_unique(state, KIND_STORAGE_DRAIN, limit.to_string()).await
}

pub async fn create_watch_check_job(state: &AppState, watch_id: i64) -> Result<(i64, bool)> {
    state
        .db
        .call(move |conn| crate::db::watches::enqueue(conn, watch_id, KIND_WATCH_CHECK))
        .await
}

pub async fn create_tag_suggestions_job(state: &AppState, post_id: i64) -> Result<(i64, bool)> {
    if !state.cfg.tag_suggestions.enabled {
        return Err(Error::invalid("Tag suggestions are disabled"));
    }
    state
        .db
        .call(move |conn| crate::db::tag_suggestions::enqueue(conn, post_id, KIND_TAG_SUGGESTIONS))
        .await
}

pub async fn create_merge_media_job(
    state: &AppState,
    post_id: i64,
    selected: Vec<i64>,
    version: String,
    compression: Option<crate::media::compress::Compression>,
) -> Result<Option<i64>> {
    let job = state
        .db
        .call(move |conn| {
            crate::db::media_merges::enqueue(conn, post_id, &selected, &version, compression)
        })
        .await?;
    if job.is_some() {
        state.media_jobs.notify_one();
    }
    Ok(job)
}

pub async fn create_compress_media_job(
    state: &AppState,
    post_id: i64,
    media_id: Option<i64>,
    version: String,
    settings: crate::media::compress::Compression,
) -> Result<Option<i64>> {
    let job = state
        .db
        .call(move |conn| {
            crate::db::media_merges::enqueue_compression(
                conn, post_id, media_id, &version, settings,
            )
        })
        .await?;
    if job.is_some() {
        state.media_jobs.notify_one();
    }
    Ok(job)
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
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if job_db::find_active(&tx, kind, &payload)?.is_some() {
                return Ok(None);
            }
            let id = job_db::create_for(&tx, kind, &payload, 0, None)?;
            tx.commit()?;
            Ok(Some(id))
        })
        .await
}

/// What a finished job reports back.
#[derive(Debug, Clone)]
pub enum JobReport {
    Import(ImportOutcome),
    Batch {
        message: String,
        attempted: i64,
    },
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
            JobReport::Done { message, .. } | JobReport::Batch { message, .. } => message.clone(),
        }
    }

    pub fn post_ids(&self) -> &[i64] {
        match self {
            JobReport::Import(outcome) => &outcome.post_ids,
            JobReport::Done { .. } | JobReport::Batch { .. } => &[],
        }
    }
}

/// Run a job that is already marked `running`, then record how it went.
pub async fn run_job(state: &AppState, job: &job_db::Job) -> Result<JobReport> {
    let _flush_lock = if job.kind == KIND_FLUSH_CHANNEL {
        let lock = flush_job_lock(state, payload_i64(job)?)
            .await?
            .ok_or_else(|| Error::other("The queue flush job is locked by another worker"))?;
        let id = job.id;
        if !state
            .db
            .call(move |conn| Ok(job_db::get(conn, id)?.is_some_and(|job| job.state == "running")))
            .await?
        {
            return Err(Error::other(
                "The queue flush job was interrupted before it started",
            ));
        }
        Some(lock)
    } else {
        None
    };
    let _watch_lock = if job.kind == KIND_WATCH_CHECK {
        let watch_id = payload_i64(job)?;
        let lock = watch::lock(state, watch_id)
            .await?
            .ok_or_else(|| Error::other("watch is locked by another worker"))?;
        let job_id = job.id;
        let still_running = state
            .db
            .call(move |conn| {
                Ok(job_db::get(conn, job_id)?.is_some_and(|job| job.state == "running"))
            })
            .await?;
        if !still_running {
            return Err(Error::other(
                "watched check was interrupted before it started",
            ));
        }
        Some(lock)
    } else {
        None
    };
    let label = match job.kind.as_str() {
        KIND_IMPORT => "importing",
        KIND_WATCH_CHECK => "scanning source",
        KIND_TAG_SUGGESTIONS => "analyzing artwork",
        KIND_MERGE_MEDIA => "merging images",
        KIND_COMPRESS_MEDIA => "compressing image",
        KIND_PUBLISH_CHANNEL | KIND_PUBLISH_ITEM => "publishing",
        KIND_FLUSH_CHANNEL => "Flushing queue",
        KIND_STORAGE_DRAIN => "uploading mirrors",
        other => return Err(Error::invalid(format!("unknown job kind: {other}"))),
    };
    state
        .db
        .call({
            let id = job.id;
            let gallery_edit = is_gallery_edit(&job.kind);
            move |conn| {
                if gallery_edit {
                    job_db::label_running(conn, id, label)
                } else {
                    job_db::set_state(conn, id, "running", Some(label))
                }
            }
        })
        .await?;

    let result = run_kind(state, job).await;
    match &result {
        Ok(_) if job.kind == KIND_TAG_SUGGESTIONS || is_gallery_edit(&job.kind) => {
            // The media edit or suggestion batch and its success status commit together.
        }
        Ok(report) => {
            let message = report.message();
            let (total, post_id) = match report {
                JobReport::Import(outcome) => (
                    outcome.post_ids.len() as i64,
                    outcome.post_ids.first().copied(),
                ),
                JobReport::Done { post_id, .. } => (1, *post_id),
                JobReport::Batch { attempted, .. } => (*attempted, None),
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
        KIND_WATCH_CHECK => watch::check(state, payload_i64(job)?, job.id).await,
        KIND_TAG_SUGGESTIONS => tag_suggestions::run(state, payload_i64(job)?, job.id).await,
        KIND_MERGE_MEDIA => media_merge::run(state, job).await,
        KIND_COMPRESS_MEDIA => media_merge::compress(state, job).await,
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
        KIND_FLUSH_CHANNEL => {
            let report =
                crate::tg::publish::flush_channel(state, payload_i64(job)?, Some(job.id)).await?;
            Ok(JobReport::Batch {
                message: report.describe(),
                attempted: report.total,
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
        JobReport::Done { .. } | JobReport::Batch { .. } => {
            Err(Error::other("expected an import job"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(created: usize, updated: usize, files: usize, skipped: usize) -> ImportOutcome {
        ImportOutcome {
            post_ids: (0..created + updated).map(|i| i as i64).collect(),
            created,
            updated,
            files,
            skipped: (0..skipped)
                .map(|i| (format!("post-{i}"), "nope".to_string()))
                .collect(),
            suggestions: Vec::new(),
            gallery_edits_reset: 0,
        }
    }

    /// A job row is one target, so the summary must not recite the zeros.
    #[test]
    fn summaries_say_what_changed_not_what_did_not() {
        assert_eq!(outcome(1, 0, 3, 0).summary(), "new post · 3 files");
        assert_eq!(outcome(0, 1, 1, 0).summary(), "updated post · 1 file");
        assert_eq!(
            outcome(10, 2, 34, 0).summary(),
            "12 posts (10 new, 2 updated) · 34 files"
        );
        assert_eq!(
            outcome(1, 0, 3, 2).summary(),
            "new post · 3 files · 2 skipped"
        );
        assert_eq!(
            outcome(4, 0, 4, 0).summary(),
            "4 posts (4 new, 0 updated) · 4 files"
        );
    }
}
