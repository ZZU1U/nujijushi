//! Import a Telegram post through the Bot API.
//!
//! Strategy A: forward the post into a chat the bot is allowed to write to, read
//! the media file ids, download them, then delete the temporary copy.
//! Strategy B (public channels the bot cannot read): scrape the t.me embed page —
//! single image, reduced resolution, clearly marked as degraded.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::db::tags;
use crate::error::{Error, Result};
use crate::model::Rating;
use crate::source::gallerydl::run_with_timeout;
use crate::source::{NormalizedPost, PostAuthor, PostFile, Source, normalize};
use crate::state::AppState;
use crate::tg::Transport;
use crate::tg::botapi::{BotFile, BotMessage, CopiedMessage};

/// Bot API download limit per file.
pub const MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;
/// How far around the linked message id to look for album siblings.
pub const ALBUM_WINDOW: i32 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramLink {
    /// What the bot needs to address the chat: `-100…` or `@username`.
    pub channel: String,
    pub message_id: i32,
    /// Public channel username, when the link has one.
    pub public_name: Option<String>,
    /// Canonical t.me link stored on the post.
    pub canonical: String,
}

impl TelegramLink {
    pub fn label(&self) -> String {
        self.channel.trim_start_matches('@').to_string()
    }
}

pub fn is_telegram_link(target: &str) -> bool {
    let lowered = target.trim().to_ascii_lowercase();
    lowered.starts_with("https://t.me/")
        || lowered.starts_with("http://t.me/")
        || lowered.starts_with("https://telegram.me/")
        || lowered.starts_with("http://telegram.me/")
        || lowered.starts_with("t.me/")
        || lowered.starts_with("telegram.me/")
}

/// Parse `t.me/<name>/<id>`, `t.me/c/<internal>/<id>` and `t.me/s/<name>/<id>`.
pub fn parse_link(raw: &str) -> Option<TelegramLink> {
    let trimmed = raw.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let mut rest = without_scheme
        .strip_prefix("t.me/")
        .or_else(|| without_scheme.strip_prefix("telegram.me/"))?;
    // query/fragment (`?single`, `#...`) are irrelevant here
    for marker in ['?', '#'] {
        if let Some(index) = rest.find(marker) {
            rest = &rest[..index];
        }
    }
    let segments: Vec<&str> = rest.trim_matches('/').split('/').collect();
    let segments: Vec<&str> = segments
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .collect();
    // `t.me/s/<name>/<id>` is the web-preview form
    let segments = if segments.first() == Some(&"s") && segments.len() >= 3 {
        &segments[1..]
    } else {
        &segments[..]
    };

    match segments {
        ["c", internal, id] => {
            let message_id = id.parse::<i32>().ok()?;
            if internal.chars().all(|c| c.is_ascii_digit()) && !internal.is_empty() {
                Some(TelegramLink {
                    channel: format!("-100{internal}"),
                    message_id,
                    public_name: None,
                    canonical: format!("https://t.me/c/{internal}/{message_id}"),
                })
            } else {
                None
            }
        }
        [name, id] if *name != "c" => {
            let message_id = id.parse::<i32>().ok()?;
            let name = name.trim_start_matches('@').to_string();
            if name.is_empty() {
                return None;
            }
            Some(TelegramLink {
                channel: format!("@{name}"),
                message_id,
                public_name: Some(name.clone()),
                canonical: format!("https://t.me/{name}/{message_id}"),
            })
        }
        _ => None,
    }
}

