//! HTTP handlers.

use std::{collections::HashMap, sync::Arc};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router, middleware};
use serde_json::{Value, json};

use super::WebState;
use super::admin::{accepted, action_result};
use super::auth;
use super::data::*;
use super::forms;
use super::forms::{
    ChannelForm, FormMap, ImportForm, LoginForm, POST_PAGE_SIZES, PostDelete, PostDetailQuery,
    PostEdit, PostFilter, PostTagAction, QueueAction, post_page_size,
};
use crate::db::{
    channels as channel_db, jobs as job_db, posts as post_db, queue as queue_db, tags as tag_db,
};
use crate::error::Error;
use crate::model::{
    Attribution, Channel, NsfwPolicy, ParseMode, PostState, QueueState, Rating, TagStyle,
};
use crate::state::AppState;

pub struct WebError(Error);

impl From<Error> for WebError {
    fn from(err: Error) -> Self {
        WebError(err)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            Error::Invalid(message) => (StatusCode::BAD_REQUEST, message.to_string()),
            Error::NotFound(message) => (StatusCode::NOT_FOUND, message.to_string()),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not complete this request".into(),
            ),
        };
        if status.is_server_error() {
            tracing::warn!("web request failed: {}", self.0);
        }
        super::admin::error(status, message)
    }
}

pub(super) type WebResult<T> = std::result::Result<T, WebError>;

pub fn router(ws: Arc<WebState>) -> Router {
    let api = Router::new()
        .route("/dashboard", get(dashboard))
        .route("/posts", get(posts_page).post(posts_bulk))
        .route("/posts/{id}", get(post_page).post(post_update))
        .route(
            "/posts/{id}/delete",
            get(post_delete_page).post(post_delete),
        )
        .route("/posts/{id}/tags", post(post_tag_action))
        .route("/tags/autocomplete", get(tag_autocomplete))
        .route("/posts/{id}/queue", post(post_queue))
        .route("/posts/{id}/media", post(super::media_merges::action))
        .route(
            "/posts/{id}/tag-suggestions",
            post(super::tag_suggestions::action),
        )
        .route(
            "/watches",
            get(super::curation::watches_page).post(super::curation::create_watch),
        )
        .route("/watches/{id}/check", post(super::curation::check_watch))
        .route("/watches/{id}/toggle", post(super::curation::toggle_watch))
        .route("/watches/{id}/remove", post(super::curation::remove_watch))
        .route("/duplicates", get(super::curation::duplicates_page))
        .route("/duplicates/dismiss", post(super::curation::dismiss))
        .route("/duplicates/retry", post(super::curation::retry_duplicates))
        .route("/channels", get(channels_page).post(channel_create))
        .route(
            "/channels/{id}/edit",
            get(channel_edit_page).post(channel_update),
        )
        .route("/channels/{id}/queue", get(queue_page))
        .route("/channels/{id}/flush", post(flush_action))
        .route("/channels/{id}/schedule", post(channel_schedule))
        .route(
            "/channels/{id}/preview",
            get(preview_page).post(super::setup::start_scheduling),
        )
        .route("/setup/queue", post(super::setup::queue_post))
        .route("/queue/action", post(queue_action))
        .route("/publish", post(publish_action))
        .route("/storage/drain", post(storage_drain))
        .route("/storage/cleanup", get(cleanup_preview).post(cleanup_apply))
        .route("/jobs", get(jobs_page))
        .route("/jobs/retry", post(jobs_retry))
        .route("/import", get(import_page).post(import_submit))
        .route("/users", get(users_page).post(users_action))
        .route("/settings", get(settings_page).post(settings_save))
        .route("/logout", post(logout))
        .route("/session", get(session))
        .route("/login", get(login_challenge).post(login_submit))
        .route("/posts/cards", get(posts_more))
        .route("/jobs/{id}/status", get(super::admin::job))
        .route("/channels/{id}/queue/status", get(super::queue_status::get))
        .fallback(|| async {
            super::admin::error(StatusCode::NOT_FOUND, "Admin endpoint not found")
        })
        .layer(middleware::from_fn(super::admin::api_headers));

    Router::new()
        .nest("/api/v1", api)
        .route("/api/status", get(api_status))
        .route("/admin-assets/assets/{name}", get(super::admin::asset))
        .route("/metrics", get(metrics_endpoint))
        .route(crate::bot::WEBHOOK_PATH, post(webhook))
        .route("/thumb/{hash}", get(thumb))
        .route("/media/{hash}", get(media))
        .layer(middleware::from_fn_with_state(
            ws.clone(),
            auth::require_session,
        ))
        .layer(middleware::from_fn(super::admin::navigation))
        .with_state(ws)
}

pub(super) fn check_csrf(ws: &WebState, provided: &str) -> WebResult<()> {
    if auth::constant_time_eq(provided, &ws.csrf) {
        Ok(())
    } else {
        Err(WebError(Error::invalid("invalid form token (stale page?)")))
    }
}

// ---------------------------------------------------------------- auth

#[derive(serde::Serialize)]
struct SessionView<'a> {
    csrf: &'a str,
}

async fn session(State(ws): State<Arc<WebState>>) -> Response {
    Json(SessionView { csrf: &ws.csrf }).into_response()
}

const LOGIN_COOKIE: &str = "nujijushi_login";

async fn login_challenge() -> Response {
    let csrf = crate::config::random_token();
    let cookie = format!(
        "{LOGIN_COOKIE}={csrf}; Path=/api/v1/login; HttpOnly; SameSite=Strict; Max-Age=600"
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(SessionView { csrf: &csrf }),
    )
        .into_response()
}

async fn login_submit(
    State(ws): State<Arc<WebState>>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    if form.csrf.is_empty()
        || !auth::read_cookie(&headers, LOGIN_COOKIE)
            .is_some_and(|expected| auth::constant_time_eq(&form.csrf, expected))
    {
        return super::admin::error(StatusCode::FORBIDDEN, "Sign-in expired. Try again.");
    }
    let expected = ws.app.cfg.ui_token.as_deref().unwrap_or_default();
    if !auth::constant_time_eq(form.token.trim(), expected.trim()) {
        return super::admin::error(StatusCode::UNAUTHORIZED, "Wrong UI token");
    }
    let target = form
        .next
        .as_deref()
        .filter(|path| auth::local_page(path).is_some())
        .unwrap_or("/");
    let mut response = action_result(target, "Signed in");
    if let Ok(cookie) = HeaderValue::from_str(&auth::session_cookie(&ws.session)) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "nujijushi_login=; Path=/api/v1/login; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

async fn logout(State(ws): State<Arc<WebState>>, Form(form): Form<FormMap>) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let mut response = action_result("/login", "");
    if let Ok(cookie) = HeaderValue::from_str(&auth::clear_cookie()) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

// ---------------------------------------------------------------- metrics

/// Prometheus scrape endpoint. It sits outside the cookie gate because a scraper
/// has no session: it exposes counts only, never titles, captions or tokens.
async fn metrics_endpoint(State(ws): State<Arc<WebState>>) -> Response {
    match crate::metrics::render(&ws.app).await {
        Ok(body) => (
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response(),
        Err(err) => {
            tracing::error!("metrics render failed: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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

const THUMB_CACHE: &str = "private, max-age=86400";

async fn thumb(
    State(ws): State<Arc<WebState>>,
    Path(hash): Path<String>,
    request_headers: HeaderMap,
) -> Response {
    let missing = (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "no-store")]);
    if !safe_hash(&hash) {
        return missing.into_response();
    }
    let Ok(bytes) = tokio::fs::read(ws.app.blobs.thumb_path(&hash)).await else {
        return missing.into_response();
    };
    // The URL identifies the source blob. Validate the actual WebP in case a
    // thumbnail is regenerated with a different encoder or size.
    let tag = blake3::hash(&bytes).to_hex();
    let response = if thumbnail_matches(&request_headers, tag.as_str()) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, "image/webp")], bytes).into_response()
    };
    (
        [
            (header::CACHE_CONTROL, THUMB_CACHE),
            (header::VARY, "Cookie"),
        ],
        [(header::ETAG, format!("\"{tag}\""))],
        response,
    )
        .into_response()
}

