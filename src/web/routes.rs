//! HTTP handlers.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router, middleware};
use serde_json::{Value, json};

use super::auth;
use super::forms;
use super::forms::{
    ChannelForm, FormMap, ImportForm, LoginForm, PostEdit, PostFilter, QueueAction,
};
use super::views::APP_CSS;
use super::{WebState, auth as web_auth};
use crate::db::{
    channels as channel_db, jobs as job_db, posts as post_db, queue as queue_db, tags as tag_db,
};
use crate::error::Error;
use crate::model::{
    Attribution, Channel, NsfwPolicy, ParseMode, PostState, QueueState, Rating, TagStyle,
};

pub struct WebError(Error);

impl From<Error> for WebError {
    fn from(err: Error) -> Self {
        WebError(err)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        tracing::warn!("web error: {}", self.0);
        let status = match &self.0 {
            Error::Invalid(_) => StatusCode::BAD_REQUEST,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>error</title>\
             <body style=\"font:14px system-ui;padding:2rem;background:#111;color:#eee\">\
             <h1>Something went wrong</h1><pre style=\"white-space:pre-wrap\">{}</pre>\
             <p><a style=\"color:#7dd3fc\" href=\"/\">back</a></p>",
            htmlescape::encode_minimal(&self.0.to_string())
        );
        (status, Html(body)).into_response()
    }
}

type WebResult<T> = std::result::Result<T, WebError>;

pub fn router(ws: Arc<WebState>) -> Router {
    let protected = Router::new()
        .route("/", get(dashboard))
        .route("/posts", get(posts_page).post(posts_bulk))
        .route("/posts/{id}", get(post_page).post(post_update))
        .route("/channels", get(channels_page).post(channel_create))
        .route(
            "/channels/{id}/edit",
            get(channel_edit_page).post(channel_update),
        )
        .route("/channels/{id}/queue", get(queue_page))
        .route("/channels/{id}/preview", get(preview_page))
        .route("/queue/action", post(queue_action))
        .route("/publish", post(publish_action))
        .route("/storage/drain", post(storage_drain))
        .route("/import", get(import_page).post(import_submit))
        .route("/users", get(users_page).post(users_action))
        .route("/logout", post(logout))
        .with_state(ws.clone());

    Router::new()
        .route("/login", get(login_page).post(login_submit))
        .route("/static/app.css", get(css))
        .route(crate::bot::WEBHOOK_PATH, post(webhook))
        .route("/thumb/{hash}", get(thumb))
        .route("/media/{hash}", get(media))
        .merge(protected)
        .layer(middleware::from_fn_with_state(
            ws.clone(),
            auth::require_session,
        ))
        .with_state(ws)
}

fn flash_redirect(path: &str, message: &str) -> Response {
    let encoded: String = message
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | ' ' => {
                if c == ' ' {
                    "+".to_string()
                } else {
                    c.to_string()
                }
            }
            other => format!("%{:02X}", other as u32 & 0xff),
        })
        .collect();
    Redirect::to(&format!("{path}?flash={encoded}")).into_response()
}

fn decode_flash(raw: &Option<String>) -> Option<String> {
    let raw = raw.as_deref()?;
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn check_csrf(ws: &WebState, provided: &str) -> WebResult<()> {
    if auth::constant_time_eq(provided, &ws.csrf) {
        Ok(())
    } else {
        Err(WebError(Error::invalid("invalid form token (stale page?)")))
    }
}

// ---------------------------------------------------------------- auth

async fn login_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Html<String>> {
    let html = ws.pages.render(
        "login.html",
        &json!({
            "page": "login",
            "csrf": ws.csrf,
            "flash": decode_flash(&query.flash),
            "next": "/",
            "home": ws.app.cfg.home.display().to_string(),
        }),
    )?;
    Ok(Html(html))
}

async fn login_submit(State(ws): State<Arc<WebState>>, Form(form): Form<LoginForm>) -> Response {
    let expected = ws.app.cfg.ui_token.clone().unwrap_or_default();
    if auth::constant_time_eq(form.token.trim(), expected.trim()) {
        let mut response = Redirect::to("/").into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            auth::session_cookie(&ws.session).parse().unwrap(),
        );
        response
    } else {
        flash_redirect("/login", "wrong token")
    }
}

async fn logout(State(_ws): State<Arc<WebState>>) -> Response {
    let mut response = Redirect::to("/login").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, auth::clear_cookie().parse().unwrap());
    response
}

async fn css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        APP_CSS.to_string(),
    )
        .into_response()
}

// ---------------------------------------------------------------- webhook