/// Import a t.me link, copying through `copy_chat` (a chat id the bot can post to).
pub async fn fetch(
    state: &AppState,
    transport: &dyn Transport,
    target: &str,
    copy_chat: &str,
) -> Result<Vec<NormalizedPost>> {
    let link = parse_link(target)
        .ok_or_else(|| Error::invalid(format!("not a Telegram post link: {target}")))?;

    match collect_copies(transport, copy_chat, &link).await {
        Ok(copies) => {
            let caption = copies.iter().find_map(|copy| copy.caption.clone());
            let mut files = Vec::new();
            for copy in &copies {
                files.extend(copy.files.iter().cloned());
            }
            let staged = download_files(state, transport, &files).await;
            // the temporary copies are never left behind
            for copy in &copies {
                if let Err(err) = transport.delete_message(copy_chat, copy.message_id).await {
                    tracing::debug!("cannot delete the temporary copy: {err}");
                }
            }
            let staged = staged?;
            if staged.is_empty() {
                return Err(Error::other(
                    "the bot could not download any media from that post (20 MB limit?)",
                ));
            }
            let mut post = build_post(&link, caption.as_deref(), staged);
            post.author = Some(PostAuthor {
                source_id: link.label(),
                name: Some(link.label()),
                handle: link.public_name.as_ref().map(|name| format!("@{name}")),
                url: Some(link.canonical.clone()),
            });
            Ok(vec![post])
        }
        Err(err) => {
            if !state.cfg.public_telegram_fallback {
                return Err(Error::other(format!(
                    "cannot read that post ({err}). Add the bot to that channel, or enable [import] public_telegram_fallback."
                )));
            }
            let Some(name) = link.public_name.clone() else {
                return Err(Error::other(format!(
                    "cannot read that post ({err}): the bot is not a member and private channels have no public fallback."
                )));
            };
            let mut post = fetch_embed(state, &link, &name).await?;
            post.author = Some(PostAuthor {
                source_id: link.label(),
                name: Some(name.clone()),
                handle: Some(format!("@{name}")),
                url: Some(link.canonical.clone()),
            });
            Ok(vec![post])
        }
    }
}

async fn collect_copies(
    transport: &dyn Transport,
    copy_chat: &str,
    link: &TelegramLink,
) -> Result<Vec<CopiedMessage>> {
    let first = transport
        .copy_message(copy_chat, &link.channel, link.message_id)
        .await
        .map_err(|err| Error::other(err.to_string()))?;
    let mut copies = vec![first.clone()];

    if let Some(group) = first.media_group_id.clone() {
        for offset in 1..=ALBUM_WINDOW {
            for message_id in [link.message_id - offset, link.message_id + offset] {
                if message_id <= 0 {
                    continue;
                }
                match transport
                    .copy_message(copy_chat, &link.channel, message_id)
                    .await
                {
                    Ok(candidate)
                        if candidate.media_group_id.as_deref() == Some(group.as_str()) =>
                    {
                        copies.push(candidate);
                    }
                    Ok(candidate) => {
                        // a different post: undo the copy right away
                        let _ = transport
                            .delete_message(copy_chat, candidate.message_id)
                            .await;
                    }
                    Err(_) => {}
                }
            }
        }
    }
    copies.sort_by_key(|copy| copy.message_id);
    Ok(copies)
}