fn thumbnail_matches(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            // Compare the opaque part of each entity tag, including weak tags.
            // A quoted value may itself contain commas.
            value.trim() == "*"
                || value
                    .split_inclusive('"')
                    .skip(1)
                    .step_by(2)
                    .any(|candidate| candidate.strip_suffix('"') == Some(tag))
        })
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

async fn dashboard(State(ws): State<Arc<WebState>>) -> WebResult<Json<DashboardPage>> {
    let app = &ws.app;
    let (stats, posts, delivered) = app
        .db
        .call(|conn| {
            let posts = post_db::count(conn, None)?;
            let stats = DashboardStats {
                posts,
                approved: post_db::count(conn, Some(PostState::Approved))?,
                queued: queue_db::count_all(conn, QueueState::Queued)?,
                sent: queue_db::count_successful_sends(conn)?,
                unknown: queue_db::count_all(conn, QueueState::Unknown)?,
            };
            Ok((stats, posts, queue_db::has_confirmed_delivery(conn)?))
        })
        .await?;

    let channels = app.db.call(|conn| channel_db::list(conn)).await?;
    let now = chrono::Utc::now();
    let mut channel_views = Vec::new();
    let mut setup_channels = Vec::new();
    for channel in &channels {
        let channel_id = channel.id;
        let since = crate::queue::scheduler::local_day_start(channel, now);
        let (counts, sent_today) = app
            .db
            .call(move |conn| {
                Ok((
                    queue_db::counts(conn, channel_id)?,
                    queue_db::sends_since(conn, channel_id, since)?,
                ))
            })
            .await?;
        let count = |state: QueueState| {
            counts
                .iter()
                .find(|(name, _)| name == state.as_str())
                .map_or(0, |(_, n)| *n)
        };
        let queued = count(QueueState::Queued);
        setup_channels.push(super::setup::ChannelProgress {
            id: channel.id,
            active: channel.enabled && !channel.paused,
            queued: queued > 0,
            held: count(QueueState::Held) > 0,
            sending: count(QueueState::Sending) > 0,
            sent: count(QueueState::Sent) > 0,
        });
        // when the next post would go out, if the scheduler is running
        let next_at = crate::queue::scheduler::upcoming_slots(channel, now, sent_today, 1)
            .into_iter()
            .next()
            .map(|slot| crate::tg::publish::slot_label(slot, now));
        channel_views.push(DashboardChannel {
            publishing: count(QueueState::Sending) > 0,
            id: channel.id,
            title: channel.title.clone(),
            tg_id: channel.tg_id.clone(),
            queued,
            interval: channel.interval_minutes,
            nsfw_policy: channel.nsfw_policy.as_str(),
            paused: channel.paused,
            enabled: channel.enabled,
            failures: channel.consecutive_failures,
            next: crate::tg::publish::next_slot_hint(channel, now),
            next_at,
            reason: crate::queue::scheduler::slot_reason(channel, now, sent_today),
        });
    }

    let sends = crate::tg::audit::recent(app, 8).await?;
    let send_views: Vec<SendView> = sends
        .iter()
        .map(|send| SendView {
            channel_id: send.channel_id,
            post_id: send.post_id,
            ok: send.ok,
            at: send
                .sent_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            ids: send.tg_message_ids.clone(),
            error: send.error.clone(),
        })
        .collect();

    let mirrors = app
        .db
        .call(|conn| crate::db::mirrors::summary(conn))
        .await?;
    let mut storage_views: Vec<SinkView> = Vec::new();
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
        storage_views.push(SinkView {
            name: sink.name().to_string(),
            location: sink.describe(),
            done: get("done"),
            pending: get("pending"),
            failed: get("failed"),
            blocked: get("blocked"),
        });
    }

    let jobs = app.db.call(|conn| job_db::list(conn, 6)).await?;
    let job_views: Vec<JobView> = jobs.iter().map(|job| job_view(job, now)).collect();

    let unknown = app
        .db
        .call(|conn| queue_db::unconfirmed(conn, None))
        .await?;

    let scheduler = scheduler_view(app, now).await?;
    let setup = super::setup::view(
        &app.cfg,
        posts > 0,
        delivered,
        &setup_channels,
        !scheduler.stale,
    );
    Ok(Json(DashboardPage {
        stats,
        channels: channel_views,
        sends: send_views,
        jobs: job_views,
        recent_unknown: unknown,
        storage: storage_views,
        scheduler,
        setup,
    }))
}

// ---------------------------------------------------------------- posts

#[derive(serde::Deserialize)]
struct TagAutocompleteQuery {
    q: Option<String>,
    post_id: Option<i64>,
}

#[derive(serde::Serialize)]
struct TagMatch {
    name: String,
    count: i64,
}

async fn tag_autocomplete(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<TagAutocompleteQuery>,
) -> WebResult<Json<Vec<TagMatch>>> {
    let Some(q) = query.q.filter(|q| !q.trim().is_empty()) else {
        return Ok(Json(Vec::new()));
    };
    let tags = ws
        .app
        .db
        .call(move |conn| tag_db::autocomplete(conn, &q, query.post_id))
        .await?;
    Ok(Json(
        tags.into_iter()
            .map(|(name, count)| TagMatch { name, count })
            .collect(),
    ))
}

async fn posts_page(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Json<PostsPage>> {
    let app = &ws.app;
    let per_page = query
        .per_page
        .as_deref()
        .and_then(post_page_size)
        .unwrap_or(POST_PAGE_SIZES[0]);
    let requested_page = query.page.unwrap_or(1);
    let filter = grid_filter(&query, per_page);
    let (cards, mut decorations, total, page, page_count, next_cursor) = app
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let total = post_db::count_cards(&tx, &filter)?;
            let page_count = (total.saturating_add(per_page - 1) / per_page).max(1);
            let page = requested_page.clamp(1, page_count);
            let filter = post_db::PostFilter {
                offset: (page - 1) * per_page,
                ..filter
            };
            let cards = post_db::cards(&tx, &filter)?;
            let decorations = grid_decorations(&tx, &cards)?;
            let next = if page < page_count {
                next_card_cursor(&tx, &cards)?
            } else {
                None
            };
            tx.commit()?;
            Ok((cards, decorations, total, page, page_count, next))
        })
        .await?;
    let active_tag = query.tag.as_deref().and_then(tag_db::normalize);
    let sources = app.db.call(|conn| post_db::sources(conn)).await?;
    let (tags, channels, default_channel) = app
        .db
        .call({
            let active_tag = active_tag.clone();
            move |conn| {
                let mut tags = tag_db::most_used(conn, 30)?;
                if let Some(tag) = active_tag {
                    tags.retain(|(name, _)| name != &tag);
                    let count = tag_db::visible_count(conn, &tag)?;
                    tags.insert(0, (tag, count));
                }
                let channels = channel_db::list(conn)?;
                let default = crate::db::web_queue_default(conn, &channels)?;
                Ok((tags, channels, default))
            }
        })
        .await?;
    let cards = grid_card_views(cards, &mut decorations, &query);

    Ok(Json(PostsPage {
        cards,
        total,
        page_number: page,
        page_count,
        per_page,
        next_cursor,
        default_channel,
        page_sizes: POST_PAGE_SIZES,
        grid_url: query.page_url(page, per_page),
        previous_page: (page > 1).then(|| query.page_url(page - 1, per_page)),
        next_page: (page < page_count).then(|| query.page_url(page + 1, per_page)),
        sources,
        top_tags: tags
            .iter()
            .map(|(tag, count)| TagCount {
                tag: tag.clone(),
                count: *count,
                url: query.tag_url(Some(tag)),
                active: active_tag.as_ref() == Some(tag),
            })
            .collect::<Vec<_>>(),
        clear_tag_url: query.tag_url(None),
        channels: channels.iter().map(ChannelChoice::from).collect(),
        states: PostState::all()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        filters: FiltersView {
            state: query.state,
            source: query.source,
            tag: active_tag,
            nsfw: query.nsfw,
            sent: query.sent,
            search: query.search,
        },
        setup: query.setup,
    }))
}

