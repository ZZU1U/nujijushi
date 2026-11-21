//! Admin actions for editing and restoring gallery images.

use std::sync::Arc;

use axum::{
    Form,
    extract::{Path, State},
    response::Response,
};

use crate::error::Error;
use crate::media::compress::Compression;

use super::{
    WebState,
    forms::{self, FormMap},
    routes::{WebResult, check_csrf},
};

pub(super) async fn action(
    State(ws): State<Arc<WebState>>,
    Path(post_id): Path<i64>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let version = forms::field(&form, "gallery_version")
        .ok_or_else(|| Error::invalid("Missing gallery version. Reload the post."))?
        .to_owned();
    let mut job_id = None;
    let message = match forms::field(&form, "action") {
        Some("reorder") => {
            let ids = forms::field(&form, "media_ids")
                .ok_or_else(|| Error::invalid("Missing artwork order"))?
                .split(',')
                .map(str::parse::<i64>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| Error::invalid("Invalid artwork order"))?;
            ws.app
                .db
                .call(move |conn| crate::db::media_merges::reorder(conn, post_id, &ids, &version))
                .await?;
            "Artwork order saved"
        }
        Some("merge") => {
            let compression = match forms::field(&form, "output") {
                None | Some("lossless") => None,
                Some(mode) => Some(compression_settings(&form, mode)?),
            };
            job_id = crate::jobs::create_merge_media_job(
                &ws.app,
                post_id,
                forms::selected_ids(&form),
                version,
                compression,
            )
            .await?;
            if job_id.is_some() {
                "Image merge queued"
            } else {
                "A gallery edit is already queued for this post"
            }
        }
        Some(action @ ("compress" | "compress_all")) => {
            let mode = forms::field(&form, "output").unwrap_or("fit");
            let settings = compression_settings(&form, mode)?;
            job_id = crate::jobs::create_compress_media_job(
                &ws.app,
                post_id,
                if action == "compress_all" {
                    None
                } else {
                    Some(media_id(&form)?)
                },
                version,
                settings,
            )
            .await?;
            if job_id.is_some() {
                "Image compression queued"
            } else {
                "A gallery edit is already queued for this post"
            }
        }
        Some(action @ ("hide" | "restore")) => {
            let media_id = media_id(&form)?;
            let hidden = action == "hide";
            ws.app
                .db
                .call(move |conn| {
                    crate::db::media_merges::set_hidden(conn, post_id, media_id, &version, hidden)
                })
                .await?;
            if hidden {
                "Image hidden from outgoing copies"
            } else {
                "Image restored to outgoing copies"
            }
        }
        Some("undo_compression") => {
            let media_id = media_id(&form)?;
            ws.app
                .db
                .call(move |conn| {
                    crate::db::media_merges::undo_compression(conn, post_id, media_id, &version)
                })
                .await?;
            "Image restored"
        }
        Some("undo") => {
            let id = forms::field(&form, "merge_id")
                .and_then(|id| id.parse().ok())
                .ok_or_else(|| Error::invalid("Missing merge ID"))?;
            ws.app
                .db
                .call(move |conn| crate::db::media_merges::undo(conn, post_id, id, &version))
                .await?;
            "Original panels restored"
        }
        _ => return Err(Error::invalid("Unknown gallery action").into()),
    };
    Ok(super::admin::accepted(
        &format!("/posts/{post_id}?section=media"),
        message,
        job_id.into_iter().collect(),
    ))
}

fn compression_settings(form: &FormMap, mode: &str) -> WebResult<Compression> {
    match mode {
        "fit" => Ok(Compression::new(85, true)?),
        "quality" => {
            let quality = forms::field(form, "quality")
                .unwrap_or("85")
                .parse()
                .map_err(|_| Error::invalid("JPEG quality must be from 1 to 100"))?;
            Ok(Compression::new(quality, false)?)
        }
        _ => Err(Error::invalid("Unknown image compression mode").into()),
    }
}

fn media_id(form: &FormMap) -> WebResult<i64> {
    Ok(forms::field(form, "media_id")
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| Error::invalid("Missing image ID"))?)
}
