//! Run the image merge outside the request and commit only against its selected gallery.

use crate::db::{
    jobs::Job,
    media_merges::{self, Intent},
};
use crate::error::{Error, Result};
use crate::state::AppState;

use super::JobReport;

pub(super) async fn run(state: &AppState, job: &Job) -> Result<JobReport> {
    let post_id = job
        .post_id
        .ok_or_else(|| Error::invalid("Missing merge post ID"))?;
    let job_id = job.id;
    let payload = job.payload.clone().unwrap_or_default();
    let intent = parse(post_id, &payload)?;
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
    let (png, thumbnail) = tokio::task::spawn_blocking(move || {
        let png = crate::media::merge::stitch(&paths, limit, |count| {
            db.with(|conn| crate::db::jobs::progress(conn, job_id, count as i64, total))
        })?;
        let thumbnail = crate::blob::encode_thumbnail(&png, 512)?
            .ok_or_else(|| Error::other("Could not generate a thumbnail for the merged image"))?;
        Ok::<_, Error>((png, thumbnail))
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
                &png,
                &thumbnail,
            )
        })
        .await?;
    Ok(JobReport::Done {
        message,
        post_id: Some(post_id),
    })
}

fn parse(post_id: i64, payload: &str) -> Result<Intent<'_>> {
    let invalid = || Error::invalid("Invalid media merge request");
    let mut parts = payload.splitn(3, ':');
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
    Ok(Intent {
        post_id,
        first_id,
        last_id,
        version,
    })
}