fn grid_filter(query: &PostFilter, limit: i64) -> post_db::PostFilter {
    post_db::PostFilter {
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
        // the grid is about "what is still to publish": sent posts are hidden
        // unless asked for, and `?sent=all` brings them back
        sent: match query.sent.as_deref() {
            Some("yes") => post_db::SentFilter::Sent,
            Some("all") => post_db::SentFilter::Any,
            Some("not_queued") => post_db::SentFilter::NotQueued,
            _ => post_db::SentFilter::NotSent,
        },
        // rejected posts are the curator's "no": the grid hides them unless the
        // state filter asks for something (`?state=all`, `?state=rejected`)
        without_rejected: matches!(query.state.as_deref(), None | Some("")),
        author_id: None,
        search: query.search.clone().filter(|s| !s.is_empty()),
        limit,
        offset: 0,
    }
}

fn next_card_cursor(
    conn: &rusqlite::Connection,
    cards: &[crate::model::PostCard],
) -> crate::error::Result<Option<String>> {
    cards
        .last()
        .map(|card| post_db::card_cursor(conn, card.id))
        .transpose()
}

#[derive(Default)]
struct CardDecoration {
    tags: Vec<String>,
    queue: Vec<CardBadge>,
}

fn grid_decorations(
    conn: &rusqlite::Connection,
    cards: &[crate::model::PostCard],
) -> crate::error::Result<HashMap<i64, CardDecoration>> {
    let ids: Vec<_> = cards.iter().map(|card| card.id).collect();
    let mut out = HashMap::<i64, CardDecoration>::new();
    for (post, channel, state) in queue_db::card_states(conn, &ids)? {
        out.entry(post).or_default().queue.push(CardBadge {
            class: state.as_str(),
            label: queue_state_label(state),
            channel,
        });
    }
    for (post, tag) in tag_db::card_tags(conn, &ids)? {
        out.entry(post).or_default().tags.push(tag);
    }
    Ok(out)
}

fn grid_card_views(
    cards: Vec<crate::model::PostCard>,
    decorations: &mut HashMap<i64, CardDecoration>,
    query: &PostFilter,
) -> Vec<PostCardView> {
    cards
        .into_iter()
        .map(|card| {
            let decoration = decorations.remove(&card.id).unwrap_or_default();
            PostCardView {
                id: card.id,
                source: card.source,
                rating: card.rating.as_str(),
                state: card.state.as_str(),
                artist: card.author_name,
                submitted_by: card.submitted_by,
                submitter: card.submitter,
                submitter_guest: card.submitter_guest,
                media_count: card.media_count,
                thumb: card.thumb_hash,
                nsfw: card.nsfw,
                snippet: card.snippet,
                pushed: card.pushed,
                queue_badges: decoration.queue,
                tags: decoration
                    .tags
                    .into_iter()
                    .map(|tag| CardTag {
                        url: query.tag_url(Some(&tag)),
                        name: tag,
                    })
                    .collect(),
                imported: card
                    .imported_at
                    .with_timezone(&chrono::Local)
                    .format("%m-%d %H:%M")
                    .to_string(),
            }
        })
        .collect()
}

async fn posts_more(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<PostFilter>,
) -> WebResult<Json<CardsPage>> {
    const BATCH: usize = 24;
    let cursor = query
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.split_once('|'))
        .and_then(|(time, id)| {
            chrono::DateTime::parse_from_rfc3339(time).ok()?;
            Some((time.to_owned(), id.parse().ok()?))
        })
        .ok_or_else(|| Error::invalid("Invalid posts cursor. Reload Posts and try again."))?;
    let filter = grid_filter(&query, (BATCH + 1) as i64);
    let (cards, mut decorations, next_cursor) = ws
        .app
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let mut cards = post_db::cards_before(&tx, &filter, Some(cursor))?;
            let more = cards.len() > BATCH;
            cards.truncate(BATCH);
            let next = if more {
                next_card_cursor(&tx, &cards)?
            } else {
                None
            };
            let decorations = grid_decorations(&tx, &cards)?;
            tx.commit()?;
            Ok((cards, decorations, next))
        })
        .await?;
    Ok(Json(CardsPage {
        cards: grid_card_views(cards, &mut decorations, &query),
        next_cursor,
    }))
}

async fn settings_page(State(ws): State<Arc<WebState>>) -> WebResult<Json<SettingsPage>> {
    let (channels, default_channel) = queue_channels(&ws.app).await?;
    let cleanup_job = ws
        .app
        .db
        .call(|conn| job_db::latest_kind(conn, crate::jobs::KIND_STORAGE_CLEANUP))
        .await?;
    Ok(Json(SettingsPage {
        channels: channels.iter().map(ChannelChoice::from).collect(),
        default_channel,
        cleanup_job: cleanup_job
            .as_ref()
            .map(|job| job_view(job, chrono::Utc::now())),
    }))
}

#[derive(serde::Deserialize)]
struct CleanupQuery {
    older_than_days: Option<u32>,
}

async fn cleanup_preview(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<CleanupQuery>,
) -> WebResult<Json<crate::storage::cleanup::Preview>> {
    let days = query
        .older_than_days
        .unwrap_or(crate::storage::cleanup::DEFAULT_AGE_DAYS);
    Ok(Json(crate::storage::cleanup::preview(&ws.app, days).await?))
}

#[derive(serde::Deserialize)]
struct CleanupForm {
    csrf: String,
    #[serde(flatten)]
    confirmation: crate::storage::cleanup::Confirmation,
}

async fn cleanup_apply(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<CleanupForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    if form.confirmation.cutoff > chrono::Utc::now()
        || form.confirmation.revision.len() != 64
        || !form
            .confirmation
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::invalid("Preview cleanup before applying it").into());
    }
    let job = crate::jobs::create_cleanup_job(&ws.app, form.confirmation).await?;
    let message = if job.is_some() {
        "Storage cleanup queued"
    } else {
        "Storage cleanup is already queued or running"
    };
    Ok(accepted("/settings", message, job.into_iter().collect()))
}

async fn settings_save(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let channel = forms::field(&form, "channel")
        .filter(|v| !v.is_empty())
        .map(|id| {
            id.parse::<i64>()
                .map_err(|_| Error::invalid("Choose an available channel"))
        })
        .transpose()?;
    ws.app
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let channel = channel.map(|id| channel_db::get(&tx, id)).transpose()?;
            crate::db::set_web_queue_default(&tx, channel.as_ref())?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(action_result("/settings", "Queue preference saved"))
}

async fn queue_channels(app: &AppState) -> crate::error::Result<(Vec<Channel>, Option<i64>)> {
    app.db
        .call(|conn| {
            let channels = channel_db::list(conn)?;
            let default = crate::db::web_queue_default(conn, &channels)?;
            Ok((channels, default))
        })
        .await
}

