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
    routes::{WebResult, check_csrf, flash_redirect},
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
    let message = match forms::field(&form, "action") {
        Some("merge") => {
            let compression = match forms::field(&form, "output") {
                None | Some("lossless") => None,
                Some(mode) => Some(compression_settings(&form, mode)?),
            };
            if crate::jobs::create_merge_media_job(
                &ws.app,
                post_id,
                forms::selected_ids(&form),
                version,
                compression,
            )
            .await?
            {
                "Image merge queued"
            } else {
                "A gallery edit is already queued for this post"
            }
        }
        Some("compress") => {
            let mode = forms::field(&form, "output").unwrap_or("fit");
            let settings = compression_settings(&form, mode)?;
            if crate::jobs::create_compress_media_job(
                &ws.app,
                post_id,
                media_id(&form)?,
                version,
                settings,
            )
            .await?
            {
                "Image compression queued"
            } else {
                "A gallery edit is already queued for this post"
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
    Ok(flash_redirect(
        &format!("/posts/{post_id}?section=media"),
        message,
    ))
}

fn compression_settings(form: &FormMap, mode: &str) -> WebResult<Compression> {
    let fit = match mode {
        "fit" => true,
        "quality" => false,
        _ => return Err(Error::invalid("Unknown image compression mode").into()),
    };
    let quality = forms::field(form, "quality")
        .unwrap_or("85")
        .parse()
        .map_err(|_| Error::invalid("JPEG quality must be from 1 to 100"))?;
    Ok(Compression::new(quality, fit)?)
}

fn media_id(form: &FormMap) -> WebResult<i64> {
    Ok(forms::field(form, "media_id")
        .and_then(|id| id.parse().ok())
        .ok_or_else(|| Error::invalid("Missing image ID"))?)
}