/// Download files into the staging directory, honouring the Bot API size cap.
async fn download_files(
    state: &AppState,
    transport: &dyn Transport,
    files: &[BotFile],
) -> Result<Vec<PostFile>> {
    let dir = state
        .cfg
        .staging_dir()
        .join(format!("tg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;

    let mut seen: Vec<&str> = Vec::new();
    let mut staged = Vec::new();
    for file in files {
        if seen.contains(&file.file_id.as_str()) {
            continue;
        }
        seen.push(file.file_id.as_str());
        if file.size > MAX_DOWNLOAD_BYTES {
            tracing::warn!(
                size = file.size,
                "skipping a file above the 20 MB bot download limit"
            );
            continue;
        }
        let dest = dir.join(format!("{}.{}", staged.len(), file.extension()));
        let written = transport
            .download_file(&file.file_id, &dest)
            .await
            .map_err(|err| Error::other(err.to_string()))?;
        if written == 0 {
            continue;
        }
        if written > MAX_DOWNLOAD_BYTES {
            let _ = std::fs::remove_file(&dest);
            tracing::warn!(file_id = %file.file_id, "downloaded file exceeds the bot limit, dropped");
            continue;
        }
        staged.push(PostFile {
            path: dest,
            is_video: file.is_video(),
            sensitive: false,
            meta: None,
        });
    }
    if staged.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(staged)
}

fn build_post(link: &TelegramLink, caption: Option<&str>, files: Vec<PostFile>) -> NormalizedPost {
    let caption = caption.map(|text| text.to_string());
    let tags = caption.as_deref().map(hashtags).unwrap_or_default();
    let rating = rating_for(&tags, caption.as_deref());
    let mut post = NormalizedPost::new("telegram", format!("{}/{}", link.label(), link.message_id));
    post.url = Some(link.canonical.clone());
    post.description = caption.as_deref().and_then(normalize::tidy_description);
    post.tags = tags;
    post.rating = rating;
    post.files = files;
    post
}

/// Hashtags inside a caption become tags (Telegram posts have no tag fields).
pub fn hashtags(caption: &str) -> Vec<String> {
    let raw: Vec<String> = caption
        .split_whitespace()
        .filter(|word| word.starts_with('#'))
        .map(|word| {
            word.trim_start_matches('#')
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        })
        .filter(|word| !word.is_empty())
        .collect();
    let mut tags = tags::normalize_all(raw);
    tags.truncate(30);
    tags
}

fn rating_for(tags: &[String], caption: Option<&str>) -> Rating {
    let lowered = caption.unwrap_or_default().to_ascii_lowercase();
    if lowered.contains("#nsfw") || lowered.contains("#explicit") {
        return Rating::Explicit;
    }
    normalize::rating_from_tags(tags)
}

/// Pull the post photo out of a t.me embed page.
///
/// Only post-specific markup counts: a bare `og:image` is usually the site's own
/// preview picture, and importing that would silently archive the wrong image.
pub fn extract_embed_image(html: &str) -> Option<String> {
    for marker in ["tgme_page_photo_image", "tgme_widget_message_photo_wrap"] {
        let Some(index) = html.find(marker) else {
            continue;
        };
        let tail = &html[index..];
        let Some(src) = tail.find("src=\"") else {
            continue;
        };
        let rest = &tail[src + 5..];
        let Some(end) = rest.find('"') else { continue };
        let url = &rest[..end];
        if url.starts_with("http") {
            return Some(url.replace("&amp;", "&"));
        }
    }
    None
}

/// A 404/placeholder page has no post markup at all.
pub fn looks_like_post_page(html: &str) -> bool {
    html.contains("tgme_widget_message") || html.contains("tgme_page_photo_image")
}

async fn fetch_embed(state: &AppState, link: &TelegramLink, name: &str) -> Result<NormalizedPost> {
    let page_url = format!("https://t.me/{name}/{}", link.message_id);
    let mut page_args: Vec<String> = vec!["-sSL".into(), "--max-time".into(), "30".into()];
    page_args.extend(crate::http::proxy_args(&state.cfg, "-x"));
    page_args.push(page_url);
    let output = run_with_timeout("curl", &page_args, 60)
        .await
        .map_err(|err| Error::other(format!("embed fetch failed: {err}")))?;
    if !output.status.success() {
        return Err(Error::other(
            "the public embed page is not reachable (is the channel public?)",
        ));
    }
    let html = String::from_utf8_lossy(&output.stdout).to_string();
    if !looks_like_post_page(&html) {
        return Err(Error::other(
            "that post is not publicly viewable (private channel, or deleted)",
        ));
    }
    let image = extract_embed_image(&html)
        .ok_or_else(|| Error::other("that public post has no photo the embed page exposes"))?;

    let dir = state
        .cfg
        .staging_dir()
        .join(format!("tg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let dest: PathBuf = dir.join("embed.jpg");
    let mut image_args: Vec<String> = vec![
        "-sSL".into(),
        "--max-time".into(),
        "60".into(),
        "-o".into(),
        dest.display().to_string(),
    ];
    image_args.extend(crate::http::proxy_args(&state.cfg, "-x"));
    image_args.push(image);
    let output = run_with_timeout("curl", &image_args, 120).await?;
    if !output.status.success() || !dest.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(Error::other("cannot download the embed image"));
    }
    let bytes = std::fs::metadata(&dest)?.len();
    if bytes == 0 || bytes > MAX_DOWNLOAD_BYTES {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(Error::other("the embed image is empty or too large"));
    }

    let mut post = build_post(
        link,
        None,
        vec![PostFile {
            path: dest,
            is_video: false,
            sensitive: false,
            meta: None,
        }],
    );
    post.notes = Some("public embed (degraded: single image, reduced size)".to_string());
    post.raw_json =
        Some(serde_json::json!({ "degraded": true, "source": "t.me embed page" }).to_string());
    Ok(post)
}

/// Import a post the user forwarded to the bot (its media is already readable).
pub async fn from_forwarded(
    state: &AppState,
    transport: &dyn Transport,
    message: &BotMessage,
) -> Result<Vec<NormalizedPost>> {
    let staged = download_files(state, transport, &message.files).await?;
    if staged.is_empty() {
        return Ok(Vec::new());
    }
    let source_id = match (
        message.forward_from_chat_id,
        message.forward_from_message_id,
    ) {
        (Some(chat_id), Some(message_id)) => format!("{chat_id}/{message_id}"),
        _ => format!("forward/{}/{}", message.chat_id, message.message_id),
    };
    let mut post = NormalizedPost::new("telegram_forward", source_id);
    post.description = message
        .caption
        .as_deref()
        .and_then(normalize::tidy_description);
    post.tags = message.caption.as_deref().map(hashtags).unwrap_or_default();
    post.rating = rating_for(&post.tags, message.caption.as_deref());
    post.files = staged;
    post.author = message
        .forward_from_chat_title
        .clone()
        .map(|title| PostAuthor {
            source_id: message
                .forward_from_chat_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| title.clone()),
            name: Some(title),
            handle: None,
            url: None,
        });
    Ok(vec![post])
}

/// A `Source` adapter so `nujijushi import <t.me link>` reuses the same routing.
pub struct TelegramSource;

#[async_trait]
impl Source for TelegramSource {
    fn kind(&self) -> &'static str {
        "telegram"
    }

    fn supports(&self, target: &str) -> bool {
        is_telegram_link(target)
    }

    async fn fetch(&self, target: &str) -> Result<Vec<NormalizedPost>> {
        // the real path needs a state + transport; see `source::fetch_target`
        Err(Error::invalid(format!(
            "telegram links need the bot API: use `nujijushi import` which handles them ({target})"
        )))
    }
}

/// Best-effort cleanup of a staging directory produced by this importer.
pub fn cleanup(paths: &[PostFile]) {
    let mut dirs: Vec<PathBuf> = paths
        .iter()
        .filter_map(|file| file.path.parent().map(Path::to_path_buf))
        .collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        if dir
            .file_name()
            .map(|name| name.to_string_lossy().starts_with("tg-"))
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_public_links() {
        let link = parse_link("https://t.me/night_art/42").unwrap();
        assert_eq!(link.channel, "@night_art");
        assert_eq!(link.message_id, 42);
        assert_eq!(link.public_name.as_deref(), Some("night_art"));
        assert_eq!(link.canonical, "https://t.me/night_art/42");

        assert_eq!(parse_link("t.me/night_art/42").unwrap().message_id, 42);
        assert_eq!(
            parse_link("https://t.me/night_art/42?single")
                .unwrap()
                .message_id,
            42
        );
        assert_eq!(
            parse_link("https://t.me/s/night_art/42").unwrap().channel,
            "@night_art"
        );
    }

    #[test]
    fn parses_private_links() {
        let link = parse_link("https://t.me/c/1234567890/77").unwrap();
        assert_eq!(link.channel, "-1001234567890");
        assert_eq!(link.message_id, 77);
        assert!(link.public_name.is_none());
        assert_eq!(link.canonical, "https://t.me/c/1234567890/77");
    }

    #[test]
    fn rejects_non_posts() {
        assert!(parse_link("https://t.me/night_art").is_none());
        assert!(parse_link("https://example.com/x/1").is_none());
        assert!(parse_link("https://t.me/night_art/notanumber").is_none());
        assert!(parse_link("").is_none());
        assert!(!is_telegram_link("https://t.me"));
        assert!(is_telegram_link("https://t.me/night_art/1"));
    }

    #[test]
    fn hashtags_become_tags() {
        let tags = hashtags("#Original #digital_art a caption #nsfw");
        assert!(tags.contains(&"original".to_string()));
        assert!(tags.contains(&"digital_art".to_string()));
        assert!(tags.contains(&"nsfw".to_string()));
        assert_eq!(tags.len(), 3, "plain words are not tags");
    }

    #[test]
    fn rating_follows_the_caption() {
        assert_eq!(rating_for(&[], Some("nice #nsfw art")), Rating::Explicit);
        assert_eq!(rating_for(&["nude".to_string()], None), Rating::Explicit);
        assert_eq!(
            rating_for(&["lingerie".to_string()], None),
            Rating::Questionable
        );
        assert_eq!(
            rating_for(&["landscape".to_string()], Some("hello")),
            Rating::Sfw
        );
    }

    #[test]
    fn extracts_the_embed_photo() {
        let html = r#"<a class="tgme_widget_message_photo_wrap" style="background-image:url('https://cdn4.telegram-cdn.org/file/abc.jpg')">
            <img class="tgme_page_photo_image" src="https://cdn4.telegram-cdn.org/file/abc.jpg"></a>"#;
        assert_eq!(
            extract_embed_image(html).as_deref(),
            Some("https://cdn4.telegram-cdn.org/file/abc.jpg")
        );

        // a page that only carries the site preview image is not a post
        let og =
            r#"<meta property="og:image" content="https://cdn4.telegram-cdn.org/file/xyz.jpg">"#;
        assert!(extract_embed_image(og).is_none());
        assert!(!looks_like_post_page(og));

        assert!(extract_embed_image("<html>no photo</html>").is_none());
    }
}