/// Telegram delivery endpoint. Authenticated by the secret header instead of
/// the UI cookie, so only the `[bot] mode = "webhook"` path ever answers here.
async fn webhook(
    State(ws): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(expected) = ws.webhook_secret.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let provided = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !auth::constant_time_eq(provided, expected) {
        tracing::warn!("rejected a webhook call with a bad secret");
        return StatusCode::FORBIDDEN.into_response();
    }
    if ws.app.transport.is_none() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let bot = match crate::bot::Bot::new(&ws.app).await {
        Ok(bot) => bot,
        Err(err) => {
            tracing::error!("webhook: cannot build the bot: {err}");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    match bot.handle_webhook_body(&body).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(err @ Error::Telegram(_)) => {
            // a malformed update will never get better: tell Telegram not to retry
            tracing::warn!("webhook update rejected: {err}");
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(err) => {
            // internal failure: let Telegram redeliver
            tracing::error!("webhook update failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------- media

fn safe_hash(hash: &str) -> bool {
    !hash.is_empty() && hash.len() <= 80 && hash.chars().all(|c| c.is_ascii_alphanumeric())
}

async fn thumb(State(ws): State<Arc<WebState>>, Path(hash): Path<String>) -> Response {
    if !safe_hash(&hash) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(ws.app.blobs.thumb_path(&hash)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "image/webp")], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn media(State(ws): State<Arc<WebState>>, Path(hash): Path<String>) -> Response {
    if !safe_hash(&hash) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let lookup = hash.clone();
    let ext = ws
        .app
        .db
        .call(move |conn| {
            use rusqlite::OptionalExtension;
            Ok(conn
                .query_row("SELECT ext FROM blobs WHERE hash = ?1", [lookup], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?)
        })
        .await
        .ok()
        .flatten();
    let Some(ext) = ext else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match std::fs::read(ws.app.blobs.path_for(&hash, &ext)) {
        Ok(bytes) => {
            let mime = crate::blob::mime_for(&ext).unwrap_or("application/octet-stream");
            ([(header::CONTENT_TYPE, mime)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------- dashboard

async fn dashboard(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let stats = app
        .db
        .call(|conn| {
            let posts: i64 = conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))?;
            let approved: i64 = conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE state = 'approved'",
                [],
                |r| r.get(0),
            )?;
            let queued: i64 = conn.query_row(
                "SELECT COUNT(*) FROM queue WHERE state = 'queued'",
                [],
                |r| r.get(0),
            )?;
            let sent: i64 =
                conn.query_row("SELECT COUNT(*) FROM sends WHERE ok = 1", [], |r| r.get(0))?;
            let unknown: i64 = conn.query_row(
                "SELECT COUNT(*) FROM queue WHERE state = 'unknown'",
                [],
                |r| r.get(0),
            )?;
            Ok(json!({
                "posts": posts, "approved": approved, "queued": queued,
                "sent": sent, "unknown": unknown,
            }))
        })
        .await?;

    let channels = app.db.call(|conn| channel_db::list(conn)).await?;
    let now = chrono::Utc::now();
    let mut channel_views = Vec::new();
    for channel in &channels {
        let channel_id = channel.id;
        let queued = app
            .db
            .call(move |conn| queue_db::count_state(conn, channel_id, QueueState::Queued))
            .await?;
        channel_views.push(json!({
            "id": channel.id,
            "title": channel.title,
            "tg_id": channel.tg_id,
            "queued": queued,
            "interval": channel.interval_minutes,
            "nsfw_policy": channel.nsfw_policy.as_str(),
            "paused": channel.paused,
            "enabled": channel.enabled,
            "failures": channel.consecutive_failures,
            "next": crate::tg::publish::next_slot_hint(channel, now),
            "reason": crate::queue::scheduler::slot_reason(channel, now, 0),
        }));
    }

    let sends = crate::tg::audit::recent(app, 8).await?;
    let send_views: Vec<Value> = sends
        .iter()
        .map(|send| {
            json!({
                "channel_id": send.channel_id,
                "post_id": send.post_id,
                "ok": send.ok,
                "at": send.sent_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
                "ids": send.tg_message_ids,
                "error": send.error,
            })
        })
        .collect();

    let mirrors = app
        .db
        .call(|conn| crate::db::mirrors::summary(conn))
        .await?;
    let mut storage_views: Vec<Value> = Vec::new();
    for sink in crate::storage::sinks(app) {
        let counts: Vec<&(String, String, i64)> = mirrors
            .iter()
            .filter(|(name, _, _)| name == sink.name())
            .collect();
        let get = |state: &str| {
            counts
                .iter()
                .filter(|(_, name, _)| name == state)
                .map(|(_, _, count)| *count)
                .sum::<i64>()
        };
        storage_views.push(json!({
            "name": sink.name(),
            "where": sink.describe(),
            "done": get("done"),
            "pending": get("pending"),
            "failed": get("failed"),
        }));
    }

    let jobs = app.db.call(|conn| job_db::list(conn, 6)).await?;
    let job_views: Vec<Value> = jobs
        .iter()
        .map(|job| {
            json!({
                "id": job.id,
                "kind": job.kind,
                "state": job.state,
                "payload": job.payload,
                "message": job.message,
            })
        })
        .collect();

    let unknown = app
        .db
        .call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT q.id, q.channel_id, q.post_id, q.last_error, c.title
                 FROM queue q JOIN channels c ON c.id = q.channel_id
                 WHERE q.state = 'unknown' ORDER BY q.id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, i64>(0)?,
                    "channel_id": row.get::<_, i64>(1)?,
                    "post_id": row.get::<_, i64>(2)?,
                    "error": row.get::<_, Option<String>>(3)?,
                    "channel": row.get::<_, String>(4)?,
                }))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await?;

    let html = ws.pages.render(
        "dashboard.html",
        &json!({
            "page": "dashboard",
            "csrf": ws.csrf,
            "flash": decode_flash(&query.flash),
            "stats": stats,
            "channels": channel_views,
            "sends": send_views,
            "jobs": job_views,
            "recent_unknown": unknown,
            "storage": storage_views,
        }),
    )?;
    Ok(Html(html))
}

// ---------------------------------------------------------------- posts

async fn posts_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let page = query.page.unwrap_or(1).max(1);
    let per_page = 24;
    let filter = post_db::PostFilter {
        state: query
            .state
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "all")
            .and_then(PostState::try_parse),
        source: query.source.clone().filter(|s| !s.is_empty()),
        tag: query.tag.clone().filter(|s| !s.is_empty()),
        nsfw: match query.nsfw.as_deref() {
            Some("yes") => Some(true),
            Some("no") => Some(false),
            _ => None,
        },
        author_id: None,
        search: query.search.clone().filter(|s| !s.is_empty()),
        limit: per_page,
        offset: (page - 1) * per_page,
    };
    let filter_for_count = post_db::PostFilter {
        limit: 0,
        offset: 0,
        ..filter.clone()
    };
    let cards = app
        .db
        .call(move |conn| post_db::cards(conn, &filter))
        .await?;
    // a real COUNT over the same filter: fetching up to 100k cards just to
    // size the pager was wrong for search/nsfw filters and slow either way
    let total = app
        .db
        .call(move |conn| post_db::count_cards(conn, &filter_for_count))
        .await?;
    let sources = app.db.call(|conn| post_db::sources(conn)).await?;
    let tags = app.db.call(|conn| tag_db::most_used(conn, 30)).await?;
    let channels = app.db.call(|conn| channel_db::list(conn)).await?;

    let cards: Vec<Value> = cards
        .iter()
        .map(|card| {
            json!({
                "id": card.id,
                "source": card.source,
                "rating": card.rating.as_str(),
                "state": card.state.as_str(),
                "artist": card.author_name,
                "media_count": card.media_count,
                "thumb": card.thumb_hash,
                "nsfw": card.nsfw,
                "snippet": card.snippet,
                "pushed": card.pushed,
                "delivered": card.delivered,
                "channels": card.channels,
                "imported": card.imported_at.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string(),
            })
        })
        .collect();

    let html = ws.pages.render(
        "posts.html",
        &json!({
            "page": "posts",
            "csrf": ws.csrf,
            "flash": decode_flash(&query.flash),
            "cards": cards,
            "total": total,
            "page_no": page,
            "has_next": (page * per_page) < total,
            "filters": {
                "state": query.state,
                "source": query.source,
                "tag": query.tag,
                "nsfw": query.nsfw,
                "search": query.search,
            },
            "sources": sources,
            "top_tags": tags.iter().map(|(t, n)| json!({"tag": t, "count": n})).collect::<Vec<_>>(),
            "channels": channels.iter().map(|c| json!({"id": c.id, "title": c.title})).collect::<Vec<_>>(),
            "states": PostState::all().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        }),
    )?;
    Ok(Html(html))
}

async fn posts_bulk(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let ids = forms::selected_ids(&form);
    if ids.is_empty() {
        return Ok(flash_redirect("/posts", "no posts selected"));
    }
    let action = forms::field(&form, "action")
        .unwrap_or_default()
        .to_string();
    let channel_id = forms::field(&form, "channel").and_then(|raw| raw.parse::<i64>().ok());
    let app = &ws.app;
    let note = match action.as_str() {
        "approve" | "reject" => {
            let state = if action == "approve" {
                PostState::Approved
            } else {
                PostState::Rejected
            };
            let ids_for_db = ids.clone();
            app.db
                .call(move |conn| {
                    for id in &ids_for_db {
                        post_db::set_state(conn, *id, state)?;
                    }
                    Ok(())
                })
                .await?;
            format!("{} post(s) -> {}", ids.len(), state)
        }
        "nsfw_yes" | "nsfw_no" | "nsfw_auto" => {
            let value = match action.as_str() {
                "nsfw_yes" => Some(true),
                "nsfw_no" => Some(false),
                _ => None,
            };
            let ids_for_db = ids.clone();
            app.db
                .call(move |conn| {
                    for id in &ids_for_db {
                        post_db::set_nsfw_override(conn, *id, value)?;
                    }
                    Ok(())
                })
                .await?;
            format!("nsfw override updated on {} post(s)", ids.len())
        }
        "queue" => {
            let Some(channel_id) = channel_id else {
                return Ok(flash_redirect("/posts", "choose a channel first"));
            };
            let channel = app
                .db
                .call(move |conn| channel_db::get(conn, channel_id))
                .await?;
            let summary = crate::queue::enqueue_many(app, &channel, &ids, 0).await;
            let mut note = format!("{} into {}", summary.describe(), channel.title);
            // name the policy reasons instead of only counting them
            let reasons = summary.skip_reasons();
            if !reasons.is_empty() {
                note.push_str(&format!(" ({})", reasons.join("; ")));
            }
            note
        }
        "delete" => {
            let ids_for_db = ids.clone();
            app.db
                .call(move |conn| {
                    for id in &ids_for_db {
                        post_db::delete(conn, *id)?;
                    }
                    Ok(())
                })
                .await?;
            format!("{} post(s) deleted", ids.len())
        }
        other => format!("unknown action {other}"),
    };
    Ok(flash_redirect("/posts", &note))
}

async fn post_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let post = app.db.call(move |conn| post_db::get(conn, id)).await?;
    let channels = app.db.call(|conn| channel_db::list(conn)).await?;
    let rules = app.db.call(|conn| tag_db::TagRules::load(conn)).await?;
    let submitted_by = app
        .db
        .call(move |conn| {
            use rusqlite::OptionalExtension;
            Ok(conn
                .query_row(
                    "SELECT submitted_by FROM posts WHERE id = ?1",
                    [id],
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten())
        })
        .await?;

    let mut captions = Vec::new();
    for channel in &channels {
        let decision = crate::nsfw::evaluate(&post, channel, &rules);
        let preview = match crate::tg::publish::build_plan(app, channel, &post, 0, decision.clone())
        {
            Ok(plan) => json!({
                "channel": channel.title,
                "tg_id": channel.tg_id,
                "caption": plan.caption.text,
                "length": plan.caption.length,
                "truncated": plan.caption.truncated,
                "messages": plan.messages.len(),
                "verdict": plan.decision.reason,
                "allowed": decision.is_allowed(),
                "spoiler": decision.spoiler(),
            }),
            Err(err) => json!({
                "channel": channel.title,
                "tg_id": channel.tg_id,
                "caption": format!("template error: {err}"),
                "length": 0,
                "truncated": false,
                "messages": 0,
                "verdict": err.to_string(),
                "allowed": false,
                "spoiler": false,
            }),
        };
        captions.push(preview);
    }

    let media: Vec<Value> = post
        .media
        .iter()
        .map(|item| {
            json!({
                "hash": item.blob_hash,
                "thumb": item.poster_hash.clone().unwrap_or_else(|| item.blob_hash.clone()),
                "ext": item.ext,
                "is_video": item.is_video,
                "width": item.width,
                "height": item.height,
                "kib": (item.bytes as f64 / 1024.0).round(),
            })
        })
        .collect();

    let html = ws.pages.render(
        "post_detail.html",
        &json!({
            "page": "posts",
            "csrf": ws.csrf,
            "flash": Value::Null,
            "post": {
                "id": post.id,
                "source": post.source,
                "source_id": post.source_id,
                "url": post.url,
                "title": post.title,
                "description": post.description,
                "artist": post.author.as_ref().map(|a| a.display()),
                "artist_url": post.author.as_ref().and_then(|a| a.url.clone()),
                "rating": post.rating.as_str(),
                "state": post.state.as_str(),
                "nsfw_override": match post.nsfw_override { Some(true) => "yes", Some(false) => "no", None => "auto" },
                "nsfw": post.is_nsfw(rules.hits_nsfw(&post.tags)),
                "tags": post.tags,
                "notes": post.notes,
                "imported": post.imported_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
            },
            "submitted_by": submitted_by,
            "media": media,
            "captions": captions,
            "ratings": Rating::all().iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "states": PostState::all().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "channels": channels.iter().map(|c| json!({"id": c.id, "title": c.title})).collect::<Vec<_>>(),
        }),
    )?;
    Ok(Html(html))
}

async fn post_update(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<PostEdit>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let app = &ws.app;
    if let Some(rating) = form.rating.as_deref().filter(|r| !r.is_empty()) {
        let rating = rating
            .parse::<Rating>()
            .map_err(|_| WebError(Error::invalid("bad rating")))?;
        app.db
            .call(move |conn| post_db::set_rating(conn, id, rating))
            .await?;
    }
    if let Some(nsfw) = form.nsfw.as_deref() {
        let value = match nsfw {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        };
        app.db
            .call(move |conn| post_db::set_nsfw_override(conn, id, value))
            .await?;
    }
    if let Some(state) = form.state.as_deref().filter(|s| !s.is_empty())
        && let Some(state) = PostState::try_parse(state)
    {
        app.db
            .call(move |conn| post_db::set_state(conn, id, state))
            .await?;
    }
    if let Some(tags) = form.tags.as_deref() {
        let tags: Vec<String> = tags
            .split([',', ' ', '\n'])
            .map(|t| t.trim().trim_start_matches('#').to_string())
            .filter(|t| !t.is_empty())
            .collect();
        app.db
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM post_tags WHERE post_id = ?1 AND origin = 'manual'",
                    [id],
                )?;
                for tag in &tags {
                    tag_db::link(conn, id, tag, "manual")?;
                }
                Ok(())
            })
            .await?;
    }
    if let Some(notes) = form.notes.as_deref() {
        let notes = (!notes.trim().is_empty()).then(|| notes.trim().to_string());
        app.db
            .call(move |conn| post_db::set_notes(conn, id, notes.as_deref()))
            .await?;
    }
    Ok(flash_redirect(&format!("/posts/{id}"), "saved"))
}

// ---------------------------------------------------------------- channels

async fn channels_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let channels = app.db.call(|conn| channel_db::list(conn)).await?;
    let now = chrono::Utc::now();
    let mut views = Vec::new();
    for channel in &channels {
        let channel_id = channel.id;
        let queued = app
            .db
            .call(move |conn| queue_db::count_state(conn, channel_id, QueueState::Queued))
            .await?;
        views.push(json!({
            "id": channel.id,
            "alias": channel.alias,
            "discovered": channel.discovered,
            "title": channel.title,
            "tg_id": channel.tg_id,
            "queued": queued,
            "interval": channel.interval_minutes,
            "nsfw_policy": channel.nsfw_policy.as_str(),
            "tag_style": channel.tag_style.as_str(),
            "attribution": channel.attribution.as_str(),
            "active": format!("{} - {} ({})", channel.active_from, channel.active_to, channel.tz),
            "paused": channel.paused,
            "enabled": channel.enabled,
            "next": crate::tg::publish::next_slot_hint(channel, now),
        }));
    }
    let html = ws.pages.render(
        "channels.html",
        &json!({
            "page": "channels",
            "csrf": ws.csrf,
            "flash": decode_flash(&query.flash),
            "channels": views,
            "tz_hint": "Europe/Berlin, UTC, America/New_York ...",
            "templates": app.renderer.template_names(),
        }),
    )?;
    Ok(Html(html))
}

