//! Run image edits outside the request and commit only against their selected gallery.

use crate::db::{
    jobs::Job,
    media_merges::{self, Intent, Rendition},
};
use crate::error::{Error, Result};
use crate::media::compress::Compression;
use crate::state::AppState;

use super::JobReport;

pub(super) async fn run(state: &AppState, job: &Job) -> Result<JobReport> {
    let post_id = job
        .post_id
        .ok_or_else(|| Error::invalid("Missing merge post ID"))?;
    let job_id = job.id;
    let payload = job.payload.clone().unwrap_or_default();
    let (intent, compression) = parse(post_id, &payload)?;
    let first_id = intent.first_id;
    let last_id = intent.last_id;
    let version = intent.version.to_owned();
    let panels = state
        .db
        .call(move |conn| {
            media_merges::panels(
                conn,
                &Intent {
                    post_id,
                    first_id,
                    last_id,
                    version: &version,
                },
            )
        })
        .await?;
    if !panels.iter().all(crate::media::merge::supports) {
        return Err(Error::invalid("Select still JPEG, PNG or WebP images"));
    }
    let paths = panels
        .iter()
        .map(|panel| state.blobs.path_for(&panel.blob_hash, &panel.ext))
        .collect::<Vec<_>>();
    let limit = state.cfg.max_upload_bytes;
    let db = state.db.clone();
    let total = job.total;
    let (bytes, thumbnail) = tokio::task::spawn_blocking(move || {
        let bytes = crate::media::merge::stitch(&paths, limit, compression, |count| {
            db.with(|conn| crate::db::jobs::progress(conn, job_id, count as i64, total))
        })?;
        with_thumbnail(bytes)
    })
    .await??;
    let store = state.blobs.clone();
    let version = intent.version.to_owned();
    let message = state
        .db
        .call(move |conn| {
            media_merges::finish(
                conn,
                &store,
                job_id,
                &Intent {
                    post_id,
                    first_id,
                    last_id,
                    version: &version,
                },
                Rendition {
                    bytes: &bytes,
                    ext: if compression.is_some() { "jpg" } else { "png" },
                    thumbnail: &thumbnail,
                },
            )
        })
        .await?;
    Ok(JobReport::Done {
        message,
        post_id: Some(post_id),
    })
}

pub(super) async fn compress(state: &AppState, job: &Job) -> Result<JobReport> {
    let invalid = || Error::invalid("Invalid image compression request");
    let post_id = job.post_id.ok_or_else(invalid)?;
    let payload = job.payload.as_deref().ok_or_else(invalid)?;
    let mut parts = payload.split(':');
    let media_id = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let version = parts
        .next()
        .filter(|part| part.len() == 64)
        .ok_or_else(invalid)?
        .to_owned();
    let settings = parse_compression(parts)?;
    let input = state
        .db
        .call({
            let version = version.clone();
            move |conn| media_merges::compression_input(conn, post_id, media_id, &version)
        })
        .await?;
    let path = state.blobs.path_for(&input.blob_hash, &input.ext);
    let limit = state.cfg.max_upload_bytes;
    let (bytes, thumbnail) = tokio::task::spawn_blocking(move || {
        with_thumbnail(crate::media::compress::compress(&path, settings, limit)?)
    })
    .await??;
    let job_id = job.id;
    let store = state.blobs.clone();
    let message = state
        .db
        .call(move |conn| {
            media_merges::finish_compression(
                conn,
                &store,
                job_id,
                &Intent {
                    post_id,
                    first_id: media_id,
                    last_id: media_id,
                    version: &version,
                },
                Rendition {
                    bytes: &bytes,
                    ext: "jpg",
                    thumbnail: &thumbnail,
                },
            )
        })
        .await?;
    Ok(JobReport::Done {
        message,
        post_id: Some(post_id),
    })
}

fn with_thumbnail(bytes: Vec<u8>) -> Result<(Vec<u8>, Vec<u8>)> {
    let thumbnail = crate::blob::encode_thumbnail(&bytes, 512)?
        .ok_or_else(|| Error::other("Could not generate a thumbnail for the edited image"))?;
    Ok((bytes, thumbnail))
}

fn parse(post_id: i64, payload: &str) -> Result<(Intent<'_>, Option<Compression>)> {
    let invalid = || Error::invalid("Invalid media merge request");
    let mut parts = payload.split(':').peekable();
    let first_id = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let last_id = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(invalid)?;
    let version = parts
        .next()
        .filter(|part| part.len() == 64)
        .ok_or_else(invalid)?;
    let compression = parts
        .peek()
        .is_some()
        .then(|| parse_compression(parts))
        .transpose()?;
    Ok((
        Intent {
            post_id,
            first_id,
            last_id,
            version,
        },
        compression,
    ))
}

fn parse_compression<'a>(mut parts: impl Iterator<Item = &'a str>) -> Result<Compression> {
    let parsed = (parts.next(), parts.next(), parts.next());
    match parsed {
        (Some(quality), Some(fit), None) => {
            let quality = quality
                .parse()
                .map_err(|_| Error::invalid("Invalid JPEG quality"))?;
            let fit = fit
                .parse()
                .map_err(|_| Error::invalid("Invalid compression mode"))?;
            Compression::new(quality, fit)
        }
        _ => Err(Error::invalid("Invalid image compression request")),
    }
}
