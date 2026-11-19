//! Incremental, local indexing of still artwork for duplicate review.

use std::collections::HashMap;
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
    let items = state
        .db
        .call(|conn| duplicates::pending(conn, BATCH_SIZE))
        .await?;
    if items.is_empty() {
        return Ok(());
    }
    let Some(lock) = state.try_staging_lock("duplicates.lock").await? else {
        return Ok(());
    };
    let db = state.db.clone();
    let blobs = state.blobs.clone();
    // Keep decoding and the batch lock together, including if the worker is cancelled.
    tokio::task::spawn_blocking(move || {
        let _lock = lock;
        let mut cache = HashMap::new();
        for item in &items {
            let fingerprint = if item.fingerprinted {
                item.fingerprint
            } else {
                *cache
                    .entry(item.blob_hash.as_str())
                    .or_insert_with(|| read_fingerprint(&blobs, item))
            };
            db.with(|conn| duplicates::finish(conn, item, fingerprint))?;
        }
        Ok(())
    })
    .await?
}

fn read_fingerprint(blobs: &BlobStore, item: &PendingMedia) -> Option<Fingerprint> {
    let original = blobs.path_for(&item.blob_hash, &item.ext);
    match item.ext.as_str() {
        "png" => {
            let decoder = PngDecoder::new(BufReader::new(File::open(&original).ok()?)).ok()?;
            if decoder.is_apng().ok()? {
                return None;
            }
        }
        "webp" => {
            let decoder = WebPDecoder::new(BufReader::new(File::open(&original).ok()?)).ok()?;
            if decoder.has_animation() {
                return None;
            }
        }
        "jpg" | "jpeg" => {}
        _ => return None,
    }
    let image = image::open(blobs.thumb_path(&item.blob_hash))
        .ok()
        .or_else(|| {
            let mut reader = ImageReader::open(&original).ok()?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(128 * 1024 * 1024);
            reader.limits(limits);
            reader.decode().ok()
        })?;
    Some(fingerprint(&image))
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
