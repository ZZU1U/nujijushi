//! Map a signed-in X status into the archive's import format.

use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use nujijushi_sources::twitter::{self, MediaKind, Status, Twitter};
use reqwest::header::HeaderValue;

use super::{NormalizedPost, PostAuthor, PostFile, cookies, gallerydl, normalize, staging};
use crate::config::{Config, TwitterBackend};
use crate::error::{Error, Result};
use crate::model::Rating;

pub(super) const STAGING_PREFIX: &str = "twitter-";

pub(crate) struct Session {
    path: PathBuf,
    cookie: HeaderValue,
    csrf: HeaderValue,
}

pub(crate) fn session(cfg: &Config) -> Result<Option<Session>> {
    for filename in ["twitter.txt", "x.com.txt"] {
        let path = cfg.cookies_dir().join(filename);
        let jar = match std::fs::read_to_string(&path) {
            Ok(jar) => jar,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(Error::config(format!("cannot read cookies/{filename}"))),
        };
        let (cookie, csrf) = session_headers(&jar, Utc::now().timestamp()).ok_or_else(|| {
            Error::config(format!(
                "cookies/{filename} needs unexpired auth_token and ct0 cookies for x.com"
            ))
        })?;
        return Ok(Some(Session { path, cookie, csrf }));
    }
    Ok(None)
}

fn session_headers(jar: &str, now: i64) -> Option<(HeaderValue, HeaderValue)> {
    let auth = cookies::value(jar, "x.com", "auth_token", now)?;
    let csrf = cookies::value(jar, "x.com", "ct0", now)?;
    let mut cookie = HeaderValue::from_str(&format!("auth_token={auth}; ct0={csrf}")).ok()?;
    let mut csrf = HeaderValue::from_str(csrf).ok()?;
    cookie.set_sensitive(true);
    csrf.set_sensitive(true);
    Some((cookie, csrf))
}

pub(super) async fn fetch(cfg: &Config, target: &str) -> Result<Option<Vec<NormalizedPost>>> {
    if cfg.twitter_backend != TwitterBackend::Native {
        return Ok(None);
    }
    let Ok(url) = url::Url::parse(target) else {
        return Ok(None);
    };
    let Some(link) = twitter::status_link(&url) else {
        return Ok(None);
    };
    let Some(Session { path, cookie, csrf }) = session(cfg)? else {
        return Ok(None);
    };

    let http = crate::http::client_with_redirects(cfg, reqwest::redirect::Policy::none())?;
    let client = Twitter::new(http.clone(), cookie, csrf);
    #[cfg(test)]
    let client = match &cfg.twitter_test_origin {
        Some(origin) => client.with_test_origin(origin),
        None => client,
    };
    let status_url = twitter::status_url(link.id);
    let attempt = async {
        match client.status(link.id).await {
            Ok(status) => stage(cfg, &http, status, link.handle).await.map(Some),
            Err(twitter::Error::UnsupportedMedia) => Ok(None),
            Err(error) => Err(Error::other(error.to_string())),
        }
    };
    let post = tokio::time::timeout(Duration::from_secs(cfg.import_timeout_secs), attempt)
        .await
        .map_err(|_| Error::Timeout("native X import exceeded the import timeout".into()))??;
    match post {
        Some(post) => Ok(Some(vec![post])),
        None => gallerydl::GalleryDlSource::new(cfg.clone())
            .fetch_with_cookie(&status_url, &path)
            .await
            .map(Some),
    }
}

async fn stage(
    cfg: &Config,
    http: &reqwest::Client,
    status: Status,
    link_handle: Option<&str>,
) -> Result<NormalizedPost> {
    let staging = staging::StagingDir::new(cfg, STAGING_PREFIX)?;
    let url = twitter::status_url(status.id);
    let mut files = Vec::with_capacity(status.media.len());
    for (index, media) in status.media.iter().enumerate() {
        let path = staging
            .path()
            .join(format!("{:04}.{}", index + 1, media.extension));
        let is_video = media.kind == MediaKind::Video;
        let content_type = if is_video { "video/" } else { "image/" };
        #[cfg(test)]
        let media_url = match &cfg.twitter_test_origin {
            Some(origin) => {
                let mut local = origin.clone();
                local.set_path(media.url.path());
                local.set_query(media.url.query());
                local
            }
            None => media.url.clone(),
        };
        #[cfg(not(test))]
        let media_url = media.url.clone();
        staging::download(
            http,
            media_url,
            &url,
            &path,
            &format!("X attachment {}", index + 1),
            content_type,
        )
        .await?;
        files.push(PostFile {
            path,
            is_video,
            sensitive: media.sensitive,
            meta: Some(media.metadata.to_string()),
        });
    }
    let post = normalize_status(status, url, link_handle, files);
    staging.finish();
    Ok(post)
}

fn normalize_status(
    status: Status,
    url: String,
    link_handle: Option<&str>,
    files: Vec<PostFile>,
) -> NormalizedPost {
    let mut post = NormalizedPost::new("twitter", status.id.to_string());
    post.url = Some(url);
    post.description = normalize::tidy_description(&status.text);
    post.posted_at = Some(status.posted_at);
    post.raw_json = Some(
        serde_json::json!({
            "tweet_id": post.source_id,
            "content": status.text,
            "hashtags": status.hashtags,
            "possibly_sensitive": status.sensitive,
        })
        .to_string(),
    );
    post.tags = crate::db::tags::normalize_all(status.hashtags);
    post.rating = if status.sensitive || files.iter().any(|file| file.sensitive) {
        Rating::Explicit
    } else {
        normalize::rating_from_tags(&post.tags)
    };
    let handle = status
        .author
        .handle
        .or_else(|| link_handle.map(str::to_owned));
    let name = status
        .author
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !normalize::same_handle(name, handle.as_deref()))
        .map(str::to_owned);
    post.author = Some(PostAuthor {
        source_id: status.author.id.to_string(),
        name,
        url: handle
            .as_ref()
            .map(|handle| format!("https://x.com/{handle}")),
        handle,
    });
    post.files = files;
    post
}

#[cfg(test)]
mod tests;