async fn posts_bulk(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let back = forms::field(&form, "back")
        .filter(|path| super::auth::local_page(path).is_some_and(|uri| uri.path() == "/posts"))
        .unwrap_or("/posts");
    let ids = forms::selected_ids(&form);
    if ids.is_empty() {
        return Ok(action_result(back, "no posts selected"));
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
            let enabled = app.cfg.guest_reputation;
            app.db
                .call(move |conn| {
                    let tx =
                        conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    for id in ids_for_db {
                        crate::db::reputation::review(&tx, id, state, enabled, true, None)?;
                    }
                    tx.commit()?;
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
                    crate::db::transaction(conn, |conn| {
                        for id in &ids_for_db {
                            post_db::set_nsfw_override(conn, *id, value)?;
                        }
                        Ok(())
                    })
                })
                .await?;
            format!("nsfw override updated on {} post(s)", ids.len())
        }
        "queue" => {
            let Some(channel_id) = channel_id else {
                return Ok(action_result(back, "choose a channel first"));
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
                    let tx =
                        conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    for id in &ids_for_db {
                        if post_is_sending(&tx, *id)? {
                            return Err(Error::invalid(
                                "Wait for active sends before deleting these posts",
                            ));
                        }
                        post_db::delete(&tx, *id)?;
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await?;
            format!("{} post(s) deleted", ids.len())
        }
        other => format!("unknown action {other}"),
    };
    Ok(action_result(back, &note))
}

async fn post_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Query(query): Query<PostDetailQuery>,
) -> WebResult<Json<PostDetailPage>> {
    let app = &ws.app;
    let suggestions_enabled = app.cfg.tag_suggestions.enabled;
    let (
        post,
        tags,
        removals,
        tag_review,
        merge_undo,
        compressed,
        merge_job,
        hidden,
        gallery_version,
        outgoing,
    ) = app
        .db
        .call(move |conn| {
            let post = post_db::get(conn, id)?;
            let tag_review = if suggestions_enabled {
                crate::db::tag_suggestions::review(conn, &post)?
            } else {
                None
            };
            Ok((
                post.clone(),
                tag_db::provenance(conn, id)?,
                tag_db::removals(conn, id)?,
                tag_review,
                crate::db::media_merges::undo_ids(conn, id)?,
                crate::db::media_merges::compressed_positions(conn, id)?,
                crate::db::media_merges::latest_job(conn, id)?,
                crate::db::media_merges::hidden_positions(conn, id, &post.media)?,
                crate::db::media_merges::edit_version(conn, id, &post.media)?,
                crate::db::media_merges::outgoing(conn, post.clone())?,
            ))
        })
        .await?;
    let manual_tags = tags
        .iter()
        .filter(|tag| tag.manual)
        .map(|tag| tag.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let (channels, default_channel) = queue_channels(app).await?;
    let rules = app.db.call(|conn| tag_db::TagRules::load(conn)).await?;
    let submitter = app
        .db
        .call(move |conn| post_db::submitter(conn, id))
        .await?;

    let guest_review = app.cfg.guest_reputation
        && app
            .db
            .call(move |conn| crate::db::reputation::reviewable(conn, id))
            .await?;
    let post_queues = app
        .db
        .call(move |conn| queue_db::for_post(conn, id))
        .await?;
    let now = chrono::Utc::now();
    let tag_review = tag_review.map(|review| super::tag_suggestions::review_view(review, now));
    let mut captions = Vec::new();
    for channel in &channels {
        let decision = crate::nsfw::evaluate(&outgoing, channel, &rules);
        let mut preview =
            match crate::tg::publish::build_plan(app, channel, &outgoing, 0, decision.clone()) {
                Ok(plan) => CaptionView {
                    channel: channel.title.clone(),
                    tg_id: channel.tg_id.clone(),
                    caption: plan.caption.text,
                    length: plan.caption.length,
                    truncated: plan.caption.truncated,
                    messages: plan.messages.len(),
                    verdict: plan.decision.reason,
                    allowed: decision.is_allowed(),
                    spoiler: decision.spoiler(),
                    eta: None,
                    eta_in: None,
                },
                Err(err) => CaptionView {
                    channel: channel.title.clone(),
                    tg_id: channel.tg_id.clone(),
                    caption: format!("template error: {err}"),
                    length: 0,
                    truncated: false,
                    messages: 0,
                    verdict: err.to_string(),
                    allowed: false,
                    spoiler: false,
                    eta: None,
                    eta_in: None,
                },
            };
        // when this post would go out, if it is queued in this channel
        if let Some(slot) = post_eta(app, &post_queues, channel).await? {
            preview.eta = Some(crate::tg::publish::slot_label(slot, now));
            preview.eta_in = Some(crate::tg::publish::format_duration(slot - now));
        }
        captions.push(preview);
    }

    let merge_undo: std::collections::HashMap<_, _> = merge_undo.into_iter().collect();
    let photo_limit = crate::media::compress::photo_limit(app.cfg.max_upload_bytes);
    let can_merge = outgoing
        .media
        .iter()
        .filter(|item| crate::media::merge::supports(item))
        .take(2)
        .count()
        == 2;
    let has_media_tools = !post.media.is_empty();
    let media: Vec<MediaView> = post
        .media
        .iter()
        .enumerate()
        .map(|(position, item)| {
            let editable = crate::media::merge::supports(item);
            MediaView {
                id: item.id,
                hash: item.blob_hash.clone(),
                thumb: item
                    .poster_hash
                    .clone()
                    .unwrap_or_else(|| item.blob_hash.clone()),
                ext: item.ext.clone(),
                is_video: item.is_video,
                width: item.width,
                height: item.height,
                kib: (item.bytes as f64 / 1024.0).round(),
                hidden: hidden.binary_search(&position).is_ok(),
                can_select: can_merge && editable && hidden.binary_search(&position).is_err(),
                can_compress: editable,
                compressed: compressed.contains(&position),
                over_photo_limit: editable
                    && (item.bytes > photo_limit as i64
                        || item.width.zip(item.height).is_some_and(|(w, h)| {
                            crate::media::compress::geometry_error(w, h).is_some()
                        })),
                undo_id: merge_undo.get(&position).copied(),
            }
        })
        .collect();
    let media_total = crate::model::format_bytes(
        post.media
            .iter()
            .map(|m| m.bytes.max(0) as u64)
            .sum::<u64>(),
    );
    let nsfw = post.is_nsfw(rules.hits_nsfw(&post.tags));
    Ok(Json(PostDetailPage {
        open_section: query.section,
        guest_review,
        post: PostView {
            id: post.id,
            source: post.source,
            source_id: post.source_id,
            url: post.url,
            title: post.title,
            description: post.description,
            artist: post.author.as_ref().map(|a| a.display()),
            artist_url: post.author.as_ref().and_then(|a| a.url.clone()),
            rating: post.rating.as_str(),
            state: post.state.as_str(),
            nsfw_override: match post.nsfw_override {
                Some(true) => "yes",
                Some(false) => "no",
                None => "auto",
            },
            nsfw,
            tags,
            manual_tags,
            tag_removals: removals,
            notes: post.notes,
            imported: post
                .imported_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        },
        submitter: submitter.map(|(tg_id, label, admin)| SubmitterView {
            tg_id,
            label,
            admin,
        }),
        media_total,
        media,
        can_merge,
        has_media_tools,
        merge_job: merge_job.as_ref().map(|job| job_view(job, now)),
        gallery_version,
        outgoing_count: outgoing.media.len(),
        tag_review,
        can_suggest_tags: suggestions_enabled && post.media.iter().any(|media| !media.is_video),
        captions,
        default_channel,
        ratings: Rating::all().iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        states: PostState::all()
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        channels: channels.iter().map(ChannelChoice::from).collect(),
    }))
}

async fn post_queue(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let channel_id = forms::field(&form, "channel")
        .and_then(|raw| raw.parse().ok())
        .ok_or_else(|| Error::invalid("Choose a destination channel"))?;
    let app = &ws.app;
    let channel = app
        .db
        .call(move |conn| channel_db::get(conn, channel_id))
        .await?;
    let outcome = crate::queue::enqueue_checked(app, &channel, id, 0).await?;
    if let crate::queue::EnqueueOutcome::Skipped(reason) = &outcome {
        return Err(Error::invalid(reason.clone()).into());
    }
    Ok(action_result(
        &format!("/posts/{id}"),
        &format!("{} · {}", outcome.describe(), channel.title),
    ))
}

async fn post_update(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<PostEdit>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let app = &ws.app;
    let enabled = app.cfg.guest_reputation;
    let back = if form.tags.is_some() {
        format!("/posts/{id}?section=tags")
    } else {
        format!("/posts/{id}")
    };
    let rating = form
        .rating
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(str::parse::<Rating>)
        .transpose()
        .map_err(|_| WebError(Error::invalid("bad rating")))?;
    let state = form.state.as_deref().and_then(PostState::try_parse);
    if form.approval_points.is_some() && state != Some(PostState::Approved) {
        return Err(Error::invalid("choose approved to set a reward").into());
    }
    app.db
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if form.title.is_some() || form.description.is_some() {
                post_db::edit_text(&tx, id, form.title.as_deref(), form.description.as_deref())?;
            }
            if let Some(state) = state {
                let changed = post_db::get(&tx, id)?.state != state;
                if changed || form.approval_points.is_some() {
                    crate::db::reputation::review(
                        &tx,
                        id,
                        state,
                        enabled,
                        true,
                        form.approval_points,
                    )?;
                }
            }
            if let Some(rating) = rating {
                post_db::set_rating(&tx, id, rating)?;
            }
            if let Some(nsfw) = form.nsfw.as_deref() {
                let value = match nsfw {
                    "yes" => Some(true),
                    "no" => Some(false),
                    _ => None,
                };
                post_db::set_nsfw_override(&tx, id, value)?;
            }
            if let Some(tags) = form.tags.as_deref() {
                let tags: Vec<String> = tags
                    .split([',', ' ', '\n'])
                    .map(|t| t.trim().trim_start_matches('#').to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                tag_db::replace_manual_in(&tx, id, &tags)?;
            }
            if let Some(notes) = form.notes.as_deref() {
                let notes = (!notes.trim().is_empty()).then_some(notes.trim());
                post_db::set_notes(&tx, id, notes)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(action_result(&back, "Changes saved"))
}

fn post_is_sending(conn: &rusqlite::Connection, id: i64) -> crate::error::Result<bool> {
    Ok(queue_db::for_post(conn, id)?
        .iter()
        .any(|(_, _, state)| *state == QueueState::Sending))
}

async fn post_delete_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Json<PostDeletePage>> {
    let (post, sending) = ws
        .app
        .db
        .call(move |conn| Ok((post_db::get(conn, id)?, post_is_sending(conn, id)?)))
        .await?;
    Ok(Json(PostDeletePage {
        id,
        title: post.title,
        imported_at: post.imported_at.to_rfc3339(),
        sending,
    }))
}

async fn post_delete(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<PostDelete>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let deleted = ws
        .app
        .db
        .call(move |conn| {
            // Keep the delivery check and deletion atomic with lease acquisition.
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let post = post_db::get(&tx, id)?;
            if post.imported_at != form.imported_at {
                return Err(Error::invalid(
                    "This post changed. Open the confirmation again.",
                ));
            }
            if post_is_sending(&tx, id)? {
                return Ok(false);
            }
            post_db::delete(&tx, id)?;
            tx.commit()?;
            Ok(true)
        })
        .await?;
    if deleted {
        Ok(action_result("/posts", "Post deleted"))
    } else {
        Ok(action_result(
            &format!("/posts/{id}/delete"),
            "Wait for the current send to finish before deleting.",
        ))
    }
}

async fn post_tag_action(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<PostTagAction>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let tag_id = || form.tag_id.ok_or_else(|| Error::invalid("Missing tag ID"));
    let message = match form.action.as_str() {
        "add" => {
            let tags = form.tags.ok_or_else(|| Error::invalid("Enter a tag"))?;
            ws.app
                .db
                .call(move |conn| tag_db::add_post_tags(conn, id, &tags))
                .await?;
            "tags added"
        }
        "remove_tag" => {
            let tag_id = tag_id()?;
            ws.app
                .db
                .call(move |conn| {
                    crate::db::transaction(conn, |conn| tag_db::remove_by_id(conn, id, tag_id))
                })
                .await?;
            "tag removed"
        }
        "remove" => {
            let tag_id = tag_id()?;
            let changed = ws
                .app
                .db
                .call(move |conn| tag_db::suppress_source(conn, id, tag_id))
                .await?;
            if changed {
                "source tag removed"
            } else {
                "source tag unavailable or already removed"
            }
        }
        "undo" => {
            let tag_id = tag_id()?;
            let changed = ws
                .app
                .db
                .call(move |conn| tag_db::restore_source(conn, id, tag_id))
                .await?;
            if changed {
                "source tag removal undone"
            } else {
                "no saved removal for this tag"
            }
        }
        _ => return Err(Error::invalid("unknown source tag action").into()),
    };
    Ok(action_result(&format!("/posts/{id}?section=tags"), message))
}

// ---------------------------------------------------------------- channels

async fn channels_page(State(ws): State<Arc<WebState>>) -> WebResult<Json<ChannelsPage>> {
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
        views.push(ChannelSummary {
            id: channel.id,
            alias: channel.alias.clone(),
            discovered: channel.discovered,
            title: channel.title.clone(),
            tg_id: channel.tg_id.clone(),
            queued,
            interval: channel.interval_minutes,
            nsfw_policy: channel.nsfw_policy.as_str(),
            tag_style: channel.tag_style.as_str(),
            attribution: channel.attribution.as_str(),
            active: format!(
                "{} - {} ({})",
                channel.active_from, channel.active_to, channel.tz
            ),
            paused: channel.paused,
            enabled: channel.enabled,
            next: crate::tg::publish::next_slot_hint(channel, now),
        });
    }
    Ok(Json(ChannelsPage {
        channels: views,
        timezones: timezones(),
        templates: app.renderer.template_names(),
    }))
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
    if let Some(tz) = form.tz.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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

fn timezones() -> &'static [&'static str] {
    static ZONES: std::sync::LazyLock<Vec<&str>> = std::sync::LazyLock::new(|| {
        let mut zones: Vec<_> = chrono_tz::TZ_VARIANTS
            .iter()
            .map(|zone| zone.name())
            .collect();
        zones.sort_unstable();
        zones
    });
    &ZONES
}

async fn verify_destination(app: &AppState, target: &str) -> crate::error::Result<()> {
    let transport = app.transport.as_ref().ok_or_else(|| {
        Error::invalid("Configure a Telegram bot token and restart nujijushi, then try again.")
    })?;
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        let chat = transport.get_chat(target).await.map_err(|error| {
            if error.is_retryable() {
                Error::invalid("Telegram could not check this channel. Try again in a moment.")
            } else {
                Error::invalid("Telegram could not find this channel. Check its @username or numeric ID and make sure the bot has access.")
            }
        })?;
        if !chat.is_channel {
            return Err(Error::invalid("This destination is not a Telegram channel. Enter the username or ID of a broadcast channel."));
        }
        let status = transport.check_chat(&chat.id.to_string()).await.map_err(|error| {
            if error.is_retryable() {
                Error::invalid("Telegram could not check the bot’s permissions. Try again in a moment.")
            } else {
                Error::invalid("Add the bot as a channel administrator with permission to post messages, then try again.")
            }
        })?;
        if !status.is_admin || !status.can_post {
            return Err(Error::invalid("Give the bot administrator access with permission to post messages, then try again."));
        }
        Ok(())
    })
    .await
    .map_err(|_| Error::invalid("Telegram did not respond in time. Check the connection and try again."))?
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
    verify_destination(&ws.app, &channel.tg_id).await?;
    let to_insert = channel;
    let id = ws
        .app
        .db
        .call(move |conn| channel_db::insert(conn, &to_insert))
        .await?;
    Ok(action_result(
        &format!("/channels/{id}/edit"),
        &format!("Channel #{id} verified and created"),
    ))
}

