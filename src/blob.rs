//! Content-addressed blob store: dedupe by blake3, thumbnails beside it.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::ImageReader;
use rusqlite::{Connection, params};

use crate::error::{Error, Result};

pub const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "m4v", "mkv"];
pub const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "avif"];

#[derive(Debug, Clone, PartialEq)]
pub struct Ingested {
    pub hash: String,
    pub ext: String,
    pub bytes: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub is_video: bool,
}

impl Ingested {
    pub fn mime(&self) -> Option<String> {
        mime_for(&self.ext).map(|m| m.to_string())
    }
}

pub fn mime_for(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        _ => return None,
    })
}

pub fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

pub fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
    thumbs: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GcReport {
    pub removed_blobs: usize,
    pub removed_thumbs: usize,
    pub freed_bytes: u64,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>, thumbs: impl Into<PathBuf>) -> Result<Self> {
        let store = Self {
            root: root.into(),
            thumbs: thumbs.into(),
        };
        std::fs::create_dir_all(&store.root)?;
        std::fs::create_dir_all(&store.thumbs)?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, hash: &str, ext: &str) -> PathBuf {
        let shard = hash.get(0..2).unwrap_or("00");
        self.root.join(shard).join(format!("{hash}.{ext}"))
    }

    pub fn thumb_path(&self, hash: &str) -> PathBuf {
        let shard = hash.get(0..2).unwrap_or("00");
        self.thumbs.join(shard).join(format!("{hash}.webp"))
    }

    pub fn read(&self, hash: &str, ext: &str) -> Result<Vec<u8>> {
        let path = self.path_for(hash, ext);
        std::fs::read(&path).map_err(|e| Error::NotFound(format!("blob {}: {e}", path.display())))
    }

    pub fn ingest_file(&self, path: &Path, ext_hint: Option<&str>) -> Result<Ingested> {
        let bytes = std::fs::read(path)?;
        let ext = ext_hint
            .map(|e| e.to_ascii_lowercase())
            .or_else(|| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
            })
            .unwrap_or_else(|| "bin".to_string());
        self.ingest_bytes(&bytes, &ext)
    }

    pub fn ingest_bytes(&self, bytes: &[u8], ext: &str) -> Result<Ingested> {
        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
        let hash = blake3::hash(bytes).to_hex().to_string();
        let dest = self.path_for(&hash, &ext);
        if !dest.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // write atomically so a crash never leaves a truncated blob
            write_atomically(&dest, |tmp| Ok(std::fs::write(tmp, bytes)?))?;
        }

        let is_video = is_video_ext(&ext);
        let (width, height) = if is_video {
            (None, None)
        } else {
            probe_dimensions(bytes).unwrap_or((None, None))
        };

        Ok(Ingested {
            hash,
            ext,
            bytes: bytes.len() as i64,
            width,
            height,
            is_video,
        })
    }

    /// Longest-edge `size` webp thumbnail. Returns the thumb hash key (the blob hash)
    /// or None when the bytes are not a decodable image.
    pub fn write_thumb(&self, hash: &str, bytes: &[u8], size: u32) -> Result<Option<PathBuf>> {
        let dest = self.thumb_path(hash);
        if dest.exists() {
            return Ok(Some(dest));
        }
        encode_thumbnail(bytes, size)?
            .map(|bytes| self.write_thumb_bytes(hash, &bytes))
            .transpose()
    }

    pub(crate) fn write_thumb_bytes(&self, hash: &str, bytes: &[u8]) -> Result<PathBuf> {
        let dest = self.thumb_path(hash);
        if dest.exists() {
            return Ok(dest);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomically(&dest, |tmp| Ok(std::fs::write(tmp, bytes)?))?;
        Ok(dest)
    }

    /// Register a blob row (idempotent).
    pub fn record(&self, conn: &Connection, ingested: &Ingested) -> Result<()> {
        conn.execute(
            "INSERT INTO blobs (hash, ext, bytes, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (hash) DO NOTHING",
            params![
                ingested.hash,
                ingested.ext,
                ingested.bytes,
                crate::db::fmt_ts(chrono::Utc::now())
            ],
        )?;
        Ok(())
    }

    pub fn thumb_exists(&self, hash: &str) -> bool {
        self.thumb_path(hash).exists()
    }

    /// Delete blobs no media row references, plus orphan thumbnails.
    pub fn gc(&self, conn: &Connection) -> Result<GcReport> {
        let mut report = GcReport::default();

        // Hold the write lock from the reference check through unlinking so a
        // media commit cannot concurrently attach a blob selected for cleanup.
        let tx =
            rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
        let orphans = crate::db::media_merges::orphan_blobs(&tx)?;
        for (hash, ext, bytes) in orphans {
            let path = self.path_for(&hash, &ext);
            if path.exists() && std::fs::remove_file(&path).is_ok() {
                report.removed_blobs += 1;
                report.freed_bytes += bytes.max(0) as u64;
            }
            let thumb = self.thumb_path(&hash);
            if thumb.exists() && std::fs::remove_file(&thumb).is_ok() {
                report.removed_thumbs += 1;
            }
            tx.execute("DELETE FROM blobs WHERE hash = ?1", [hash])?;
        }

        // thumbnails whose blob is gone
        for shard in std::fs::read_dir(&self.thumbs)?.flatten() {
            let dir = shard.path();
            if !dir.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&dir)?.flatten() {
                let path = entry.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)",
                    [stem],
                    |row| row.get(0),
                )?;
                if !exists && std::fs::remove_file(&path).is_ok() {
                    report.removed_thumbs += 1;
                }
            }
        }
        tx.commit()?;
        Ok(report)
    }

    /// Total bytes on disk (blobs + thumbs).
    pub fn disk_usage(&self) -> u64 {
        [self.root.as_path(), self.thumbs.as_path()]
            .iter()
            .map(|dir| dir_size(dir))
            .sum()
    }
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

