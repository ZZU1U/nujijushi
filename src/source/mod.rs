//! Source abstraction: turn a URL (or a folder) into normalized posts + staged files.

pub mod gallerydl;
pub mod localdir;
pub mod normalize;
pub mod pair;
pub mod telegram;

use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::error::Result;
use crate::model::Rating;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostAuthor {
    pub source_id: String,
    pub name: Option<String>,
    pub handle: Option<String>,
    pub url: Option<String>,
}

impl PostAuthor {
    /// Same rule as the stored `model::Author::display` (one definition).
    pub fn display(&self) -> String {
        crate::model::display_author(self.name.as_deref(), self.handle.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PostFile {
    /// File staged on local disk (staging dir); consumed by the blob store.
    pub path: PathBuf,
    pub is_video: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedPost {
    pub source: String,
    pub source_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rating: Rating,
    pub posted_at: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
    pub author: Option<PostAuthor>,
    pub files: Vec<PostFile>,
    pub raw_json: Option<String>,
    /// Human note stored with the post (e.g. "public embed (degraded)").
    pub notes: Option<String>,
}

impl NormalizedPost {
    pub fn new(source: impl Into<String>, source_id: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            source_id: source_id.into(),
            url: None,
            title: None,
            description: None,
            rating: Rating::Sfw,
            posted_at: None,
            tags: Vec::new(),
            author: None,
            files: Vec::new(),
            raw_json: None,
            notes: None,
        }
    }

    /// Human label used in captions: "pixiv", "twitter", ...
    pub fn source_label(&self) -> &'static str {
        source_label(&self.source)
    }
}

pub fn source_label(source: &str) -> &'static str {
    match source {
        "twitter" | "x" => "Twitter",
        "pixiv" => "Pixiv",
        "danbooru" => "Danbooru",
        "gelbooru" => "Gelbooru",
        "artstation" => "ArtStation",
        "deviantart" => "DeviantArt",
        "mastodon" => "Mastodon",
        "bluesky" => "Bluesky",
        "tumblr" => "Tumblr",
        "local" => "local file",
        _ => "source",
    }
}

/// A place posts can be imported from.
#[async_trait::async_trait]
pub trait Source: Send + Sync {
    fn kind(&self) -> &'static str;
    fn supports(&self, target: &str) -> bool;
    async fn fetch(&self, target: &str) -> Result<Vec<NormalizedPost>>;
}

/// All sources available for this configuration, in priority order.
pub fn registry(cfg: &Config) -> Vec<Box<dyn Source>> {
    vec![
        Box::new(gallerydl::GalleryDlSource::new(cfg.clone())),
        Box::new(localdir::LocalDirSource),
    ]
}

/// Route a target to the first source that claims it.
pub async fn fetch_with(cfg: &Config, target: &str) -> Result<Vec<NormalizedPost>> {
    let target = target.trim();
    if target.is_empty() {
        return Err(crate::error::Error::invalid("empty import target"));
    }
    for source in registry(cfg) {
        if source.supports(target) {
            tracing::debug!(source = source.kind(), target, "dispatching import");
            let posts = source.fetch(target).await?;
            if posts.is_empty() {
                return Err(crate::error::Error::other(format!(
                    "{} returned no posts for {target}",
                    source.kind()
                )));
            }
            return Ok(posts);
        }
    }
    Err(crate::error::Error::invalid(format!(
        "no source can handle {target}"
    )))
}

/// Fetch a target, routing Telegram links to the bot-API importer.
pub async fn fetch_target(
    state: &crate::state::AppState,
    target: &str,
    copy_chat: Option<&str>,
) -> Result<Vec<NormalizedPost>> {
    if telegram::is_telegram_link(target) {
        let transport = state.require_transport()?;
        let chat = copy_chat.ok_or_else(|| {
            crate::error::Error::config(
                "telegram links need a chat the bot can forward into (a telegram user id)",
            )
        })?;
        return telegram::fetch(state, transport.as_ref(), target, chat).await;
    }
    fetch_with(&state.cfg, target).await
}

/// Is this something the URL-based sources can fetch? Interfaces ask this before
/// treating a message or an argument as an import; `fetch_with` routes with the
/// same vocabulary, so the answer is not re-derived per caller.
pub fn looks_importable(target: &str) -> bool {
    let target = target.trim();
    telegram::is_telegram_link(target) || gallerydl::looks_like_url(target)
}

/// Give a pasted link the scheme it is missing (`t.me/x/1` -> `https://t.me/x/1`)
/// and drop punctuation a chat client glued to it.
pub fn normalize_target(target: &str) -> String {
    let trimmed = target.trim().trim_matches(|c: char| {
        matches!(
            c,
            '<' | '(' | '[' | '"' | '\'' | ',' | ';' | ')' | ']' | '>' | '.'
        )
    });
    let lowered = trimmed.to_ascii_lowercase();
    if lowered.starts_with("t.me/") || lowered.starts_with("telegram.me/") {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_vocabulary_for_importable_targets() {
        for target in [
            "https://www.pixiv.net/artworks/1",
            "http://x.com/a/status/1",
            "t.me/night_art/42",
            "telegram.me/night_art/42",
            "https://t.me/s/night_art/42",
            "  https://x.com/a/status/1  ",
        ] {
            assert!(looks_importable(target), "{target} should be importable");
        }
        for target in ["", "hello", "pixiv.net/artworks/1", "ftp://x/1"] {
            assert!(!looks_importable(target), "{target} should not be");
        }
    }

    #[test]
    fn pasted_links_get_a_scheme_and_lose_punctuation() {
        assert_eq!(normalize_target("t.me/x/1"), "https://t.me/x/1");
        assert_eq!(normalize_target("T.ME/x/1"), "https://T.ME/x/1");
        assert_eq!(
            normalize_target("telegram.me/x/1"),
            "https://telegram.me/x/1"
        );
        assert_eq!(
            normalize_target("<https://x.com/a/status/1>,"),
            "https://x.com/a/status/1"
        );
        assert_eq!(normalize_target(" https://x.com/a "), "https://x.com/a");
    }
}
