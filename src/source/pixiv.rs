//! Connect the native Pixiv client to the archive's staging and watch flows.

use std::{collections::HashSet, time::Duration};

use chrono::{DateTime, Utc};
use nujijushi_sources::pixiv::{self, Artwork, FeedItem, Original, Pixiv};
use reqwest::header::HeaderValue;

use super::watch::Candidate;
use super::{NormalizedPost, PostAuthor, PostFile, cookies, normalize, staging};
use crate::config::{Config, PixivBackend};
use crate::error::{Error, Result};

pub(super) const STAGING_PREFIX: &str = "pixiv-";

pub(crate) fn session_cookie(cfg: &Config) -> Result<Option<HeaderValue>> {
    let cookies = match std::fs::read_to_string(cfg.cookies_dir().join("pixiv.txt")) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::config("cannot read cookies/pixiv.txt")),
    };
    parse_session_cookie(&cookies, Utc::now().timestamp())
        .map(Some)
        .ok_or_else(|| Error::config("cookies/pixiv.txt needs a valid Pixiv PHPSESSID cookie"))
}

fn parse_session_cookie(cookies: &str, now: i64) -> Option<HeaderValue> {
    let value = cookies::value(cookies, "www.pixiv.net", "PHPSESSID", now)?;
    let mut header = HeaderValue::from_str(&format!("PHPSESSID={value}")).ok()?;
    header.set_sensitive(true);
    Some(header)
}

fn client(cfg: &Config) -> Result<(Pixiv, reqwest::Client)> {
    let http = crate::http::client_with_redirects(cfg, reqwest::redirect::Policy::none())?;
    let pixiv = Pixiv::new(http.clone(), session_cookie(cfg)?);
    #[cfg(test)]
    let pixiv = match &cfg.pixiv_test_origin {
        Some(origin) => pixiv.with_test_origin(origin),
        None => pixiv,
    };
    Ok((pixiv, http))
}

pub(super) async fn fetch(cfg: &Config, target: &str) -> Result<Option<Vec<NormalizedPost>>> {
    let Some(id) = pixiv::artwork_id(target).filter(|_| cfg.pixiv_backend == PixivBackend::Native)
    else {
        return Ok(None);
    };
    with_timeout(cfg, fetch_artwork(cfg, id)).await
}

pub(crate) async fn watch_artist(cfg: &Config, id: u64) -> Result<pixiv::Artist> {
    with_timeout(cfg, async {
        let (pixiv, _) = client(cfg)?;
        pixiv
            .artwork_artist(id)
            .await
            .map_err(|error| Error::other(error.to_string()))
    })
    .await
}

async fn fetch_artwork(cfg: &Config, id: u64) -> Result<Option<Vec<NormalizedPost>>> {
    let (pixiv, http) = client(cfg)?;
    let artwork = match pixiv.artwork(id).await {
        Ok(artwork) => artwork,
        Err(pixiv::Error::Ugoira) => return Ok(None),
        Err(error) => return Err(Error::other(error.to_string())),
    };
    let staging = staging::StagingDir::new(cfg, STAGING_PREFIX)?;
    let url = pixiv::artwork_url(artwork.id);
    let mut files = Vec::with_capacity(artwork.pages.len());
    for (index, page) in artwork.pages.iter().enumerate() {
        let extension = page
            .url
            .path()
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .filter(|ext| crate::blob::is_image_ext(ext))
            .ok_or_else(|| Error::other("Pixiv original has an unsupported image format"))?;
        let path = staging.path().join(format!("{:04}.{extension}", index + 1));
        #[cfg(test)]
        let media_url = match &cfg.pixiv_test_origin {
            Some(origin) => {
                let mut url = origin.clone();
                url.set_path(page.url.path());
                url
            }
            None => page.url.clone(),
        };
        #[cfg(not(test))]
        let media_url = page.url.clone();
        staging::download(
            &http,
            media_url,
            &url,
            &path,
            &format!("Pixiv page {}", index + 1),
            "image/",
        )
        .await?;
        let Original { width, height, .. } = page;
        files.push(PostFile {
            path,
            is_video: false,
            sensitive: false,
            meta: Some(serde_json::json!({"width": width, "height": height}).to_string()),
        });
    }
    let post = normalize_artwork(artwork, url, files);
    staging.finish();
    Ok(Some(vec![post]))
}

