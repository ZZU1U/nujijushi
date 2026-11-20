//! Browser navigation shell and the same-origin admin API.

use std::sync::Arc;

use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use serde_json::{Value, json};

use super::{WebState, routes::job_view};

include!(concat!(env!("OUT_DIR"), "/admin_assets.rs"));

#[derive(Clone)]
pub(super) struct ClientError(pub String);

#[derive(Clone, Copy)]
pub(super) struct AcceptedJob(pub i64);

#[derive(Clone)]
pub(super) struct AcceptedJobs(pub Vec<i64>);

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
    ) || match path.split('/').collect::<Vec<_>>().as_slice() {
        ["", "posts", id]
        | ["", "posts", id, "delete"]
        | ["", "channels", id, "edit" | "queue" | "preview"] => id.parse::<i64>().is_ok(),
        _ => false,
    }
}

pub(super) async fn navigation(req: Request, next: Next) -> Response {
    if let Some(shell) = SHELL
        && req.method() == Method::GET
        && ui_page(req.uri().path())
        && req
            .headers()
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/html"))
    {
        return ([(header::CACHE_CONTROL, "no-store")], Html(shell)).into_response();
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
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Job not found"})),
        )
            .into_response(),
        Err(err) => super::routes::WebError::from(err).into_response(),
    }
}

pub(super) async fn api_response(req: Request, next: Next) -> Response {
    let response = next.run(req).await;
    let status = response.status();
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if mime.starts_with("application/json") {
        let mut response = response;
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        return response;
    }

    let mut cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let (status, data) = if status.is_redirection() {
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("/");
        let job_ids = response
            .extensions()
            .get::<AcceptedJobs>()
            .map(|jobs| jobs.0.clone())
            .unwrap_or_else(|| {
                response
                    .extensions()
                    .get::<AcceptedJob>()
                    .map(|job| job.0)
                    .into_iter()
                    .collect()
            });
        let job_id = job_ids.first().copied();
        let mut message = None;
        let mut destination = url::Url::parse("http://localhost")
            .ok()
            .and_then(|base| base.join(location).ok());
        if let Some(url) = destination.as_mut() {
            let query: Vec<_> = url
                .query_pairs()
                .filter_map(|(key, value)| {
                    if key == "flash" {
                        message = Some(value.into_owned());
                        None
                    } else {
                        Some((key.into_owned(), value.into_owned()))
                    }
                })
                .collect();
            url.set_query(None);
            if !query.is_empty() {
                url.query_pairs_mut().extend_pairs(query);
            }
        }
        let destination = destination
            .as_ref()
            .filter(|url| {
                url.host_str() == Some("localhost") && super::auth::local_page(location).is_some()
            })
            .map(|url| &url[url::Position::BeforePath..])
            .unwrap_or("/");
        let status = if job_id.is_some() {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        };
        (
            status,
            json!({"location":destination, "message":message, "job_id":job_id,"job_ids":job_ids}),
        )
    } else if status.is_success() && mime.starts_with("text/html") {
        match to_bytes(response.into_body(), 32 * 1024 * 1024).await {
            Ok(body) => (status, json!({"html":String::from_utf8_lossy(&body)})),
            Err(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error":"The page could not be loaded"}),
            ),
        }
    } else {
        let detail = response
            .extensions()
            .get::<ClientError>()
            .map(|error| error.0.clone())
            .unwrap_or_else(|| {
                status
                    .canonical_reason()
                    .unwrap_or("Request failed")
                    .to_string()
            });
        (status, json!({"error":detail}))
    };
    let mut response = (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json::<Value>(data),
    )
        .into_response();
    for cookie in cookies.drain(..) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}