async fn channel_edit_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Json<ChannelEditPage>> {
    let app = &ws.app;
    let channel = app.db.call(move |conn| channel_db::get(conn, id)).await?;
    let preview = crate::tg::publish::preview_channel(app, &channel, 1).await?;
    Ok(Json(ChannelEditPage {
        channel: ChannelSettings {
            id: channel.id,
            alias: channel.alias.clone(),
            discovered: channel.discovered,
            title: channel.title.clone(),
            tg_id: channel.tg_id.clone(),
            enabled: channel.enabled,
            interval_minutes: channel.interval_minutes,
            active_from: channel.active_from,
            active_to: channel.active_to,
            tz: channel.tz,
            daily_cap: channel.daily_cap,
            jitter_secs: channel.jitter_secs,
            artist_cooldown: channel.artist_cooldown,
            nsfw_policy: channel.nsfw_policy.as_str(),
            min_media: channel.min_media,
            blocked_tags: channel.blocked_tags,
            required_tags: channel.required_tags,
            tag_style: channel.tag_style.as_str(),
            max_tags: channel.max_tags,
            attribution: channel.attribution.as_str(),
            parse_mode: channel.parse_mode.as_str(),
            template_name: channel.template_name,
            template: channel.template,
            silent: channel.silent,
            protect: channel.protect,
            include_source_tag: channel.include_source_tag,
            paused: channel.paused,
            failures: channel.consecutive_failures,
        },
        timezones: timezones(),
        policies: NsfwPolicy::all()
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        tag_styles: TagStyle::all()
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        attributions: Attribution::all()
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        parse_modes: ParseMode::all()
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>(),
        templates: app.renderer.template_names(),
        default_template: app.renderer.template_source("default").unwrap_or_default(),
        sample: preview.first().map(|item| CaptionSample {
            caption: item.caption.clone(),
            reason: item.reason.clone(),
            media: item.media,
            messages: item.messages,
            spoiler: item.spoiler,
        }),
    }))
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
    if existing.tg_id != channel.tg_id {
        verify_destination(app, &channel.tg_id).await?;
    }
    let to_update = channel;
    app.db
        .call(move |conn| channel_db::update(conn, &to_update))
        .await?;
    Ok(action_result(&format!("/channels/{id}/edit"), "saved"))
}