fn channel_from_form(mut channel: Channel, form: &ChannelForm) -> Result<Channel, Error> {
    if let Some(alias) = form
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        channel.alias = crate::model::slugify(alias);
    } else if channel.alias.is_empty() {
        channel.alias = crate::model::slugify(&form.tg_id);
    }
    channel.title = form.title.trim().to_string();
    channel.tg_id = form.tg_id.trim().to_string();
    if channel.title.is_empty() || channel.tg_id.is_empty() {
        return Err(Error::invalid("title and channel id are required"));
    }
    if let Some(interval) = form.interval_minutes {
        channel.interval_minutes = interval.max(1);
    }
    if let Some(from) = form.active_from.as_deref().filter(|s| !s.is_empty()) {
        crate::queue::scheduler::parse_hhmm(from)?;
        channel.active_from = from.to_string();
    }
    if let Some(to) = form.active_to.as_deref().filter(|s| !s.is_empty()) {
        crate::queue::scheduler::parse_hhmm(to)?;
        channel.active_to = to.to_string();
    }
    if let Some(tz) = form.tz.as_deref().filter(|s| !s.is_empty()) {
        tz.parse::<chrono_tz::Tz>()
            .map_err(|_| Error::invalid(format!("unknown timezone: {tz}")))?;
        channel.tz = tz.to_string();
    }
    if let Some(cap) = form.daily_cap {
        channel.daily_cap = cap.max(0);
    }
    if let Some(jitter) = form.jitter_secs {
        channel.jitter_secs = jitter.max(0);
    }
    if let Some(cooldown) = form.artist_cooldown {
        channel.artist_cooldown = cooldown.max(0);
    }
    if let Some(policy) = form.nsfw_policy.as_deref().filter(|s| !s.is_empty()) {
        channel.nsfw_policy = policy.parse::<NsfwPolicy>()?;
    }
    if let Some(min_media) = form.min_media {
        channel.min_media = min_media.max(0);
    }
    if let Some(tags) = &form.blocked_tags {
        channel.blocked_tags = crate::model::join_csv(crate::model::split_csv(tags));
    }
    if let Some(tags) = &form.required_tags {
        channel.required_tags = crate::model::join_csv(crate::model::split_csv(tags));
    }
    if let Some(style) = form.tag_style.as_deref().filter(|s| !s.is_empty()) {
        channel.tag_style = style.parse::<TagStyle>()?;
    }
    if let Some(max_tags) = form.max_tags {
        channel.max_tags = max_tags.max(0);
    }
    if let Some(attribution) = form.attribution.as_deref().filter(|s| !s.is_empty()) {
        channel.attribution = attribution.parse::<Attribution>()?;
    }
    if let Some(name) = form.template_name.as_deref().filter(|s| !s.is_empty()) {
        channel.template_name = name.to_string();
    }
    let template = form.template.clone().unwrap_or_default();
    channel.template = (!template.trim().is_empty()).then_some(template);
    channel.silent = ChannelForm::flag(&form.silent);
    channel.protect = ChannelForm::flag(&form.protect);
    channel.include_source_tag = ChannelForm::flag(&form.include_source_tag);
    channel.enabled = ChannelForm::flag(&form.enabled);
    Ok(channel)
}

