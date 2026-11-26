//! Stage RedNote media and map the note into the archive's import format.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use nujijushi_sources::rednote::{self, MediaKind, Note, RedNote};
use url::Url;

use super::{NormalizedPost, PostAuthor, PostFile, cookies, normalize, staging};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::Rating;

pub(super) const STAGING_PREFIX: &str = "rednote-";
const COOKIE_PATH: &str = "rednote.txt";
const HOSTS: [&str; 4] = [
    "www.rednote.com",
    "rednote.com",
    "www.xiaohongshu.com",
    "xiaohongshu.com",
];

pub(crate) fn session(cfg: &Config, now: i64) -> Result<Option<String>> {
    let jar = match std::fs::read_to_string(cfg.cookies_dir().join(COOKIE_PATH)) {
        Ok(jar) => jar,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::config("cannot read cookies/rednote.txt")),
    };
    if HOSTS
        .iter()
        .any(|host| cookies::has_for_host(&jar, host, now))
    {
        Ok(Some(jar))
    } else {
        Err(Error::config(
            "cookies/rednote.txt needs unexpired Netscape cookies for RedNote",
        ))
    }
}

pub(super) async fn fetch(cfg: &Config, target: &str) -> Result<Option<Vec<NormalizedPost>>> {
    let Ok(url) = Url::parse(target) else {
        return Ok(None);
    };
    if !rednote::is_site_url(&url) {
        return Ok(None);
    }
    let attempt = async {
        let http = crate::http::client_with_redirects(cfg, reqwest::redirect::Policy::none())?;
        let client = RedNote::new(http.clone());
        #[cfg(test)]
        let client = match &cfg.rednote_test_origin {
            Some(origin) => client.with_test_origin(origin),
            None => client,
        };
        let now = Utc::now().timestamp();
        let jar = session(cfg, now)?;
        let note = client
            .note(url, |url| {
                jar.as_deref()
                    .and_then(|jar| cookies::header_for(jar, url, now))
            })
            .await
            .map_err(|error| Error::other(error.to_string()))?;
        stage(cfg, &http, note).await
    };
    let post = tokio::time::timeout(Duration::from_secs(cfg.import_timeout_secs), attempt)
        .await
        .map_err(|_| Error::Timeout("RedNote import exceeded the import timeout".into()))??;
    Ok(Some(vec![post]))
}

async fn stage(cfg: &Config, http: &reqwest::Client, note: Note) -> Result<NormalizedPost> {
    let staging = staging::StagingDir::new(cfg, STAGING_PREFIX)?;
    let mut files = Vec::with_capacity(note.media.len());
    for (index, media) in note.media.iter().enumerate() {
        let index = index + 1;
        let pending = staging.path().join(format!("{index:04}.part"));
        let is_video = media.kind == MediaKind::Video;
        let mime = if is_video { "video/mp4" } else { "image/" };
        #[cfg(test)]
        let media_url = match &cfg.rednote_test_origin {
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
            &note.url,
            &pending,
            &format!("RedNote attachment {index}"),
            mime,
        )
        .await?;
        let extension = if is_video {
            "mp4"
        } else {
            image_extension(&pending)?
        };
        let path = staging.path().join(format!("{index:04}.{extension}"));
        tokio::fs::rename(pending, &path).await?;
        files.push(PostFile {
            path,
            is_video,
            sensitive: false,
            meta: None,
        });
    }
    let post = normalize_note(note, files);
    staging.finish();
    Ok(post)
}

fn image_extension(path: &Path) -> Result<&'static str> {
    image::ImageReader::open(path)?
        .with_guessed_format()?
        .format()
        .and_then(|format| {
            format
                .extensions_str()
                .iter()
                .copied()
                .find(|ext| crate::blob::is_image_ext(ext))
        })
        .ok_or_else(|| Error::other("RedNote attachment has an unsupported image format"))
}

fn normalize_note(note: Note, files: Vec<PostFile>) -> NormalizedPost {
    let mut post = NormalizedPost::new("rednote", note.id);
    post.title = note.title.as_deref().and_then(normalize::tidy_description);
    post.description = note
        .description
        .as_deref()
        .and_then(normalize::tidy_description)
        .filter(|description| post.title.as_ref() != Some(description));
    post.tags = normalize::add_title_tags(
        crate::db::tags::normalize_all(note.tags),
        post.title.as_deref(),
    );
    post.posted_at = note.published_at;
    post.rating = if note.sensitive {
        Rating::Explicit
    } else {
        normalize::rating_from_tags(&post.tags)
    };
    post.author = note.author.map(|author| {
        let url = Url::parse(&note.url).ok().and_then(|url| {
            let mut url = url.join("/user/profile/").ok()?;
            url.path_segments_mut()
                .ok()?
                .pop_if_empty()
                .push(&author.id);
            Some(url.to_string())
        });
        PostAuthor {
            source_id: author.id,
            name: author.name,
            handle: None,
            url,
        }
    });
    post.url = Some(note.url);
    post.files = files;
    post
}

#[cfg(test)]
mod tests;
