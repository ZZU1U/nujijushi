//! Pair media files with the metadata sidecars tools like gallery-dl write next to them.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::blob::{is_image_ext, is_video_ext};
use crate::error::Result;
use crate::source::PostFile;

/// A media file plus the JSON written beside it (`<file>.json`).
#[derive(Debug, Clone, PartialEq)]
pub struct StagedFile {
    pub media: PathBuf,
    pub meta: Option<PathBuf>,
}

impl StagedFile {
    pub fn is_video(&self) -> bool {
        self.media
            .extension()
            .and_then(|e| e.to_str())
            .map(is_video_ext)
            .unwrap_or(false)
    }

    pub fn to_post_file(&self) -> PostFile {
        PostFile {
            path: self.media.clone(),
            is_video: self.is_video(),
            sensitive: false,
            meta: None,
        }
    }
}

/// Recursively collect media files (and their sidecars) under `root`.
pub fn collect(root: &Path) -> Result<Vec<StagedFile>> {
    let mut files = Vec::new();
    walk(root, &mut files)?;
    files.sort_by(|a, b| a.media.cmp(&b.media));
    Ok(files)
}

fn walk(dir: &Path, out: &mut Vec<StagedFile>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(is_image_ext(&ext) || is_video_ext(&ext)) {
            continue;
        }
        let sidecar = sidecar_path(&path);
        out.push(StagedFile {
            meta: sidecar.exists().then_some(sidecar),
            media: path,
        });
    }
    Ok(())
}

/// gallery-dl names the metadata file `<filename>.<ext>.json`.
pub fn sidecar_path(media: &Path) -> PathBuf {
    let mut name = media.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    media.with_file_name(name)
}

pub fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Key that ties several files of the same artwork together.
pub fn group_key(meta: Option<&Value>, media: &Path) -> String {
    const KEYS: &[&str] = &[
        "post_id",
        "tweet_id",
        "id",
        "illust_id",
        "media_id",
        "status_id",
        "artwork_id",
    ];
    if let Some(meta) = meta {
        for key in KEYS {
            match meta.get(*key) {
                Some(Value::String(s)) if !s.trim().is_empty() => return s.trim().to_string(),
                Some(Value::Number(n)) => return n.to_string(),
                _ => {}
            }
        }
        // fall back to the parent folder name, gallery-dl groups per post folder
        if let Some(parent) = media.parent().and_then(|p| p.file_name()) {
            return parent.to_string_lossy().to_string();
        }
    }
    media
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Debug, Clone)]
pub struct Group {
    pub key: String,
    pub meta: Option<Value>,
    pub files: Vec<PostFile>,
}

/// Group staged files by the post they belong to, keeping metadata order.
pub fn group(files: Vec<StagedFile>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for file in files {
        let meta = file.meta.as_deref().and_then(read_json);
        let key = group_key(meta.as_ref(), &file.media);
        // the sensitive flag is per item: this file's own sidecar decides, not the
        // group's first one (twitter marks individual images of a tweet)
        let mut post_file = file.to_post_file();
        post_file.sensitive = meta
            .as_ref()
            .map(crate::source::normalize::sensitive)
            .unwrap_or(false);
        post_file.meta = keep_meta(&meta);
        match groups.iter_mut().find(|g| g.key == key) {
            Some(existing) => {
                existing.files.push(post_file);
                if existing.meta.is_none() && meta.is_some() {
                    existing.meta = meta;
                }
            }
            None => groups.push(Group {
                key,
                meta,
                files: vec![post_file],
            }),
        }
    }
    groups
}

/// Longest per-item metadata blob we keep: a sidecar is a few KB, and anything
/// past this is almost always a tag dump nobody reads again.
pub const META_LIMIT: usize = 8 * 1024;

/// This file's own sidecar as JSON, or a marker when it is unreasonably large
/// (a truncated JSON document would only confuse `show --raw`).
fn keep_meta(meta: &Option<Value>) -> Option<String> {
    let value = meta.as_ref()?;
    let text = serde_json::to_string(value).ok()?;
    if text.len() <= META_LIMIT {
        Some(text)
    } else {
        Some(format!(
            r#"{{"_omitted":"sidecar was {} bytes"}}"#,
            text.len()
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sidecar_naming_matches_gallery_dl() {
        let path = Path::new("/tmp/x/123_p0.png");
        assert_eq!(sidecar_path(path), PathBuf::from("/tmp/x/123_p0.png.json"));
    }

    #[test]
    fn group_key_prefers_metadata_id() {
        let meta = json!({"post_id": 42});
        assert_eq!(group_key(Some(&meta), Path::new("/tmp/a.png")), "42");
        let meta = json!({"tweet_id": "77", "id": 5});
        assert_eq!(group_key(Some(&meta), Path::new("/tmp/a.png")), "77");
        assert_eq!(group_key(None, Path::new("/tmp/holiday.png")), "holiday");
    }

    #[test]
    fn groups_album_files_together() {
        let dir = tempfile::tempdir().unwrap();
        let meta = json!({"post_id": "900", "tags": ["a"]});
        for i in 0..3 {
            let media = dir.path().join(format!("900_p{i}.png"));
            std::fs::write(&media, b"x").unwrap();
            std::fs::write(sidecar_path(&media), meta.to_string()).unwrap();
        }
        let single = dir.path().join("lonely.png");
        std::fs::write(&single, b"x").unwrap();

        let files = collect(dir.path()).unwrap();
        assert_eq!(files.len(), 4);
        let groups = group(files);
        assert_eq!(groups.len(), 2);
        let album = groups.iter().find(|g| g.key == "900").unwrap();
        assert_eq!(album.files.len(), 3);
        assert!(album.meta.is_some());
        let lonely = groups.iter().find(|g| g.key == "lonely").unwrap();
        assert_eq!(lonely.files.len(), 1);
        assert!(lonely.meta.is_none());
    }

    #[test]
    fn skips_non_media_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("image.PNG"), b"x").unwrap();
        let files = collect(dir.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].media.ends_with("image.PNG"));
    }

    #[test]
    fn each_file_keeps_its_own_sensitive_flag() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("1_p0.png");
        let second = dir.path().join("1_p1.png");
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(&second, b"x").unwrap();
        std::fs::write(
            sidecar_path(&first),
            r#"{"tweet_id": 1, "possibly_sensitive": true}"#,
        )
        .unwrap();
        std::fs::write(
            sidecar_path(&second),
            r#"{"tweet_id": 1, "possibly_sensitive": false}"#,
        )
        .unwrap();

        let groups = group(collect(dir.path()).unwrap());
        assert_eq!(groups.len(), 1, "both files belong to one post");
        let flags: Vec<bool> = groups[0].files.iter().map(|file| file.sensitive).collect();
        assert_eq!(flags, vec![true, false], "the flag stays with its item");
    }

    #[test]
    fn each_file_keeps_its_own_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("1_p0.png");
        std::fs::write(&media, b"x").unwrap();
        std::fs::write(
            sidecar_path(&media),
            r#"{"tweet_id":1,"media_id":7,"width":1200}"#,
        )
        .unwrap();

        let groups = group(collect(dir.path()).unwrap());
        let meta = groups[0].files[0].meta.as_deref().expect("sidecar kept");
        assert!(meta.contains("\"media_id\":7"), "{meta}");
    }
}