async fn channel_create(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<ChannelForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let channel = channel_from_form(Channel::new("", ""), &form)?;
    if let Some(template) = &channel.template {
        ws.app.renderer.validate_template(template)?;
    }
    let to_insert = channel.clone();
    let id = ws
        .app
        .db
        .call(move |conn| channel_db::insert(conn, &to_insert))
        .await?;
    Ok(flash_redirect(
        &format!("/channels/{id}/edit"),
        &format!("channel #{} created", id),
    ))
}

async fn channel_edit_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let channel = app.db.call(move |conn| channel_db::get(conn, id)).await?;
    let preview = crate::tg::publish::preview_channel(app, &channel, 1).await?;
    let html = ws.pages.render(
        "channel_edit.html",
        &json!({
            "page": "channels",
            "csrf": ws.csrf,
            "flash": Value::Null,
            "channel": {
                "id": channel.id,
                "alias": channel.alias,
                "discovered": channel.discovered,
                "title": channel.title,
                "tg_id": channel.tg_id,
                "enabled": channel.enabled,
                "interval_minutes": channel.interval_minutes,
                "active_from": channel.active_from,
                "active_to": channel.active_to,
                "tz": channel.tz,
                "daily_cap": channel.daily_cap,
                "jitter_secs": channel.jitter_secs,
                "artist_cooldown": channel.artist_cooldown,
                "nsfw_policy": channel.nsfw_policy.as_str(),
                "min_media": channel.min_media,
                "blocked_tags": channel.blocked_tags,
                "required_tags": channel.required_tags,
                "tag_style": channel.tag_style.as_str(),
                "max_tags": channel.max_tags,
                "attribution": channel.attribution.as_str(),
                "parse_mode": channel.parse_mode.as_str(),
                "template_name": channel.template_name,
                "template": channel.template,
                "silent": channel.silent,
                "protect": channel.protect,
                "include_source_tag": channel.include_source_tag,
                "paused": channel.paused,
                "failures": channel.consecutive_failures,
            },
            "policies": NsfwPolicy::all().iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "tag_styles": TagStyle::all().iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "attributions": Attribution::all().iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "parse_modes": ParseMode::all().iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "templates": app.renderer.template_names(),
            "default_template": app.renderer.template_source("default").unwrap_or_default(),
            "sample": preview.first().map(|item| json!({
                "caption": item.caption, "reason": item.reason, "media": item.media,
                "messages": item.messages, "spoiler": item.spoiler,
            })),
        }),
    )?;
    Ok(Html(html))
}

