//! Import artwork that is already on disk: a folder of images, optionally with
//! a `post.json` / `meta.json` / `info.json` describing the set.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::source::pair::{self, StagedFile};
use crate::source::{NormalizedPost, Source, normalize};

const SET_META_NAMES: &[&str] = &["post.json", "meta.json", "info.json", "artwork.json"];

pub struct LocalDirSource;

impl LocalDirSource {
    fn set_metadata(dir: &Path) -> Option<Value> {
        SET_META_NAMES
            .iter()
            .map(|name| dir.join(name))
            .find(|path| path.exists())
            .and_then(|path| pair::read_json(&path))
    }
}

/// Translate our forgiving sidecar schema into the gallery-dl-ish keys the
/// shared normalizer understands.
fn widen(meta: &Value) -> Value {
    let Value::Object(map) = meta else {
        return meta.clone();
    };
    let mut out: Map<String, Value> = map.clone();

    let alias = |out: &mut Map<String, Value>, from: &[&str], to: &str| {
        if out.contains_key(to) {
            return;
        }
        for key in from {
            if let Some(value) = out.get(*key).cloned() {
                out.insert(to.to_string(), value);
                return;
            }
        }
    };

    alias(&mut out, &["artist", "creator", "author"], "user_name");
    alias(&mut out, &["handle", "account", "username"], "user_account");
    alias(&mut out, &["source_url", "post", "link"], "post_url");
    alias(&mut out, &["id", "post"], "post_id");
    alias(&mut out, &["caption", "text", "commentary"], "description");
    alias(&mut out, &["source", "site", "platform"], "category");
    alias(&mut out, &["nsfw"], "possibly_sensitive");
    Value::Object(out)
}

#[async_trait]
impl Source for LocalDirSource {
    fn kind(&self) -> &'static str {
        "local"
    }

    fn supports(&self, target: &str) -> bool {
        Path::new(target).is_dir()
    }

    async fn fetch(&self, target: &str) -> Result<Vec<NormalizedPost>> {
        let dir = PathBuf::from(target);
        if !dir.is_dir() {
            return Err(Error::NotFound(format!("directory {}", dir.display())));
        }
        let files = pair::collect(&dir)?;
        if files.is_empty() {
            return Err(Error::invalid(format!(
                "no image or video files found in {}",
                dir.display()
            )));
        }

        // whole folder is one post when a set-level metadata file exists
        if let Some(meta) = Self::set_metadata(&dir) {
            let widened = widen(&meta);
            let files: Vec<crate::source::PostFile> =
                files.iter().map(StagedFile::to_post_file).collect();
            let post = normalize::from_gallery_metadata(
                &widened,
                files,
                "local",
                Some(&format!("file://{}", dir.display())),
            )
            .ok_or_else(|| {
                Error::invalid(format!(
                    "{} needs an \"id\" field to be imported",
                    dir.join("post.json").display()
                ))
            })?;
            return Ok(vec![post]);
        }

        // otherwise: one post per artwork group (sidecar or single file)
        let mut out = Vec::new();
        for group in pair::group(files) {
            let meta = group.meta.as_ref().map(widen).unwrap_or_else(|| {
                json!({
                    "post_id": group.key,
                    "category": "local",
                    "title": Value::Null,
                })
            });
            if let Some(post) = normalize::from_gallery_metadata(
                &meta,
                group.files.clone(),
                "local",
                Some(&format!("file://{}", dir.display())),
            ) {
                out.push(post);
            }
        }
        if out.is_empty() {
            return Err(Error::invalid(format!(
                "nothing importable in {}",
                dir.display()
            )));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path) {
        let img = image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3]));
        img.save(path).unwrap();
    }

    #[tokio::test]
    async fn imports_a_flat_folder() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("a.png"));
        write_png(&dir.path().join("b.png"));
        let posts = LocalDirSource
            .fetch(dir.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(posts.len(), 2);
        assert!(posts.iter().all(|p| p.source == "local"));
        assert!(posts.iter().all(|p| p.files.len() == 1));
    }

    #[tokio::test]
    async fn imports_a_set_folder_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("one.png"));
        write_png(&dir.path().join("two.png"));
        std::fs::write(
            dir.path().join("post.json"),
            json!({
                "id": "set-1",
                "artist": "Some Artist",
                "handle": "@artist",
                "caption": "two panels",
                "tags": ["illustration", "Original"],
                "nsfw": false,
                "source": "pixiv",
                "source_url": "https://www.pixiv.net/artworks/1"
            })
            .to_string(),
        )
        .unwrap();

        let posts = LocalDirSource
            .fetch(dir.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(posts.len(), 1);
        let post = &posts[0];
        assert_eq!(post.source_id, "set-1");
        assert_eq!(post.files.len(), 2);
        assert_eq!(
            post.author.as_ref().unwrap().name.as_deref(),
            Some("Some Artist")
        );
        assert!(post.tags.contains(&"illustration".to_string()));
        assert_eq!(post.description.as_deref(), Some("two panels"));
        assert_eq!(post.rating, crate::model::Rating::Sfw);
        assert_eq!(
            post.url.as_deref(),
            Some("https://www.pixiv.net/artworks/1")
        );
    }

    #[tokio::test]
    async fn sidecar_tags_and_rating_are_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("c.png");
        write_png(&media);
        std::fs::write(
            pair::sidecar_path(&media),
            json!({"post_id": "c1", "tags": ["nude"], "nsfw": true}).to_string(),
        )
        .unwrap();
        let posts = LocalDirSource
            .fetch(dir.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].rating, crate::model::Rating::Explicit);
    }

    #[tokio::test]
    async fn missing_dir_is_an_error() {
        assert!(
            LocalDirSource
                .fetch("/nonexistent/nujijushi/path")
                .await
                .is_err()
        );
    }
}
