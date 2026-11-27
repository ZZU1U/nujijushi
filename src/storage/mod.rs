//! Mirror imported posts to external storage: a local folder, a WebDAV drive,
//! or a Telegram channel used as a backup drive.
//!
//! Every sink receives the same [`MirrorPost`]: the media files with a stable
//! relative path plus a `post.json` sidecar, so the export stays portable
//! whichever sink you pick.

pub mod cleanup;
pub mod local;
pub mod telegram;
pub mod webdav;

use std::path::PathBuf;

use async_trait::async_trait;

use crate::db::mirrors as mirror_db;
use crate::error::{Error, Result};
use crate::model::{Post, path_slug};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct MirrorFile {
    /// Absolute path of the blob on disk.
    pub path: PathBuf,
    /// Path inside the sink, e.g. `pixiv/nightjar/123456/00.png`.
    pub relative: String,
    pub is_video: bool,
    pub bytes: i64,
}

#[derive(Debug, Clone)]
pub struct MirrorPost {
    pub post: Post,
    /// Directory of this post inside the sink: `<source>/<artist>/<source_id>`.
    pub dir: String,
    pub files: Vec<MirrorFile>,
    /// `post.json` contents.
    pub meta: String,
}

impl MirrorPost {
    pub fn total_bytes(&self) -> i64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    pub fn meta_name(&self) -> String {
        format!("{}/post.json", self.dir)
    }
}

/// Where one post goes: shared by every sink so exports look identical.
pub fn plan(state: &AppState, post: &Post) -> Result<MirrorPost> {
    if post.media.is_empty() {
        return Err(Error::invalid(format!("post #{} has no media", post.id)));
    }
    let artist = post
        .author
        .as_ref()
        .map(|author| author.display())
        .unwrap_or_else(|| "unknown".to_string());
    let dir = format!(
        "{}/{}/{}",
        path_slug(&post.source),
        path_slug(&artist),
        path_slug(&post.source_id)
    );

    let mut files = Vec::with_capacity(post.media.len());
    for media in &post.media {
        let path = state.blobs.path_for(&media.blob_hash, &media.ext);
        files.push(MirrorFile {
            path,
            relative: format!("{dir}/{:02}.{}", media.ordinal, media.ext),
            is_video: media.is_video,
            bytes: media.bytes,
        });
    }

    let meta = serde_json::json!({
        "id": post.id,
        "source": post.source,
        "source_id": post.source_id,
        "url": post.url,
        "title": post.title,
        "description": post.description,
        "tags": post.tags,
        "rating": post.rating.as_str(),
        "nsfw": post.rating.is_nsfw(),
        "artist": {
            "name": post.author.as_ref().and_then(|a| a.name.clone()),
            "handle": post.author.as_ref().and_then(|a| a.handle.clone()),
            "url": post.author.as_ref().and_then(|a| a.url.clone()),
        },
        "imported_at": post.imported_at.to_rfc3339(),
        "posted_at": post.posted_at.map(|at| at.to_rfc3339()),
        "files": files.iter().map(|file| file.relative.clone()).collect::<Vec<_>>(),
    })
    .to_string();

    Ok(MirrorPost {
        post: post.clone(),
        dir,
        files,
        meta,
    })
}

/// Copy the outgoing gallery into a newly created directory using the mirror layout.
pub async fn share(state: &AppState, dir: &std::path::Path) -> Result<(usize, usize)> {
    let _storage = state.storage_read().await?;
    std::fs::create_dir(dir)?;
    let ids = state.db.call(|conn| crate::db::posts::ids(conn)).await?;
    let mut written = 0;
    let mut skipped = 0;
    for id in ids {
        let post = state
            .db
            .call(move |conn| crate::db::media_merges::outgoing_post(conn, id))
            .await?;
        if post.media.is_empty() {
            skipped += 1;
            continue;
        }
        let item = plan(state, &post)?;
        std::fs::create_dir_all(dir.join(&item.dir))?;
        for file in &item.files {
            std::fs::copy(&file.path, dir.join(&file.relative))?;
        }
        std::fs::write(dir.join(item.meta_name()), &item.meta)?;
        written += 1;
    }
    Ok((written, skipped))
}