async fn channel_update(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<ChannelForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let app = &ws.app;
    let existing = app.db.call(move |conn| channel_db::get(conn, id)).await?;
    let mut channel = channel_from_form(existing.clone(), &form)?;
    channel.id = id;
    if let Some(template) = &channel.template {
        app.renderer.validate_template(template)?;
    }
    let to_update = channel.clone();
    app.db
        .call(move |conn| channel_db::update(conn, &to_update))
        .await?;
    Ok(flash_redirect(&format!("/channels/{id}/edit"), "saved"))
}

async fn queue_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let channel = app.db.call(move |conn| channel_db::get(conn, id)).await?;
    let channel_id = channel.id;
    let entries = app
        .db
        .call(move |conn| {
            queue_db::entries(
                conn,
                channel_id,
                &[
                    QueueState::Queued,
                    QueueState::Unknown,
                    QueueState::Failed,
                    QueueState::Sending,
                ],
                200,
            )
        })
        .await?;
    // nothing to load here any more: `card.nsfw` already carries the override

    let items: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let post = entry.item.post_id;
            json!({
                "id": entry.item.id,
                "post_id": post,
                "state": entry.item.state.as_str(),
                "priority": entry.item.priority,
                "pinned": entry.item.pinned,
                "attempts": entry.item.attempts,
                "error": entry.item.last_error,
                "artist": entry.card.author_name,
                "snippet": entry.card.snippet,
                "thumb": entry.card.thumb_hash,
                "media_count": entry.card.media_count,
                "nsfw": entry.card.nsfw,
                "added": entry.item.added_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
            })
        })
        .collect();

    let html = ws.pages.render(
        "queue.html",
        &json!({
            "page": "channels",
            "csrf": ws.csrf,
            "flash": Value::Null,
            "channel": {"id": channel.id, "title": channel.title, "tg_id": channel.tg_id},
            "items": items,
        }),
    )?;
    Ok(Html(html))
}

