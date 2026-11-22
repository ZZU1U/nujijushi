//! Supported gallery-dl feeds and their stable post identities.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use url::Url;

use super::{fanbox, gallerydl, normalize};
use crate::config::Config;
use crate::error::{Error, Result};

#[derive(Debug)]
enum Site {
    Fanbox(String),
    Pixiv,
    Twitter,
    Bluesky,
    Danbooru,
    Gelbooru,
}

impl Site {
    fn source(&self) -> &'static str {
        match self {
            Self::Fanbox(_) => "fanbox",
            Self::Pixiv => "pixiv",
            Self::Twitter => "twitter",
            Self::Bluesky => "bluesky",
            Self::Danbooru => "danbooru",
            Self::Gelbooru => "gelbooru",
        }
    }

    fn permalink(&self, id: &str, meta: &Value) -> Option<String> {
        if matches!(self, Self::Bluesky) {
            let did = meta.get("author")?.get("did")?.as_str()?;
            let uri = meta.get("uri")?.as_str()?;
            if did.starts_with("did:")
                && actor(did)
                && token(id)
                && uri == format!("at://{did}/app.bsky.feed.post/{id}")
            {
                return Some(format!("https://bsky.app/profile/{did}/post/{id}"));
            }
            return None;
        }
        if !numeric(id) {
            return None;
        }
        Some(match self {
            Self::Fanbox(creator) => {
                let listed = meta.get("creatorId")?.as_str()?;
                if !creator.eq_ignore_ascii_case(listed) {
                    return None;
                }
                format!("{}/posts/{id}", fanbox::creator_url(creator))
            }
            Self::Pixiv => format!("https://www.pixiv.net/artworks/{id}"),
            Self::Twitter => format!("https://x.com/i/web/status/{id}"),
            Self::Danbooru => format!("https://danbooru.donmai.us/posts/{id}"),
            Self::Gelbooru => format!("https://gelbooru.com/index.php?page=post&s=view&id={id}"),
            Self::Bluesky => return None,
        })
    }
}

#[derive(Debug)]
pub struct Feed {
    site: Site,
    pub url: String,
}

fn numeric(value: &str) -> bool {
    value.bytes().all(|ch| ch.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|number| number > 0)
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn actor(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".:-".contains(&c))
}

fn booru_tags(url: &Url, ordering: &str, prefix: &str) -> Option<String> {
    let mut fields = url.query_pairs().filter(|(key, _)| key == "tags");
    let (_, tags) = fields.next()?;
    if fields.next().is_some() || tags.trim().is_empty() {
        return None;
    }
    // The checkpoint requires the default newest-first order.
    let mut tags: Vec<&str> = tags.split_whitespace().collect();
    if tags.iter().any(|tag| {
        (tag.starts_with(prefix) && *tag != ordering)
            || ["ordfav:", "ordfavgroup:", "ordpool:"]
                .iter()
                .any(|meta| tag.starts_with(meta))
    }) {
        return None;
    }
    tags.retain(|tag| *tag != ordering);
    // Gelbooru and Danbooru free accounts limit the number of search terms.
    // Their default sort is already ID descending, so don't consume a term.
    if tags.is_empty() {
        return None;
    }
    Some(tags.join(" "))
}

