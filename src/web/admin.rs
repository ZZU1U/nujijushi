//! Browser navigation shell and the same-origin admin API.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use serde::Serialize;

use super::{WebState, routes::job_view};

include!(concat!(env!("OUT_DIR"), "/admin_assets.rs"));

const MISSING_ASSETS: &str = "The React admin was not bundled. Run npm ci --prefix webui and npm run build --prefix webui, then rebuild nujijushi.";

pub(super) fn assets_available() -> bool {
    SHELL.is_some()
}

#[derive(Serialize)]
struct ActionResult<'a> {
    message: &'a str,
    job_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<&'a str>,
}

pub(super) fn action_result(location: &str, message: &str) -> Response {
    accepted(location, message, Vec::new())
}

pub(super) fn accepted(location: &str, message: &str, job_ids: Vec<i64>) -> Response {
    let status = if job_ids.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    let location = super::auth::local_page(location).map(|_| location);
    (
        status,
        Json(ActionResult {
            message,
            job_ids,
            location,
        }),
    )
        .into_response()
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

pub(super) fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

fn ui_page(path: &str) -> bool {
    matches!(
        path,
        "/" | "/login"
            | "/posts"
            | "/channels"
            | "/jobs"
            | "/import"
            | "/duplicates"
            | "/watches"
            | "/users"
            | "/settings"
    ) || match path.split('/').collect::<Vec<_>>().as_slice() {
        ["", "posts", id]
        | ["", "posts", id, "delete"]
        | ["", "channels", id, "edit" | "queue" | "preview"] => id.parse::<i64>().is_ok(),
        _ => false,
    }
}

pub(super) async fn navigation(
    State(ws): State<Arc<WebState>>,
    req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD) && ui_page(req.uri().path()) {
        return match SHELL {
            Some(shell) => {
                let shell = shell
                    .replace(
                        "=\"./assets/",
                        &format!("=\"{}", ws.url("/admin-assets/assets/")),
                    )
                    .replacen(
                        "name=\"nujijushi-base-path\" content=\"\"",
                        &format!(
                            "name=\"nujijushi-base-path\" content=\"{}\"",
                            ws.base_path()
                        ),
                        1,
                    );
                ([(header::CACHE_CONTROL, "no-store")], Html(shell)).into_response()
            }
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CACHE_CONTROL, "no-store")],
                MISSING_ASSETS,
            )
                .into_response(),
        };
    }
    next.run(req).await
}

pub(super) async fn asset(Path(name): Path<String>) -> Response {
    let Some((_, content)) = ASSETS.iter().find(|(key, _)| *key == name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = if name.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if name.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        *content,
    )
        .into_response()
}

pub(super) async fn job(State(ws): State<Arc<WebState>>, Path(id): Path<i64>) -> Response {
    match ws
        .app
        .db
        .call(move |conn| crate::db::jobs::get(conn, id))
        .await
    {
        Ok(Some(job)) => Json(job_view(&job, chrono::Utc::now())).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "Job not found"),
        Err(err) => super::routes::WebError::from(err).into_response(),
    }
}

pub(super) async fn api_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let status = response.status();
    if !status.is_success()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"application/json"))
    {
        response = error(
            status,
            status.canonical_reason().unwrap_or("Request failed"),
        );
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