async fn queue_action(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<QueueAction>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let app = &ws.app;
    let id = form.id;
    let channel_id = form.channel;
    let note = match form.action.as_str() {
        "skip" => {
            app.db.call(move |conn| queue_db::skip(conn, id)).await?;
            "skipped".to_string()
        }
        "pin" | "unpin" => {
            let pinned = form.action == "pin";
            app.db
                .call(move |conn| queue_db::set_pinned(conn, id, pinned))
                .await?;
            (if pinned { "pinned" } else { "unpinned" }).to_string()
        }
        "reset" => {
            app.db.call(move |conn| queue_db::requeue(conn, id)).await?;
            "back in the queue".to_string()
        }
        "remove" => {
            app.db.call(move |conn| queue_db::delete(conn, id)).await?;
            "removed".to_string()
        }
        "delivered" => {
            // confirm an unconfirmed send: mark it sent without touching the API
            app.db
                .call(move |conn| queue_db::resolve_unknown(conn, id, true, None))
                .await?;
            "marked as delivered".to_string()
        }
        "not_delivered" => {
            app.db
                .call(move |conn| queue_db::resolve_unknown(conn, id, false, None))
                .await?;
            "marked as not delivered, re-queued".to_string()
        }
        other => format!("unknown action {other}"),
    };
    match channel_id {
        Some(channel_id) => Ok(flash_redirect(
            &format!("/channels/{channel_id}/queue"),
            &note,
        )),
        None => Ok(flash_redirect("/", &note)),
    }
}