#[async_trait]
pub trait Sink: Send + Sync {
    fn name(&self) -> &'static str;
    /// One line for `storage ls` / `doctor`.
    fn describe(&self) -> String;
    /// Upload the post, returning an external id (path, URL or message ids).
    async fn put(&self, item: &MirrorPost) -> Result<String>;
    /// Best-effort connectivity check.
    async fn check(&self) -> Result<String> {
        Ok(self.describe())
    }
}

/// The sinks enabled in the config, in a stable order.
pub fn sinks(state: &AppState) -> Vec<Box<dyn Sink>> {
    let mut out: Vec<Box<dyn Sink>> = Vec::new();
    if let Some(local_cfg) = &state.cfg.storage.local {
        out.push(Box::new(local::LocalSink::new(
            local_cfg.path.clone(),
            state.cfg.storage_root(),
        )));
    }
    if let Some(webdav_cfg) = &state.cfg.storage.webdav {
        match crate::http::client(&state.cfg) {
            Ok(client) => out.push(Box::new(webdav::WebdavSink::new(
                client,
                webdav_cfg.clone(),
                state.cfg.storage_root(),
            ))),
            Err(err) => tracing::error!("webdav sink disabled: {err}"),
        }
    }
    if let Some(telegram_cfg) = &state.cfg.storage.telegram {
        match &state.transport {
            Some(transport) => out.push(Box::new(telegram::TelegramSink::new(
                transport.clone(),
                telegram_cfg.chat_id.clone(),
            ))),
            None => tracing::warn!(
                "telegram storage sink needs a bot token; it is disabled for this run"
            ),
        }
    }
    out
}

/// Queue a post for every enabled sink.
pub async fn enqueue(state: &AppState, post_ids: &[i64]) -> Result<usize> {
    let sinks = sinks(state);
    if sinks.is_empty() || post_ids.is_empty() {
        return Ok(0);
    }
    let names: Vec<&'static str> = sinks.iter().map(|sink| sink.name()).collect();
    let ids = post_ids.to_vec();
    state
        .db
        .call(move |conn| {
            let mut queued = 0;
            for post_id in &ids {
                for sink in &names {
                    if mirror_db::enqueue(conn, *post_id, sink)? {
                        queued += 1;
                    }
                }
            }
            Ok(queued)
        })
        .await
}

