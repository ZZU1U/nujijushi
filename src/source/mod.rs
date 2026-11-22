//! Source abstraction: turn a URL (or a folder) into normalized posts + staged files.

mod fanbox;
pub mod gallerydl;
pub mod localdir;
pub mod normalize;
pub mod pair;
pub mod telegram;
pub mod watch;

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
    /// The source flagged *this file* as sensitive (twitter does it per image).
    pub sensitive: bool,
    /// This file's own sidecar JSON, so per-item metadata is not lost.
    pub meta: Option<String>,
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
        "fanbox" | "fanbox.cc" => "Fanbox",
        host if host.ends_with(".fanbox.cc") => "Fanbox",
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

fn trim_link(target: &str) -> &str {
    target.trim().trim_matches(|c: char| {
        matches!(
            c,
            '<' | '(' | '[' | '"' | '\'' | ',' | ';' | ')' | ']' | '>' | '.'
        )
    })
}

fn strip_encoded_newline(text: &str) -> Option<&str> {
    ["%0d%0a", "%0a"].into_iter().find_map(|separator| {
        let (prefix, rest) = text.split_at_checked(separator.len())?;
        prefix.eq_ignore_ascii_case(separator).then_some(rest)
    })
}

fn starts_with_link(text: &str) -> bool {
    // A link's scheme or bare Telegram prefix occurs before any percent escapes.
    looks_importable(trim_link(text.split('%').next().unwrap_or_default()))
}

/// Recognize encoded breaks in a pasted link list while preserving URL fields
/// such as `?q=first%0Asecond` and literal percent escapes in local paths.
pub fn split_pasted_links(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut search_from = 0;
    for (index, _) in text.match_indices('%') {
        if index < search_from {
            continue;
        }
        let Some(rest) = strip_encoded_newline(&text[index..]) else {
            continue;
        };
        let mut next = rest.trim_start();
        while let Some(rest) = strip_encoded_newline(next) {
            next = rest.trim_start();
        }
        search_from = text.len() - next.len();
        if !next.is_empty() && !starts_with_link(next) {
            continue;
        }
        let previous = text[start..index].trim();
        let previous_link = previous.split_whitespace().next_back();
        if previous_link.is_some_and(|link| !starts_with_link(link)) {
            continue;
        }
        if next.is_empty() && previous_link.is_none_or(|link| link.contains(['?', '#'])) {
            continue;
        }
        if !previous.is_empty() {
            parts.push(previous);
        }
        start = search_from;
    }
    if start < text.len() || start == 0 {
        parts.push(&text[start..]);
    }
    parts
}

/// Give a pasted link the scheme it is missing (`t.me/x/1` -> `https://t.me/x/1`)
/// and drop punctuation a chat client glued to it.
pub fn normalize_target(target: &str) -> String {
    let trimmed = trim_link(target);
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
    fn splits_encoded_newlines_between_links_but_keeps_url_values_and_paths() {
        let urls =
            "%0Ahttps://x.com/a/status/1%0d%0A%0a t.me/art/2%0Ahttps://example.test/search?q=a%0Ab";
        assert_eq!(
            split_pasted_links(urls),
            [
                "https://x.com/a/status/1",
                "t.me/art/2",
                "https://example.test/search?q=a%0Ab",
            ]
        );
        assert_eq!(
            split_pasted_links("https://t.me/art/2%0a"),
            ["https://t.me/art/2"]
        );
        for untouched in [
            "https://example.test/search?q=a%0A",
            "/tmp/art%0A archive",
            "https://example.test/art%20archive",
        ] {
            assert_eq!(split_pasted_links(untouched), [untouched]);
        }
    }

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