async fn preview_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let channel = app.db.call(move |conn| channel_db::get(conn, id)).await?;
    let items = crate::tg::publish::preview_channel(app, &channel, 10).await?;
    let html = ws.pages.render(
        "preview.html",
        &json!({
            "page": "channels",
            "csrf": ws.csrf,
            "flash": Value::Null,
            "channel": {"id": channel.id, "title": channel.title, "tg_id": channel.tg_id},
            "items": items.iter().enumerate().map(|(index, item)| json!({
                "slot": index + 1,
                "queue_id": item.queue_id,
                "post_id": item.post_id,
                "title": item.title,
                "caption": item.caption,
                "reason": item.reason,
                "media": item.media,
                "messages": item.messages,
                "spoiler": item.spoiler,
                "nsfw": item.nsfw,
            })).collect::<Vec<_>>(),
        }),
    )?;
    Ok(Html(html))
}

// ---------------------------------------------------------------- storage

async fn storage_drain(State(ws): State<Arc<WebState>>) -> Response {
    match crate::storage::drain(&ws.app, 25).await {
        Ok(uploaded) => flash_redirect("/", &format!("{uploaded} mirror(s) uploaded")),
        Err(err) => flash_redirect("/", &format!("storage failed: {err}")),
    }
}

// ---------------------------------------------------------------- publish

/// "Publish now": either the next queued post of a channel, or one specific
/// queue item. Deliberately bypasses the schedule (admins only — the UI is
/// token-protected); the NSFW policy and the lease still apply.
async fn publish_action(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let app = &ws.app;
    let back = forms::field(&form, "back")
        .filter(|path| path.starts_with('/'))
        .unwrap_or("/")
        .to_string();
    let queue_id = forms::field(&form, "queue_id").and_then(|raw| raw.parse::<i64>().ok());
    let channel_id = forms::field(&form, "channel_id").and_then(|raw| raw.parse::<i64>().ok());

    let outcome = if let Some(queue_id) = queue_id {
        crate::tg::publish::publish_item(app, queue_id, false).await
    } else if let Some(channel_id) = channel_id {
        let channel = app
            .db
            .call(move |conn| channel_db::get(conn, channel_id))
            .await;
        match channel {
            Ok(channel) => crate::tg::publish::publish_channel(app, &channel, false, true).await,
            Err(err) => Err(err),
        }
    } else {
        Err(Error::invalid("nothing selected to publish"))
    };

    let note = match outcome {
        Ok(outcome) => outcome.describe(),
        Err(err) => format!("publish failed: {err}"),
    };
    Ok(flash_redirect(&back, &note))
}