/// Upload what is pending. Returns how many (post, sink) pairs succeeded.
pub async fn drain(state: &AppState, limit: i64) -> Result<usize> {
    let sinks = sinks(state);
    if sinks.is_empty() {
        return Ok(0);
    }
    let _storage = state.storage_read().await?;
    let batch = state
        .db
        .call(move |conn| mirror_db::pending(conn, None, limit))
        .await?;

    let mut done = 0;
    for mirror in batch {
        let Some(sink) = sinks.iter().find(|sink| sink.name() == mirror.sink) else {
            let reason = "sink is no longer enabled";
            state
                .db
                .call({
                    let sink_name = mirror.sink.clone();
                    let post_id = mirror.post_id;
                    move |conn| mirror_db::mark_failed(conn, post_id, &sink_name, reason)
                })
                .await?;
            continue;
        };
        let post_id = mirror.post_id;
        let sink_name = mirror.sink.clone();
        let post = state
            .db
            .call(move |conn| {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let post = crate::db::media_merges::outgoing_post(&tx, post_id)?;
                if post.media.is_empty() {
                    mirror_db::block_empty(&tx, post_id, &sink_name)?;
                }
                tx.commit()?;
                Ok(post)
            })
            .await;
        let post = match post {
            Ok(post) => post,
            Err(crate::error::Error::NotFound(_)) => continue,
            Err(err) => return Err(err),
        };
        if post.media.is_empty() {
            continue;
        }

        match plan(state, &post) {
            Ok(item) => match sink.put(&item).await {
                Ok(external_id) => {
                    let sink_name = sink.name().to_string();
                    let bytes = item.total_bytes();
                    state
                        .db
                        .call(move |conn| {
                            mirror_db::mark_done(
                                conn,
                                post_id,
                                &sink_name,
                                Some(&external_id),
                                bytes,
                            )
                        })
                        .await?;
                    tracing::info!(post = post_id, sink = sink.name(), "mirrored");
                    done += 1;
                }
                Err(err) => {
                    tracing::warn!(post = post_id, sink = sink.name(), "mirror failed: {err}");
                    let reason = err.to_string();
                    let sink_name = sink.name().to_string();
                    state
                        .db
                        .call(move |conn| {
                            mirror_db::mark_failed(conn, post_id, &sink_name, &reason)
                        })
                        .await?;
                }
            },
            Err(err) => {
                let reason = err.to_string();
                let sink_name = sink.name().to_string();
                state
                    .db
                    .call(move |conn| mirror_db::mark_failed(conn, post_id, &sink_name, &reason))
                    .await?;
            }
        }
    }
    Ok(done)
}

#[derive(Debug, Default)]
pub struct SyncSummary {
    pub queued: usize,
    pub uploaded: usize,
    pub skipped: usize,
}

