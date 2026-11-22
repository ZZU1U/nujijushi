//! Incremental, local indexing of still artwork for duplicate review.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;

use image::{
    DynamicImage, ImageReader,
    codecs::{png::PngDecoder, webp::WebPDecoder},
    imageops::FilterType,
};

use crate::blob::BlobStore;
use crate::db::duplicates::{self, Fingerprint, PendingMedia};
use crate::error::Result;
use crate::state::AppState;

const BATCH_SIZE: i64 = 16;

pub async fn index_batch(state: &AppState) -> Result<()> {
    let Some(lock) = state.try_staging_lock("duplicates.lock").await? else {
        return Ok(());
    };
    let items = state
        .db
        .call(|conn| duplicates::pending(conn, BATCH_SIZE))
        .await?;
    if items.is_empty() {
        return Ok(());
    }
    let db = state.db.clone();
    let blobs = state.blobs.clone();
    // Keep decoding and the batch lock together, including if the worker is cancelled.
    tokio::task::spawn_blocking(move || {
        let _lock = lock;
        let mut cache = HashMap::new();
        let mut backed_off = HashSet::new();
        for item in &items {
            if item.fingerprinted {
                db.with(|conn| duplicates::finish(conn, item, item.fingerprint))?;
                continue;
            }
            if backed_off.contains(item.blob_hash.as_str()) {
                continue;
            }
            match cache
                .entry(item.blob_hash.as_str())
                .or_insert_with(|| read_fingerprint(&blobs, item))
            {
                Ok(fingerprint) => {
                    db.with(|conn| duplicates::finish(conn, item, *fingerprint))?;
                }
                Err(err) => {
                    if db.with(|conn| duplicates::record_failure(conn, item))? {
                        backed_off.insert(item.blob_hash.as_str());
                        tracing::debug!(
                            media = item.id,
                            "visual duplicate check will retry: {err}"
                        );
                    }
                }
            }
        }
        Ok(())
    })
    .await?
}

fn read_fingerprint(blobs: &BlobStore, item: &PendingMedia) -> Result<Option<Fingerprint>> {
    let original = blobs.path_for(&item.blob_hash, &item.ext);
    match item.ext.as_str() {
        "png" => {
            let decoder = PngDecoder::new(BufReader::new(File::open(&original)?))?;
            if decoder.is_apng()? {
                return Ok(None);
            }
        }
        "webp" => {
            let decoder = WebPDecoder::new(BufReader::new(File::open(&original)?))?;
            if decoder.has_animation() {
                return Ok(None);
            }
        }
        "jpg" | "jpeg" => {}
        _ => return Ok(None),
    }
    if let Ok(thumb) = image::open(blobs.thumb_path(&item.blob_hash)) {
        return Ok(Some(fingerprint(&thumb)));
    }
    let mut reader = ImageReader::open(&original)?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(Some(fingerprint(&reader.decode()?)))
}

fn fingerprint(image: &DynamicImage) -> Fingerprint {
    let aspect = f64::from(image.width()) / f64::from(image.height());
    let pixels = image.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            hash |= u64::from(pixels.get_pixel(x, y)[0] > pixels.get_pixel(x + 1, y)[0]);
        }
    }
    Fingerprint {
        hash: hash as i64,
        aspect,
    }
}