// ---------------------------------------------------------------- import

async fn import_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let jobs = app.db.call(|conn| job_db::list(conn, 15)).await?;
    let channels = app.db.call(|conn| channel_db::list(conn)).await?;
    let html = ws.pages.render(
        "import.html",
        &json!({
            "page": "import",
            "csrf": ws.csrf,
            "flash": decode_flash(&query.flash),
            "jobs": jobs.iter().map(|job| json!({
                "id": job.id,
                "kind": job.kind,
                "state": job.state,
                "payload": job.payload,
                "message": job.message,
                "created": job.created_at.with_timezone(&chrono::Local).format("%H:%M:%S").to_string(),
            })).collect::<Vec<_>>(),
            "channels": channels.iter().map(|c| json!({"id": c.id, "title": c.title})).collect::<Vec<_>>(),
            "cookies": app.cfg.cookies_dir().display().to_string(),
        }),
    )?;
    Ok(Html(html))
}

async fn import_submit(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<ImportForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let targets = form.target_list();
    if targets.is_empty() {
        return Ok(flash_redirect("/import", "no targets given"));
    }
    let app = &ws.app;
    let mut created = 0;
    for target in &targets {
        crate::jobs::create_job(app, target).await?;
        created += 1;
    }
    let channel = match form.channel {
        Some(channel_id) if channel_id > 0 => app
            .db
            .call(move |conn| channel_db::get(conn, channel_id))
            .await
            .ok(),
        _ => None,
    };
    let note = match channel {
        Some(channel) => format!(
            "{created} import job(s) queued - after they finish, queue posts from /posts into {}",
            channel.title
        ),
        None => format!("{created} import job(s) queued - the worker picks them up within 2s"),
    };
    Ok(flash_redirect("/import", &note))
}

// ---------------------------------------------------------------- users

async fn users_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Html<String>> {
    let app = &ws.app;
    let users = app
        .db
        .call(|conn| crate::db::users::list(conn, 200))
        .await?;
    let mut views = Vec::new();
    for user in &users {
        let tg_id = user.tg_id;
        let (archived, pushed, sent) = app
            .db
            .call(move |conn| {
                let archived = post_db::count_submitted(conn, tg_id)?;
                let (pushed, sent) = crate::db::queue::counts_submitted(conn, tg_id)?;
                Ok((archived, pushed, sent))
            })
            .await?;
        let config_admin = app.cfg.access.is_admin(user.tg_id);
        // config wins at runtime, so show the role the bot will actually use
        let role = if config_admin {
            "admin"
        } else {
            user.role.as_str()
        };
        views.push(json!({
            "tg_id": user.tg_id,
            "label": user.label(),
            "role": role,
            "db_role": user.role,
            "archived": archived,
            "pushed": pushed,
            "sent": sent,
            "config_admin": config_admin,
            "last_seen": user.last_seen_at
                .as_deref()
                .and_then(crate::db::parse_ts)
                .map(|ts| ts.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default(),
        }));
    }
    let html = ws.pages.render(
        "users.html",
        &json!({
            "page": "users",
            "csrf": ws.csrf,
            "flash": decode_flash(&query.flash),
            "users": views,
        }),
    )?;
    Ok(Html(html))
}

async fn users_action(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let Some(tg_id) = forms::field(&form, "tg_id").and_then(|raw| raw.parse::<i64>().ok()) else {
        return Ok(flash_redirect("/users", "missing user id"));
    };
    let role = forms::field(&form, "role").unwrap_or("guest").to_string();
    if !matches!(role.as_str(), "admin" | "guest" | "banned") {
        return Ok(flash_redirect("/users", "unknown role"));
    }
    if role != "admin" && ws.app.cfg.access.is_admin(tg_id) {
        return Ok(flash_redirect(
            "/users",
            "that id is an admin in config.toml - edit the config to demote them",
        ));
    }
    let role_for_db = role.clone();
    ws.app
        .db
        .call(move |conn| crate::db::users::set_role(conn, tg_id, &role_for_db))
        .await?;
    Ok(flash_redirect("/users", &format!("{tg_id} -> {role}")))
}

#[allow(dead_code)]
fn unused() -> &'static str {
    web_auth::COOKIE
}