fn queue_state_label(state: QueueState) -> &'static str {
    match state {
        QueueState::Sent => "Sent",
        QueueState::Sending => "Sending",
        QueueState::Unknown => "Delivery unconfirmed",
        QueueState::Queued => "Queued",
        QueueState::Held => "Awaiting preview",
        QueueState::Failed => "Failed",
        QueueState::Skipped => "Skipped",
    }
}

async fn channel_schedule(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let paused = match forms::field(&form, "action") {
        Some("pause") => true,
        Some("resume") => false,
        _ => return Err(Error::invalid("Unknown schedule action").into()),
    };
    ws.app
        .db
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let channel = channel_db::get(&tx, id)?;
            if !channel.enabled {
                return Err(Error::invalid(
                    "Confirm a preview to start scheduling for this channel",
                ));
            }
            channel_db::set_paused(&tx, id, paused)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    let back = format!("/channels/{id}/queue");
    Ok(action_result(
        &back,
        if paused {
            "Schedule paused"
        } else {
            "Schedule resumed"
        },
    ))
}

async fn queue_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Json<QueuePage>> {
    let app = &ws.app;
    use super::queue_status::{PAGE_LIMIT, VISIBLE_STATES, snapshot};
    let (channel, activity, entries, mut unresolved) = app
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let channel = channel_db::get(&tx, id)?;
            let activity = snapshot(&tx, &channel)?;
            let entries = queue_db::entries(&tx, id, VISIBLE_STATES, PAGE_LIMIT)?;
            let unresolved: HashMap<_, _> = queue_db::unconfirmed(&tx, Some(id))?
                .into_iter()
                .map(|item| (item.id, item))
                .collect();
            tx.commit()?;
            Ok((channel, activity, entries, unresolved))
        })
        .await?;
    // nothing to load here any more: `card.nsfw` already carries the override

    // the plan the scheduler would follow: which item takes which slot. One
    // projection, so the ETA column and the preview page cannot disagree.
    let now = chrono::Utc::now();
    let planned: std::collections::HashMap<i64, chrono::DateTime<chrono::Utc>> =
        crate::tg::publish::project_queue(app, &channel, entries.len().min(200))
            .await?
            .into_iter()
            .map(|projected| (projected.queue_id, projected.slot))
            .collect();

    let items: Vec<QueueRow> = entries
        .iter()
        .map(|entry| {
            let post = entry.item.post_id;
            let slot = planned.get(&entry.item.id).copied();
            QueueRow {
                id: entry.item.id,
                post_id: post,
                state: entry.item.state.as_str(),
                state_label: queue_state_label(entry.item.state),
                delivery: unresolved.remove(&entry.item.id),
                priority: entry.item.priority,
                pinned: entry.item.pinned,
                attempts: entry.item.attempts,
                error: entry.item.last_error.clone(),
                artist: entry.card.author_name.clone(),
                snippet: entry.card.snippet.clone(),
                thumb: entry.card.thumb_hash.clone(),
                media_count: entry.card.media_count,
                nsfw: entry.card.nsfw,
                added: entry
                    .item
                    .added_at
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
                eta: slot.map(|slot| crate::tg::publish::slot_label(slot, now)),
                eta_in: slot.map(|slot| crate::tg::publish::format_duration(slot - now)),
            }
        })
        .collect();

    let eta_hint = planned
        .is_empty()
        .then_some("no ETA: nothing is queued, or the channel is paused/outside its hours");

    Ok(Json(QueuePage {
        channel: QueueChannel {
            id: channel.id,
            title: channel.title.clone(),
            tg_id: channel.tg_id.clone(),
            enabled: channel.enabled,
            paused: channel.paused,
        },
        activity,
        items,
        eta_hint,
    }))
}

fn guarded_queue_edit(
    conn: &mut rusqlite::Connection,
    id: i64,
    allowed: impl FnOnce(QueueState) -> bool,
    action: impl FnOnce(&rusqlite::Connection) -> crate::error::Result<()>,
) -> crate::error::Result<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let item = queue_db::get(&tx, id)?;
    if !allowed(item.state) {
        return Err(Error::invalid(format!(
            "This action is unavailable while the item is {}",
            item.state
        )));
    }
    action(&tx)?;
    tx.commit()?;
    Ok(())
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
            app.db
                .call(move |conn| {
                    guarded_queue_edit(
                        conn,
                        id,
                        |state| matches!(state, QueueState::Queued | QueueState::Failed),
                        |tx| queue_db::skip(tx, id).map(|_| ()),
                    )
                })
                .await?;
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
            app.db
                .call(move |conn| {
                    guarded_queue_edit(
                        conn,
                        id,
                        |state| matches!(state, QueueState::Failed | QueueState::Skipped),
                        |tx| queue_db::requeue(tx, id).map(|_| ()),
                    )
                })
                .await?;
            "back in the queue".to_string()
        }
        "remove" => {
            app.db
                .call(move |conn| {
                    guarded_queue_edit(
                        conn,
                        id,
                        |state| {
                            matches!(
                                state,
                                QueueState::Held
                                    | QueueState::Queued
                                    | QueueState::Failed
                                    | QueueState::Skipped
                            )
                        },
                        |tx| queue_db::delete(tx, id).map(|_| ()),
                    )
                })
                .await?;
            "removed".to_string()
        }
        "delivered" => {
            // confirm an unconfirmed send: mark it sent without touching the API
            let message_ids = match form.message_id {
                Some(id) if id > 0 => Some(id.to_string()),
                Some(_) => return Err(Error::invalid("message ID must be positive").into()),
                None => None,
            };
            app.db
                .call(move |conn| queue_db::resolve_unknown(conn, id, true, message_ids.as_deref()))
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
        Some(channel_id) => Ok(action_result(
            &format!("/channels/{channel_id}/queue"),
            &note,
        )),
        None => Ok(action_result("/", &note)),
    }
}