impl Feed {
    pub fn parse(raw: &str) -> Result<Self> {
        let invalid = || {
            Error::invalid(
                "Use a Pixiv or FANBOX creator page, X or Bluesky artist media feed, or newest-first Danbooru or Gelbooru tag search",
            )
        };
        let url = Url::parse(raw.trim()).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(invalid());
        }
        if let Some(creator) = fanbox::creator_from_feed(&url) {
            let creator = creator.to_ascii_lowercase();
            return Ok(Self {
                url: format!("{}/posts", fanbox::creator_url(&creator)),
                site: Site::Fanbox(creator),
            });
        }
        let path: Vec<_> = url.path().trim_matches('/').split('/').collect();
        let (site, canonical) = match (url.host_str(), path.as_slice()) {
            (
                Some("www.pixiv.net" | "pixiv.net"),
                ["users", id]
                | ["en", "users", id]
                | ["users", id, "artworks"]
                | ["en", "users", id, "artworks"],
            ) if numeric(id) => (
                Site::Pixiv,
                format!("https://www.pixiv.net/users/{id}/artworks"),
            ),
            (
                Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com"),
                [name] | [name, "media"],
            ) if token(name)
                && ![
                    "i",
                    "home",
                    "search",
                    "explore",
                    "settings",
                    "intent",
                    "notifications",
                    "messages",
                ]
                .contains(&name.to_ascii_lowercase().as_str()) =>
            {
                (
                    Site::Twitter,
                    format!("https://x.com/{}/media", name.to_ascii_lowercase()),
                )
            }
            (Some("bsky.app"), ["profile", id] | ["profile", id, "media"])
                if actor(id) && (id.contains('.') || id.starts_with("did:")) =>
            {
                (
                    Site::Bluesky,
                    format!("https://bsky.app/profile/{id}/media"),
                )
            }
            (Some("danbooru.donmai.us"), ["posts"]) => {
                let tags = booru_tags(&url, "order:id_desc", "order:").ok_or_else(invalid)?;
                let mut canonical =
                    Url::parse("https://danbooru.donmai.us/posts").map_err(|_| invalid())?;
                canonical.query_pairs_mut().append_pair("tags", &tags);
                (Site::Danbooru, canonical.to_string())
            }
            (Some("gelbooru.com" | "www.gelbooru.com"), ["index.php" | ""])
                if url.query_pairs().any(|(k, v)| k == "page" && v == "post")
                    && url.query_pairs().any(|(k, v)| k == "s" && v == "list") =>
            {
                let tags = booru_tags(&url, "sort:id", "sort:").ok_or_else(invalid)?;
                let mut canonical =
                    Url::parse("https://gelbooru.com/index.php").map_err(|_| invalid())?;
                canonical
                    .query_pairs_mut()
                    .append_pair("page", "post")
                    .append_pair("s", "list")
                    .append_pair("tags", &tags);
                (Site::Gelbooru, canonical.to_string())
            }
            _ => return Err(invalid()),
        };
        Ok(Self {
            site,
            url: canonical,
        })
    }

    pub fn source(&self) -> &'static str {
        self.site.source()
    }

    fn args(&self, cfg: &Config, initial: usize, since: Option<DateTime<Utc>>) -> Vec<String> {
        let mut args = vec![
            "--dump-json".into(),
            "--no-input".into(),
            "--no-colors".into(),
            "-o".into(),
            "output.jsonl=false".into(),
        ];
        let site_options: &[&str] = match self.site {
            Site::Fanbox(_) => &[fanbox::DISABLE_EMBEDS],
            Site::Twitter => &[
                "extractor.twitter.pinned=false",
                "extractor.twitter.retweets=false",
                "extractor.twitter.quoted=false",
            ],
            Site::Bluesky => &[
                "extractor.bluesky.reposts=false",
                "extractor.bluesky.quoted=false",
            ],
            Site::Danbooru => &["extractor.danbooru.external=false"],
            _ => &[],
        };
        for option in site_options {
            args.extend(["-o".into(), (*option).into()]);
        }
        if let Some(since) = since {
            args.extend([
                "--date-after".into(),
                (since - Duration::seconds(1)).timestamp().to_string(),
            ]);
        } else {
            args.extend(["--post-range".into(), format!("1-{initial}")]);
        }
        let tool = gallerydl::GalleryDlSource::new(cfg.clone());
        args.extend(gallerydl::network_args(
            tool.cookie_file(&self.url).as_deref(),
            None,
        ));
        args.extend(crate::http::proxy_args(cfg, "--proxy"));
        args.push(self.url.clone());
        args
    }

    pub async fn discover(
        &self,
        cfg: &Config,
        initial: usize,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Candidate>> {
        let args = self.args(cfg, initial, since);
        let output =
            gallerydl::run_with_timeout(&cfg.gallery_dl, &args, cfg.import_timeout_secs).await?;
        if !output.status.success() {
            return Err(Error::other(format!(
                "gallery-dl feed scan exited with {}",
                output.status
            )));
        }
        let mut items = self.parse_listing(&output.stdout)?;
        if since.is_none() {
            items.truncate(initial);
        }
        Ok(items)
    }

    fn parse_listing(&self, bytes: &[u8]) -> Result<Vec<Candidate>> {
        let records: Vec<Vec<Value>> = serde_json::from_slice(bytes)
            .map_err(|_| Error::other("gallery-dl returned invalid feed metadata"))?;
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        for record in &records {
            match record.as_slice() {
                [kind, error] if kind.as_i64() == Some(-1) => {
                    let category = error
                        .get("error")
                        .and_then(Value::as_str)
                        .filter(|s| token(s))
                        .unwrap_or("extractor error");
                    return Err(Error::other(format!(
                        "gallery-dl feed scan failed ({category})"
                    )));
                }
                [kind, media, meta] if kind.as_i64() == Some(3) => {
                    if matches!(self.site, Site::Fanbox(_))
                        && (fanbox::restricted(meta) || !fanbox::supported_media(meta, media))
                    {
                        continue;
                    }
                    let post = normalize::from_gallery_metadata(meta, vec![], self.source(), None)
                        .filter(|post| post.source == self.source())
                        .ok_or_else(|| {
                            Error::other("gallery-dl feed item has no matching source identity")
                        })?;
                    let url = self.site.permalink(&post.source_id, meta).ok_or_else(|| {
                        Error::other("gallery-dl feed item has no valid post link")
                    })?;
                    let published_at = post
                        .posted_at
                        .filter(|date| *date <= Utc::now() + Duration::minutes(10))
                        .ok_or_else(|| {
                            Error::other("gallery-dl feed item has no valid publication time")
                        })?;
                    if seen.insert(post.source_id.clone()) {
                        candidates.push(Candidate {
                            source_id: post.source_id,
                            url,
                            published_at,
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(candidates)
    }
}

#[derive(Debug)]
pub struct Candidate {
    pub source_id: String,
    pub url: String,
    pub published_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_stable_ids_and_permalinks_from_gallery_dl_lists() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/watched_gallery_listings.json"
        ))
        .unwrap();
        for (url, source, expected) in [
            (
                "https://www.fanbox.cc/@paper-bird/",
                "fanbox",
                "https://paper-bird.fanbox.cc/posts/501",
            ),
            (
                "https://www.pixiv.net/en/users/12345",
                "pixiv",
                "https://www.pixiv.net/artworks/123",
            ),
            (
                "https://twitter.com/Artist",
                "twitter",
                "https://x.com/i/web/status/1935131998317236224",
            ),
            (
                "https://bsky.app/profile/artist.bsky.social",
                "bluesky",
                "https://bsky.app/profile/did:plc:artwork/post/3lu22ub5jqk2k",
            ),
            (
                "https://danbooru.donmai.us/posts?tags=watercolor",
                "danbooru",
                "https://danbooru.donmai.us/posts/3400",
            ),
            (
                "https://gelbooru.com/index.php?page=post&s=list&tags=watercolor",
                "gelbooru",
                "https://gelbooru.com/index.php?page=post&s=view&id=654",
            ),
        ] {
            let feed = Feed::parse(url).unwrap();
            assert_eq!(feed.source(), source);
            let bytes = serde_json::to_vec(&fixtures[source]).unwrap();
            let candidates = feed.parse_listing(&bytes).unwrap();
            assert_eq!(candidates.len(), 1, "{source}");
            assert_eq!(candidates[0].url, expected);
            if source == "fanbox" {
                assert_eq!(
                    candidates[0].published_at.to_rfc3339(),
                    "2025-09-02T05:11:03+00:00"
                );
            }
        }
    }

    #[test]
    fn rejects_direct_posts_and_out_of_order_searches() {
        for url in [
            "https://www.fanbox.cc/",
            "https://www.fanbox.cc/@www",
            "https://www.fanbox.cc/@paper-bird/posts/501",
            "https://paper-bird.fanbox.cc/posts/501",
            "https://paper-bird.fanbox.cc.example.test/",
            "https://two.labels.fanbox.cc/",
            "https://www.pixiv.net/artworks/3",
            "https://x.com/artist/status/40",
            "https://danbooru.donmai.us/posts?tags=artist+order%3Ascore",
            "https://gelbooru.com/index.php?page=post&s=view&id=5",
            "https://www.pixiv.net@unrelated.test/users/1",
        ] {
            assert!(Feed::parse(url).is_err(), "{url}");
        }
    }

    #[test]
    fn fanbox_normalizes_creator_feeds_and_rejects_mismatched_post_metadata() {
        for url in [
            "https://PAPER-BIRD.fanbox.cc/",
            "https://www.fanbox.cc/@paper-bird/posts/?page=2#old",
            "https://fanbox.cc/@paper-bird",
        ] {
            let feed = Feed::parse(url).unwrap();
            assert_eq!(feed.url, "https://paper-bird.fanbox.cc/posts");
            let mismatched = serde_json::json!([[3, "https://downloads.fanbox.cc/media.jpg", {
                "category": "fanbox", "id": "501", "creatorId": "different-creator", "date": "2025-09-02 05:11:03"
            }]]);
            let err = feed
                .parse_listing(&serde_json::to_vec(&mismatched).unwrap())
                .unwrap_err();
            assert!(err.to_string().contains("no valid post link"));
        }
    }

    #[test]
    fn scan_arguments_count_posts_and_include_equal_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        config.ensure_layout().unwrap();
        config.proxy_url = Some("http://127.0.0.1:8888".into());
        let cookie = config.cookies_dir().join("pixiv.txt");
        std::fs::write(&cookie, "").unwrap();
        let feed = Feed::parse("https://pixiv.net/users/8").unwrap();
        let args = feed.args(&config, 10, None);
        assert!(args.windows(2).any(|args| args == ["--post-range", "1-10"]));
        assert!(
            args.windows(2)
                .any(|args| args == ["--proxy", "http://127.0.0.1:8888"])
        );
        assert!(
            args.windows(2)
                .any(|args| args == ["--cookies", cookie.to_str().unwrap()])
        );
        let since = DateTime::from_timestamp(1001, 0).unwrap();
        let args = feed.args(&config, 10, Some(since));
        assert!(args.windows(2).any(|args| args == ["--date-after", "1000"]));
        assert!(!args.iter().any(|arg| arg == "--post-range"));
    }
}
