//! First-run checklist and the explicit handoff from preview to scheduling.

use std::sync::Arc;

use axum::Form;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::WebState;
use super::forms::empty_as_none;
use super::routes::{WebResult, check_csrf, flash_redirect};
use crate::config::Config;
use crate::db::{channels, posts, queue, tags::TagRules};
use crate::error::{Error, Result};
use crate::model::{Channel, PostState, QueueState};
use crate::queue::{EnqueueOutcome, hold_for_preview};
use crate::state::AppState;
use crate::tg::publish::build_plan;

const PICK_POST: &str = "/posts?setup=true";

pub(super) struct ChannelProgress {
    pub id: i64,
    pub active: bool,
    pub queued: bool,
    pub held: bool,
    pub sending: bool,
    pub sent: bool,
}

pub(super) fn view(
    cfg: &Config,
    imported: bool,
    delivered: bool,
    channels: &[ChannelProgress],
    scheduler_running: bool,
) -> Value {
    let connected = cfg.require_bot_token().is_ok();
    let held = channels.iter().find(|channel| channel.held);
    let ready = channels
        .iter()
        .any(|channel| channel.active && (channel.queued || channel.sending));
    let delivered = delivered || channels.iter().any(|channel| channel.sent);
    if held.is_none() && (delivered || (connected && ready && scheduler_running)) {
        return Value::Null;
    }

    let to_preview = held.or_else(|| {
        channels
            .iter()
            .find(|channel| !ready && !channel.active && channel.queued)
    });
    let steps = [
        json!({
            "title": "Connect Telegram",
            "detail": "Configure the bot token used to publish to Telegram.",
            "done": connected,
            "href": "#telegram-setup",
            "action": "Configure Telegram",
        }),
        json!({
            "title": "Add a channel",
            "detail": "Give the bot permission to post in your Telegram channel, then add the channel here.",
            "done": !channels.is_empty(),
            "href": "/channels#add-channel",
            "action": "Add a channel",
        }),
        json!({
            "title": "Import artwork",
            "detail": "Add a folder or a supported artwork URL.",
            "done": imported,
            "href": "/import",
            "action": "Import artwork",
        }),
        json!({
            "title": "Queue a post",
            "detail": "Choose one post to hold for preview.",
            "done": held.is_some() || channels.iter().any(|channel| channel.queued || channel.sending),
            "href": PICK_POST,
            "action": "Choose artwork",
        }),
        json!({
            "title": "Preview and enable scheduling",
            "detail": "Review the artwork and caption before allowing the channel to publish.",
            "done": ready && held.is_none(),
            "href": to_preview.map(|channel| format!("/channels/{}/preview", channel.id)),
            "action": "Open preview",
        }),
        json!({
            "title": "Verify the scheduler",
            "detail": "Start or keep the scheduler running and check for a recent tick.",
            "done": ready && held.is_none() && scheduler_running,
            "href": "/",
            "action": "Refresh scheduler status",
        }),
    ];

    json!({
        "steps": steps,
        "next": steps.iter().find(|step| step["done"] == false),
        "connected": connected,
    })
}

#[derive(Deserialize)]
pub(super) struct FirstPostForm {
    csrf: String,
    post_id: Option<i64>,
    #[serde(default, deserialize_with = "empty_as_none")]
    channel: Option<i64>,
}

pub(super) async fn queue_post(
    State(ws): State<Arc<WebState>>,
    Form(form): Form<FirstPostForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    ws.app.cfg.require_bot_token()?;
    let (Some(post_id), Some(channel_id)) = (form.post_id, form.channel) else {
        return Ok(flash_redirect(
            PICK_POST,
            "Choose one post and its destination channel.",
        ));
    };
    let channel = ws
        .app
        .db
        .call(move |conn| channels::get(conn, channel_id))
        .await?;
    match hold_for_preview(&ws.app, &channel, post_id).await? {
        EnqueueOutcome::Queued(_) => {
            Ok(Redirect::to(&format!("/channels/{channel_id}/preview")).into_response())
        }
        outcome => Ok(flash_redirect(PICK_POST, &outcome.describe())),
    }
}

pub(super) async fn held_preview(app: &AppState, channel: &Channel) -> Result<Option<Value>> {
    let id = channel.id;
    let preview = app
        .db
        .call(move |conn| {
            let item = queue::list(conn, id, &[QueueState::Held], 1)?.pop();
            item.map(|item| Ok((posts::get(conn, item.post_id)?, TagRules::load(conn)?, item)))
                .transpose()
        })
        .await?;
    let Some((post, rules, item)) = preview else {
        return Ok(None);
    };
    let decision = crate::nsfw::evaluate(&post, channel, &rules);
    let allowed = post.state != PostState::Rejected && decision.is_allowed();
    let reason = if post.state == PostState::Rejected {
        "This post was rejected during curation.".to_string()
    } else {
        decision.reason.clone()
    };
    let plan = build_plan(app, channel, &post, item.id, decision)?;
    Ok(Some(json!({
        "queue_id": item.id,
        "post_id": post.id,
        "allowed": allowed && plan.media_count() > 0,
        "caption": plan.caption.text,
        "media": plan.media_count(),
        "messages": plan.messages.len(),
        "reason": reason,
    })))
}

#[derive(Deserialize)]
pub(super) struct ScheduleForm {
    csrf: String,
    queue_id: i64,
    post_id: i64,
    target: String,
    held: bool,
}

pub(super) async fn start_scheduling(
    State(ws): State<Arc<WebState>>,
    Path(id): Path<i64>,
    Form(form): Form<ScheduleForm>,
) -> WebResult<Response> {
    check_csrf(&ws, &form.csrf)?;
    ws.app.cfg.require_bot_token()?;
    ws.app
        .db
        .call(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let channel = channels::get(&tx, id)?;
            let item = queue::get(&tx, form.queue_id)?;
            let expected = if form.held {
                QueueState::Held
            } else {
                QueueState::Queued
            };
            if item.channel_id != id
                || item.state != expected
                || item.post_id != form.post_id
                || channel.tg_id != form.target
            {
                return Err(Error::invalid(
                    "the queue changed. Open the preview again before enabling scheduling.",
                ));
            }
            let post = posts::get(&tx, item.post_id)?;
            let rules = TagRules::load(&tx)?;
            if post.state == PostState::Rejected
                || post.media.is_empty()
                || !crate::nsfw::evaluate(&post, &channel, &rules).is_allowed()
            {
                return Err(Error::invalid(
                    "this post can no longer be published to this channel",
                ));
            }
            if form.held {
                queue::release_held(&tx, item.id)?;
            }
            if !channel.enabled {
                channels::set_enabled(&tx, id, true)?;
            }
            if channel.paused {
                channels::set_paused(&tx, id, false)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(flash_redirect(
        "/",
        "Scheduling enabled. The scheduler can publish queued posts as soon as a slot is due.",
    ))
}