async fn preview_page(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
) -> WebResult<Json<PreviewPage>> {
    let app = &ws.app;
    let channel = app.db.call(move |conn| channel_db::get(conn, id)).await?;
    let stopped = !channel.enabled || channel.paused;
    let held = super::setup::held_preview(app, &channel).await?;
    let mut projection = channel.clone();
    projection.enabled = true;
    projection.paused = false;
    let items = crate::tg::publish::preview_channel(app, &projection, 10).await?;
    let enable_item = if app.cfg.require_bot_token().is_ok() {
        match &held {
            Some(held) if held.allowed => Some(PreviewSelection {
                queue_id: held.queue_id,
                post_id: held.post_id,
            }),
            None if stopped => items.first().map(|item| PreviewSelection {
                queue_id: item.queue_id,
                post_id: item.post_id,
            }),
            _ => None,
        }
    } else {
        None
    };
    let now = chrono::Utc::now();
    Ok(Json(PreviewPage {
        channel: ChannelTarget {
            id: channel.id,
            title: channel.title.clone(),
            tg_id: channel.tg_id,
        },
        held,
        stopped,
        enable_item,
        items: items
            .iter()
            .enumerate()
            .map(|(index, item)| PreviewRow {
                slot: index + 1,
                queue_id: item.queue_id,
                post_id: item.post_id,
                title: item.title.clone(),
                caption: item.caption.clone(),
                media: item.media,
                messages: item.messages,
                spoiler: item.spoiler,
                nsfw: item.nsfw,
                reason: item.reason.clone(),
                eta: crate::tg::publish::slot_label(item.eta, now),
                eta_in: crate::tg::publish::format_duration(item.eta - now),
            })
            .collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------- storage

/// Queue an upload of whatever mirrors are still pending.
///
/// Uploads take minutes, so this only writes a job row: the request returns at
/// once and the worker does the transfers.
async fn storage_drain(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let job = crate::jobs::create_drain_job(&ws.app, 25).await?;
    let note = match job {
        Some(id) => format!("upload queued as job #{id} - watch the jobs panel"),
        None => "an upload is already queued".to_string(),
    };
    Ok(accepted("/", &note, job.into_iter().collect()))
}

// ---------------------------------------------------------------- publish
/// When this post would go out in `channel`, if it is queued there: the same
/// projection the queue page uses, so both pages show the same instant.
async fn post_eta(
    app: &AppState,
    queues: &[(i64, i64, QueueState)],
    channel: &Channel,
) -> crate::error::Result<Option<chrono::DateTime<chrono::Utc>>> {
    let Some((queue_id, ..)) = queues
        .iter()
        .find(|(_, channel_id, state)| *channel_id == channel.id && *state == QueueState::Queued)
    else {
        return Ok(None);
    };
    Ok(crate::tg::publish::project_queue(app, channel, 200)
        .await?
        .into_iter()
        .find(|projected| projected.queue_id == *queue_id)
        .map(|projected| projected.slot))
}

/// "Publish now": either the next queued post of a channel, or one specific
/// queue item. Deliberately bypasses the schedule (admins only — the UI is
/// token-protected); the NSFW policy and the lease still apply.
///
/// The Telegram upload happens in the job worker, not in this request: an album
/// of ten videos used to hold the POST open for minutes. A second click while
/// the first job is still pending is refused, and the lease still keeps the send
/// itself idempotent.
async fn publish_action(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let app = &ws.app;
    let back = forms::field(&form, "back")
        .filter(|path| auth::local_page(path).is_some())
        .unwrap_or("/")
        .to_string();
    let queue_id = forms::field(&form, "queue_id").and_then(|raw| raw.parse::<i64>().ok());
    let channel_id = forms::field(&form, "channel_id").and_then(|raw| raw.parse::<i64>().ok());

    let queued = if let Some(queue_id) = queue_id {
        crate::jobs::create_publish_item_job(app, queue_id).await
    } else if let Some(channel_id) = channel_id {
        crate::jobs::create_publish_channel_job(app, channel_id).await
    } else {
        return Ok(action_result(&back, "nothing selected to publish"));
    };

    let queued = queued?;
    let note = match queued {
        Some(id) => format!("publish queued as job #{id} - the jobs panel shows it finish"),
        None => "a publish for this is already queued or running".to_string(),
    };
    Ok(accepted(&back, &note, queued.into_iter().collect()))
}

async fn flush_action(
    State(ws): State<Arc<WebState>>,
    Path(channel_id): Path<i64>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    ws.app
        .db
        .call(move |conn| channel_db::get(conn, channel_id))
        .await?;
    let job = crate::jobs::create_flush_channel_job(&ws.app, channel_id).await?;
    let note = match job {
        Some(id) => format!("Queue flush scheduled as job #{id}"),
        None => "A queue flush is already queued or running".to_string(),
    };
    Ok(accepted(
        &format!("/channels/{channel_id}/queue"),
        &note,
        job.into_iter().collect(),
    ))
}

/// Is the scheduler alive? Read by the dashboard banner and `/api/status`, so a
/// queue that is not moving can be explained instead of guessed at.
#[derive(serde::Serialize)]
pub(super) struct SchedulerView {
    last_tick: Option<String>,
    age_secs: Option<i64>,
    age: String,
    stale: bool,
}

async fn scheduler_view(
    app: &AppState,
    now: chrono::DateTime<chrono::Utc>,
) -> crate::error::Result<SchedulerView> {
    use crate::queue::scheduler::{STALE_SECS, TICK_KEY, tick_age};

    let last_tick = app
        .db
        .call(|conn| crate::db::setting(conn, TICK_KEY))
        .await?;
    let age = tick_age(last_tick.as_deref(), now);
    Ok(SchedulerView {
        last_tick,
        age_secs: age,
        age: age
            .map(|secs| crate::tg::publish::format_duration(chrono::Duration::seconds(secs)))
            .unwrap_or_else(|| "never".to_string()),
        stale: age.is_none_or(|secs| secs > STALE_SECS),
    })
}

// ---------------------------------------------------------------- jobs

/// Job representation shared by initial admin reads and live status.
pub fn job_view(job: &job_db::Job, now: chrono::DateTime<chrono::Utc>) -> JobView {
    let percent = if job.total > 0 {
        (job.progress * 100 / job.total).clamp(0, 100)
    } else if job.state == "done" {
        100
    } else {
        0
    };
    let age = now - job.updated_at;
    JobView {
        id: job.id,
        kind: job.kind.clone(),
        kind_label: kind_label(&job.kind).into(),
        watch_id: (job.kind == crate::jobs::KIND_WATCH_CHECK)
            .then(|| job.payload.as_deref()?.parse::<i64>().ok())
            .flatten(),
        payload: if crate::jobs::is_gallery_edit(&job.kind) {
            job.post_id.map(|id| format!("post #{id}"))
        } else {
            job.payload.clone()
        },
        state: job.state.clone(),
        progress: job.progress,
        total: job.total,
        percent,
        // a bar only means something when one job really processes many posts
        // (an artist page or a folder): `1/1` for a single-target job was noise
        track: job.total > 1,
        message: job.message.clone(),
        post_id: job.post_id,
        active: job.is_active(),
        retryable: job.kind == crate::jobs::KIND_IMPORT && job.state == "failed",
        age_secs: age.num_seconds().max(0),
        age: crate::tg::publish::format_duration(age),
        created: job
            .created_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    }
}

/// What a job kind means to a person reading the page.
fn kind_label(kind: &str) -> &str {
    match kind {
        crate::jobs::KIND_IMPORT => "import",
        crate::jobs::KIND_PUBLISH_CHANNEL => "publish next",
        crate::jobs::KIND_PUBLISH_ITEM => "publish item",
        crate::jobs::KIND_FLUSH_CHANNEL => "flush queue",
        crate::jobs::KIND_STORAGE_DRAIN => "storage upload",
        crate::jobs::KIND_STORAGE_CLEANUP => "storage cleanup",
        crate::jobs::KIND_WATCH_CHECK => "watch check",
        crate::jobs::KIND_TAG_SUGGESTIONS => "suggest tags",
        crate::jobs::KIND_MERGE_MEDIA => "merge images",
        crate::jobs::KIND_COMPRESS_MEDIA => "compress image",
        other => other,
    }
}

async fn jobs_page(State(ws): State<Arc<WebState>>) -> WebResult<Json<JobsPage>> {
    let app = &ws.app;
    let now = chrono::Utc::now();
    let jobs = app.db.call(|conn| job_db::list(conn, 50)).await?;
    let waiting = app
        .db
        .call(|conn| job_db::count_pending(conn, crate::jobs::KIND_IMPORT))
        .await?;
    Ok(Json(JobsPage {
        jobs: jobs
            .iter()
            .map(|job| job_view(job, now))
            .collect::<Vec<_>>(),
        parallel: app.cfg.max_concurrent_imports,
        waiting,
    }))
}

/// Re-run a failed import as a new job row, so the history keeps both attempts.
async fn jobs_retry(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let back = if forms::field(&form, "back") == Some("/import") {
        "/import"
    } else {
        "/jobs"
    };
    let app = &ws.app;
    let Some(id) = forms::field(&form, "id").and_then(|raw| raw.parse::<i64>().ok()) else {
        return Ok(action_result(back, "no job selected"));
    };
    let job = app
        .db
        .call(move |conn| job_db::get(conn, id))
        .await?
        .ok_or_else(|| Error::NotFound(format!("job {id}")))?;
    if job.kind != crate::jobs::KIND_IMPORT {
        return Ok(action_result(
            back,
            "only imports can be retried - publishing has its own button",
        ));
    }
    // only a settled failure may be retried: a pending or running import would
    // otherwise be started twice for the same target
    if job.state != "failed" {
        return Ok(action_result(back, "only failed jobs can be retried"));
    }
    let target = job.payload.clone().unwrap_or_default();
    if target.trim().is_empty() {
        return Ok(action_result(back, "that job has no target to retry"));
    }
    let job = crate::jobs::create_import_job_unique(app, &target).await?;
    let note = match job {
        Some(new_id) => format!("job #{id} re-queued as #{new_id}"),
        None => format!("job #{id} is already queued or running"),
    };
    Ok(accepted(back, &note, job.into_iter().collect()))
}

// ---------------------------------------------------------------- status API

/// Machine-readable state for existing status API consumers.
#[derive(serde::Deserialize)]
struct StatusQuery {
    tag_post: Option<i64>,
    merge_post: Option<i64>,
}

async fn api_status(
    State(ws): State<Arc<WebState>>,
    Query(query): Query<StatusQuery>,
) -> WebResult<Response> {
    let mut payload = status_payload(&ws.app).await?;
    if let Some(post_id) = query.tag_post
        && ws.app.cfg.tag_suggestions.enabled
    {
        let job = ws
            .app
            .db
            .call(move |conn| crate::db::tag_suggestions::latest_job(conn, post_id))
            .await?;
        payload["tag_suggestion_job"] =
            json!(job.as_ref().map(|job| job_view(job, chrono::Utc::now())));
    }
    if let Some(post_id) = query.merge_post {
        let job = ws
            .app
            .db
            .call(move |conn| crate::db::media_merges::latest_job(conn, post_id))
            .await?;
        payload["media_merge_job"] =
            json!(job.as_ref().map(|job| job_view(job, chrono::Utc::now())));
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(payload)).into_response())
}

/// The `/api/status` document: recent jobs plus per-channel queue depth and
/// whether a publish is in flight. Cheap queries only: the last 25 jobs, the
/// channel list, and one grouped count per channel.
pub async fn status_payload(app: &AppState) -> crate::error::Result<Value> {
    let now = chrono::Utc::now();
    let jobs = app.db.call(|conn| job_db::list(conn, 25)).await?;
    let (watch_jobs, duplicate_indexing) = app
        .db
        .call(|conn| {
            let watch_jobs = crate::db::watches::list(conn)?
                .into_iter()
                .filter_map(|watch| watch.last_job_id)
                .map(|id| job_db::get(conn, id))
                .collect::<crate::error::Result<Vec<_>>>()?;
            Ok((watch_jobs, crate::db::duplicates::has_pending(conn)?))
        })
        .await?;
    let channels = app.db.call(|conn| channel_db::list(conn)).await?;

    let mut channel_views = Vec::with_capacity(channels.len());
    for channel in &channels {
        let channel_id = channel.id;
        let counts = app
            .db
            .call(move |conn| queue_db::counts(conn, channel_id))
            .await?;
        let count = |state: &str| {
            counts
                .iter()
                .filter(|(name, _)| name == state)
                .map(|(_, n)| *n)
                .sum::<i64>()
        };
        let payload = channel_id.to_string();
        let queued_publish = jobs.iter().any(|job| {
            matches!(
                job.kind.as_str(),
                crate::jobs::KIND_PUBLISH_CHANNEL | crate::jobs::KIND_FLUSH_CHANNEL
            ) && job.is_active()
                && job.payload.as_deref() == Some(payload.as_str())
        });
        channel_views.push(json!({
            "id": channel_id,
            "alias": channel.alias.clone(),
            "title": channel.title.clone(),
            "queued": count(QueueState::Queued.as_str()),
            "in_flight": count(QueueState::Sending.as_str()),
            "publishing": queued_publish || count(QueueState::Sending.as_str()) > 0,
            "paused": channel.paused,
            "enabled": channel.enabled,
            "next_due": channel.next_due_at.map(|due| due.to_rfc3339()),
            "next": crate::tg::publish::next_slot_hint(channel, now),
        }));
    }

    Ok(json!({
        "now": now.to_rfc3339(),
        "jobs": jobs.iter().map(|job| job_view(job, now)).collect::<Vec<_>>(),
        "watch_jobs": watch_jobs.iter().flatten().map(|job| job_view(job, now)).collect::<Vec<_>>(),
        "duplicate_indexing": duplicate_indexing,
        "channels": channel_views,
        "scheduler": scheduler_view(app, now).await?,
    }))
}

// ---------------------------------------------------------------- import

async fn import_page(State(ws): State<Arc<WebState>>) -> WebResult<Json<ImportPage>> {
    let app = &ws.app;
    let now = chrono::Utc::now();
    let jobs = app.db.call(|conn| job_db::list(conn, 15)).await?;
    let waiting = app
        .db
        .call(|conn| job_db::count_pending(conn, crate::jobs::KIND_IMPORT))
        .await?;
    Ok(Json(ImportPage {
        jobs: jobs
            .iter()
            .map(|job| job_view(job, now))
            .collect::<Vec<_>>(),
        parallel: app.cfg.max_concurrent_imports,
        waiting,
        cookies: app.cfg.cookies_dir().display().to_string(),
    }))
}

async fn import_submit(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<ImportForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    let targets = form.target_list();
    if targets.is_empty() {
        return Ok(action_result("/import", "no targets given"));
    }
    let app = &ws.app;
    let mut jobs = Vec::new();
    for target in &targets {
        jobs.push(crate::jobs::create_job(app, target).await?);
    }
    let note = format!(
        "{} import job(s) queued. Open the results to review and queue artwork.",
        jobs.len()
    );
    Ok(accepted("/import", &note, jobs))
}

// ---------------------------------------------------------------- users

async fn users_page(State(ws): State<Arc<WebState>>) -> WebResult<Json<UsersPage>> {
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
        // (the same helper the CLI and the bot's /users use)
        let role = crate::bot::access::display_role(config_admin, &user.role);
        views.push(UserView {
            tg_id: user.tg_id,
            label: user.label(),
            role: role.to_string(),
            db_role: user.role.clone(),
            archived,
            pushed,
            sent,
            config_admin,
            last_seen: user
                .last_seen_at
                .as_deref()
                .and_then(crate::db::parse_ts)
                .map(|ts| {
                    ts.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default(),
        });
    }
    Ok(Json(UsersPage { users: views }))
}

async fn users_action(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FormMap>,
) -> WebResult<Response> {
    check_csrf(&ws, forms::field(&form, "csrf").unwrap_or_default())?;
    let Some(tg_id) = forms::field(&form, "tg_id").and_then(|raw| raw.parse::<i64>().ok()) else {
        return Ok(action_result("/users", "missing user id"));
    };
    let role = forms::field(&form, "role").unwrap_or("guest").to_string();
    if !matches!(role.as_str(), "admin" | "guest" | "banned") {
        return Ok(action_result("/users", "unknown role"));
    }
    if role != "admin" && ws.app.cfg.access.is_admin(tg_id) {
        return Ok(action_result(
            "/users",
            "that id is an admin in config.toml - edit the config to demote them",
        ));
    }
    let role_for_db = role.clone();
    ws.app
        .db
        .call(move |conn| crate::db::users::set_role(conn, tg_id, &role_for_db))
        .await?;
    let app = ws.app.clone();
    tokio::spawn(async move { crate::bot::menus::sync_stored_user(&app, tg_id).await });
    Ok(action_result("/users", &format!("{tg_id} -> {role}")))
}