pub(crate) fn encode_thumbnail(bytes: &[u8], size: u32) -> Result<Option<Vec<u8>>> {
    let Ok(image) = image::load_from_memory(bytes) else {
        return Ok(None);
    };
    let mut out = Cursor::new(Vec::new());
    image
        .thumbnail(size, size)
        .to_rgb8()
        .write_to(&mut out, image::ImageFormat::WebP)?;
    Ok(Some(out.into_inner()))
}

fn write_atomically(dest: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let tmp = dest.with_extension(format!("{}.part", uuid::Uuid::new_v4()));
    let result = write(&tmp).and_then(|()| Ok(std::fs::rename(&tmp, dest)?));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn probe_dimensions(bytes: &[u8]) -> Option<(Option<u32>, Option<u32>)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let dims = reader.into_dimensions().ok()?;
    Some((Some(dims.0), Some(dims.1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn dedupes_identical_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"), dir.path().join("thumbs")).unwrap();
        let data = png_bytes(8, 4);
        let a = store.ingest_bytes(&data, "png").unwrap();
        let b = store.ingest_bytes(&data, "png").unwrap();
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.width, Some(8));
        assert_eq!(a.height, Some(4));
        assert!(!a.is_video);
        assert!(store.path_for(&a.hash, "png").exists());
        assert_eq!(store.read(&a.hash, "png").unwrap(), data);
        let shard = &a.hash[0..2];
        assert!(
            store
                .path_for(&a.hash, "png")
                .to_string_lossy()
                .contains(shard)
        );
    }

    #[test]
    fn writes_thumb_and_reports_non_images() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"), dir.path().join("thumbs")).unwrap();
        let data = png_bytes(64, 32);
        let ingested = store.ingest_bytes(&data, "png").unwrap();
        let thumb = store.write_thumb(&ingested.hash, &data, 32).unwrap();
        assert!(thumb.unwrap().exists());
        assert!(store.thumb_exists(&ingested.hash));
        // second call is a no-op hit
        assert!(
            store
                .write_thumb(&ingested.hash, &data, 32)
                .unwrap()
                .is_some()
        );

        let bogus = store.ingest_bytes(b"not an image at all", "png").unwrap();
        assert!(
            store
                .write_thumb(&bogus.hash, b"not an image at all", 32)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn detects_video_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("b"), dir.path().join("t")).unwrap();
        let ingested = store.ingest_bytes(b"\x00\x00\x00 ftypisom", "mp4").unwrap();
        assert!(ingested.is_video);
        assert_eq!(ingested.mime().as_deref(), Some("video/mp4"));
        assert_eq!(mime_for("jpeg"), Some("image/jpeg"));
        assert_eq!(mime_for("xyz"), None);
    }

    #[test]
    fn gc_removes_unreferenced_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("b"), dir.path().join("t")).unwrap();
        let db = crate::db::Db::open(dir.path().join("test.sqlite3")).unwrap();
        let data = png_bytes(4, 4);
        let ingested = store.ingest_bytes(&data, "png").unwrap();
        db.with(|conn| store.record(conn, &ingested)).unwrap();
        store.write_thumb(&ingested.hash, &data, 64).unwrap();

        let report = db.with(|conn| store.gc(conn)).unwrap();
        assert_eq!(report.removed_blobs, 1);
        assert!(report.removed_thumbs >= 1);
        assert!(!store.path_for(&ingested.hash, "png").exists());
        assert!(report.freed_bytes > 0);
    }
}