fn normalize_artwork(artwork: Artwork, url: String, files: Vec<PostFile>) -> NormalizedPost {
    let mut post = NormalizedPost::new("pixiv", artwork.id.to_string());
    post.url = Some(url);
    post.title = normalize::tidy_description(&artwork.title);
    post.description = normalize::tidy_description(&artwork.description)
        .filter(|desc| post.title.as_ref() != Some(desc));
    post.rating = normalize::pixiv_rating(artwork.restriction.into());
    post.posted_at = Some(artwork.published_at);
    post.tags = normalize::add_title_tags(
        crate::db::tags::normalize_all(artwork.tags),
        post.title.as_deref(),
    );
    let handle = artwork
        .artist
        .account
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let name = artwork.artist.name.trim();
    post.author = Some(PostAuthor {
        source_id: artwork.artist.id.to_string(),
        name: (!name.is_empty() && !normalize::same_handle(name, handle.as_deref()))
            .then(|| name.to_owned()),
        handle,
        url: Some(format!("https://www.pixiv.net/users/{}", artwork.artist.id)),
    });
    post.raw_json = Some(artwork.metadata.to_string());
    post.files = files;
    post
}

pub(crate) struct CreatorScan {
    pub candidates: Vec<Candidate>,
    pub visible: HashSet<String>,
}

pub(crate) async fn discover_watch(
    cfg: &Config,
    creator: u64,
    initial: usize,
    since: Option<DateTime<Utc>>,
    seen: Option<&HashSet<String>>,
) -> Result<CreatorScan> {
    with_timeout(cfg, async {
        let (pixiv, _) = client(cfg)?;
        let (visible, mut items) = match scan_creator(&pixiv, creator, seen).await {
            Err(pixiv::Error::MissingSummaries) => scan_creator(&pixiv, creator, seen).await,
            result => result,
        }
        .map_err(|error| Error::other(error.to_string()))?;
        let latest_valid_time = Utc::now() + chrono::Duration::minutes(10);
        if items
            .iter()
            .any(|item| item.published_at > latest_valid_time)
        {
            return Err(Error::other("Pixiv returned an invalid publication time"));
        }
        items.sort_unstable_by_key(|item| std::cmp::Reverse((item.published_at, item.id)));
        if seen.is_none() {
            match since {
                Some(since) => items.retain(|item| item.published_at >= since),
                None => items.truncate(initial),
            }
        }
        let candidates = items
            .into_iter()
            .map(|item| Candidate {
                source_id: item.id.to_string(),
                url: pixiv::artwork_url(item.id),
                published_at: item.published_at,
            })
            .collect();
        Ok(CreatorScan {
            candidates,
            visible,
        })
    })
    .await
}

async fn scan_creator(
    pixiv: &Pixiv,
    creator: u64,
    seen: Option<&HashSet<String>>,
) -> std::result::Result<(HashSet<String>, Vec<FeedItem>), pixiv::Error> {
    let index = pixiv.creator_index(creator).await?;
    let mut visible = HashSet::with_capacity(index.len());
    let mut needs_summary = Vec::new();
    for id in index {
        let source_id = id.to_string();
        if seen.is_none_or(|seen| !seen.contains(&source_id)) {
            needs_summary.push(id);
        }
        visible.insert(source_id);
    }
    let items = pixiv.creator_summaries(creator, &needs_summary).await?;
    Ok((visible, items))
}

async fn with_timeout<T>(cfg: &Config, run: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(cfg.import_timeout_secs), run)
        .await
        .map_err(|_| Error::Timeout("native Pixiv request exceeded the import timeout".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_only_a_current_pixiv_session() {
        let jar = concat!(
            ".example.com\tTRUE\t/\tTRUE\t0\tPHPSESSID\tforeign\n",
            ".pixiv.net\tTRUE\t/\tTRUE\t10\tPHPSESSID\texpired\n",
            ".pixiv.net\tTRUE\t/\tTRUE\t0\tPHPSESSID\tinvalid;value\n",
            "#HttpOnly_.pixiv.net\tTRUE\t/\tTRUE\t0\tPHPSESSID\tsession\n",
            ".pixiv.net\tTRUE\t/\tTRUE\t0\tOTHER\tunrelated\n",
        );
        let cookie = parse_session_cookie(jar, 11).unwrap();
        assert_eq!(cookie, "PHPSESSID=session");
        assert!(cookie.is_sensitive());
        assert!(
            parse_session_cookie(".pixiv.net\tTRUE\t/\tTRUE\t10\tPHPSESSID\texpired", 11).is_none()
        );
    }
}
