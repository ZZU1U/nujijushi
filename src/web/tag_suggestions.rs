//! Tag suggestion queue and review actions for authenticated admins.

use std::sync::Arc;

use axum::{
    Form,
    extract::{Path, State},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::db::tag_suggestions;
use crate::error::Error;

use super::{
    WebState,
    routes::{WebResult, check_csrf, flash_redirect, job_view},
};

pub(super) fn review_view(
    review: tag_suggestions::Review,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let suggestions = review
        .suggestions
        .iter()
        .map(|suggestion| {
            let mut view = json!(suggestion);
            if let Some(title) = suggestion.wiki_title.as_deref() {
                view["wiki_url"] = json!(wiki_url(title));
            }
            view
        })
        .collect::<Vec<_>>();
    json!({ "job": job_view(&review.job, now), "stale": review.stale, "suggestions": suggestions })
}

fn wiki_url(title: &str) -> Option<String> {
    let mut url = url::Url::parse("https://danbooru.donmai.us/wiki_pages/").ok()?;
    url.path_segments_mut().ok()?.pop_if_empty().push(title);
    Some(url.into())
}

#[derive(Deserialize)]
pub(super) struct ReviewForm {
    #[serde(default)]
    csrf: String,
    action: String,
    job_id: Option<i64>,
    suggestion_id: Option<i64>,
    tag: Option<String>,
}

pub(super) async fn action(
    State(ws): State<Arc<WebState>>,
    Path(post_id): Path<i64>,
    Form(form): Form<ReviewForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    if !ws.app.cfg.tag_suggestions.enabled {
        return Err(Error::invalid("Tag suggestions are disabled").into());
    }
    let message = match form.action.as_str() {
        "generate" => {
            let (_, queued) = crate::jobs::create_tag_suggestions_job(&ws.app, post_id).await?;
            if queued {
                "Tag suggestions queued"
            } else {
                "Tag suggestions already queued"
            }
        }
        "accept" | "discard" => {
            let (job_id, suggestion_id) = form
                .job_id
                .zip(form.suggestion_id)
                .ok_or_else(|| Error::invalid("Missing suggestion. Reload the post."))?;
            let accept = form.action == "accept";
            let tag = if accept {
                Some(
                    form.tag
                        .ok_or_else(|| Error::invalid("Enter a tag to save"))?,
                )
            } else {
                None
            };
            ws.app
                .db
                .call(move |conn| {
                    tag_suggestions::resolve(conn, post_id, job_id, suggestion_id, tag.as_deref())
                })
                .await?;
            if accept {
                "Suggested tag saved"
            } else {
                "Suggestion discarded"
            }
        }
        _ => return Err(Error::invalid("Unknown suggestion action").into()),
    };
    Ok(flash_redirect(
        &format!("/posts/{post_id}?section=suggestions"),
        message,
    ))
}
