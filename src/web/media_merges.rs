//! Admin actions for replacing and restoring a range of gallery images.

use std::sync::Arc;

use axum::{
    Form,
    extract::{Path, State},
    response::Response,
};

use crate::error::Error;

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
            if crate::jobs::create_merge_media_job(
                &ws.app,
                post_id,
                forms::selected_ids(&form),
                version,
            )
            .await?
            {
                "Image merge queued"
            } else {
                "An image merge is already queued for this post"
            }
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
    Ok(flash_redirect(&format!("/posts/{post_id}"), message))
}