/// Backfill: queue every post that has no mirror row yet, then upload.
pub async fn sync(
    state: &AppState,
    only_post: Option<i64>,
    limit: i64,
    dry_run: bool,
) -> Result<SyncSummary> {
    let sinks = sinks(state);
    let mut summary = SyncSummary::default();
    if sinks.is_empty() {
        return Ok(summary);
    }

    let post_ids: Vec<i64> = state
        .db
        .call(move |conn| {
            let sql = match only_post {
                Some(id) => format!("SELECT id FROM posts WHERE id = {id}"),
                None => format!(
                    "SELECT id FROM posts ORDER BY imported_at DESC LIMIT {}",
                    limit.max(1)
                ),
            };
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| row.get::<_, i64>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await?;

    let names: Vec<(&'static str, i64)> = {
        // only queue what is actually missing
        let existing = state
            .db
            .call(|conn| mirror_db::summary(conn))
            .await
            .unwrap_or_default();
        sinks
            .iter()
            .map(|sink| {
                let done = existing
                    .iter()
                    .filter(|(name, state, _)| name == sink.name() && state == "done")
                    .map(|(_, _, count)| *count)
                    .sum::<i64>();
                (sink.name(), done)
            })
            .collect()
    };

    if !dry_run {
        summary.queued = enqueue(state, &post_ids).await?;
        summary.uploaded = drain(state, limit.max(1)).await?;
    } else {
        let note: Vec<String> = names
            .iter()
            .map(|(name, done)| format!("{name} ({done} already done)"))
            .collect();
        tracing::info!(sinks = %note.join(", "), "dry run");
    }
    summary.skipped = post_ids
        .len()
        .saturating_sub(summary.queued.min(post_ids.len()));
    Ok(summary)
}

/// Per-sink totals for `storage ls` and the dashboard.
pub fn summary(state: &AppState) -> Result<Vec<(String, String, i64)>> {
    state.db.with(|conn| mirror_db::summary(conn))
}

/// Test fixtures shared by the sink tests.
#[cfg(test)]
pub mod tests_support {
    use super::*;
    use crate::model::{Author, MediaItem, PostState, Rating};
    use chrono::Utc;

    /// A planned post whose two files really exist on disk.
    pub fn sample_mirror(dir: &tempfile::TempDir) -> MirrorPost {
        let png = dir.path().join("src-00.png");
        let mp4 = dir.path().join("src-01.mp4");
        std::fs::write(&png, b"png-bytes").unwrap();
        std::fs::write(&mp4, b"mp4-bytes").unwrap();
        let post = sample_post();
        MirrorPost {
            dir: "pixiv/nightjar/123456".to_string(),
            files: vec![
                MirrorFile {
                    path: png,
                    relative: "pixiv/nightjar/123456/00.png".to_string(),
                    is_video: false,
                    bytes: 9,
                },
                MirrorFile {
                    path: mp4,
                    relative: "pixiv/nightjar/123456/01.mp4".to_string(),
                    is_video: true,
                    bytes: 9,
                },
            ],
            meta: serde_json::json!({
                "id": post.id,
                "source_id": post.source_id,
                "files": ["pixiv/nightjar/123456/00.png", "pixiv/nightjar/123456/01.mp4"],
            })
            .to_string(),
            post,
        }
    }

    pub fn sample_post() -> Post {
        Post {
            id: 7,
            source: "pixiv".into(),
            source_id: "123456".into(),
            url: Some("https://www.pixiv.net/artworks/123456".into()),
            caption_url: None,
            author: Some(Author {
                id: 1,
                source: "pixiv".into(),
                source_id: "42".into(),
                name: Some("Nightjar".into()),
                handle: Some("nightjar".into()),
                url: Some("https://www.pixiv.net/users/42".into()),
                blocked: false,
            }),
            title: Some("Quiet harbour".into()),
            description: Some("4am".into()),
            rating: Rating::Sfw,
            posted_at: None,
            imported_at: Utc::now(),
            nsfw_override: None,
            state: PostState::Approved,
            notes: None,
            tags: vec!["original".into(), "landscape".into()],
            media: (0..2)
                .map(|ordinal| MediaItem {
                    id: ordinal as i64 + 1,
                    post_id: 7,
                    ordinal: ordinal as i64,
                    blob_hash: format!("hash{ordinal}"),
                    ext: if ordinal == 1 {
                        "mp4".into()
                    } else {
                        "png".into()
                    },
                    mime: None,
                    width: Some(800),
                    height: Some(600),
                    bytes: 1024,
                    is_video: ordinal == 1,
                    poster_hash: None,
                    sensitive: false,
                    meta_json: None,
                })
                .collect(),
        }
    }

    fn state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::load_from_home(dir.path().to_path_buf()).unwrap();
        std::mem::forget(dir);
        AppState::open(cfg, None).unwrap()
    }

    #[test]
    fn layout_is_stable_and_portable() {
        let state = state();
        let planned = plan(&state, &sample_post()).unwrap();
        assert_eq!(planned.dir, "pixiv/nightjar/123456");
        assert_eq!(planned.files[0].relative, "pixiv/nightjar/123456/00.png");
        assert_eq!(planned.files[1].relative, "pixiv/nightjar/123456/01.mp4");
        assert!(planned.files[1].is_video);
        assert_eq!(planned.meta_name(), "pixiv/nightjar/123456/post.json");
        assert_eq!(planned.total_bytes(), 2048);

        let meta: serde_json::Value = serde_json::from_str(&planned.meta).unwrap();
        assert_eq!(meta["source"], "pixiv");
        assert_eq!(meta["artist"]["name"], "Nightjar");
        assert_eq!(meta["tags"][1], "landscape");
        assert_eq!(meta["files"][0], "pixiv/nightjar/123456/00.png");
    }

    #[test]
    fn unknown_artist_and_odd_names_are_slugged() {
        let state = state();
        let mut post = sample_post();
        post.author = None;
        post.source_id = "a b/c".into();
        let planned = plan(&state, &post).unwrap();
        assert_eq!(planned.dir, "pixiv/unknown/a-b-c");
    }

    #[test]
    fn posts_without_media_are_rejected() {
        let state = state();
        let mut post = sample_post();
        post.media.clear();
        assert!(plan(&state, &post).is_err());
    }
}
