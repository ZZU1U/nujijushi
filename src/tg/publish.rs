//! The publish pipeline: policy -> caption -> message plan -> lease -> send -> record.

use chrono::{Duration, Utc};
use rand::Rng;

use super::audit;
use super::policy::RetryPolicy;
use super::transport::{MediaKind, OutgoingMedia, SendOptions, TgError, Transport};
use crate::db::{queue as queue_db, tags as tag_db};
use crate::error::Result;
use crate::media::{MessageShape, plan_messages};
use crate::model::{Channel, Post, QueueState};
use crate::nsfw::{self, Decision};
use crate::queue::scheduler;
use crate::queue::selection::{Candidate, Selection};
use crate::render::{CaptionContext, RenderedCaption, build_context};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct PlannedMessage {
    pub shape: MessageShape,
    pub media: Vec<OutgoingMedia>,
}

#[derive(Debug)]
pub struct PublishPlan {
    pub channel: Channel,
    pub post: Post,
    pub queue_id: i64,
    pub decision: Decision,
    pub context: CaptionContext,
    pub caption: RenderedCaption,
    pub messages: Vec<PlannedMessage>,
}

impl PublishPlan {
    pub fn media_count(&self) -> usize {
        self.messages.iter().map(|m| m.media.len()).sum()
    }

    /// Human readable summary used by `push --dry-run` and the web UI.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "channel  #{} {} ({})\n",
            self.channel.id, self.channel.title, self.channel.tg_id
        ));
        out.push_str(&format!(
            "post     #{} {} / {}\n",
            self.post.id, self.post.source, self.post.source_id
        ));
        out.push_str(&format!("queue    #{}\n", self.queue_id));
        out.push_str(&format!(
            "policy   {} (nsfw={}, spoiler={})\n",
            self.decision.reason,
            self.decision.nsfw,
            self.caption_needs_spoiler()
        ));
        out.push_str(&format!("messages {}\n", self.messages.len()));
        for (index, message) in self.messages.iter().enumerate() {
            let kind = match message.shape {
                MessageShape::Single => "single",
                MessageShape::Album => "album",
            };
            out.push_str(&format!(
                "  {}. {kind} ({} item(s))",
                index + 1,
                message.media.len()
            ));
            if message.media.iter().any(|m| m.caption.is_some()) {
                out.push_str(" + caption");
            }
            out.push('\n');
            for media in &message.media {
                let size = std::fs::metadata(&media.path).map(|m| m.len()).unwrap_or(0);
                out.push_str(&format!(
                    "     {} {:.1} KiB{}\n",
                    media.path.display(),
                    size as f64 / 1024.0,
                    match media.kind {
                        MediaKind::Photo => "",
                        MediaKind::Video => " (video)",
                    }
                ));
            }
        }
        out.push_str(&format!(
            "caption  {}{}\n----\n{}\n----",
            self.caption.length,
            if self.caption.truncated {
                " (truncated)"
            } else {
                ""
            },
            self.caption.text
        ));
        out
    }

    fn caption_needs_spoiler(&self) -> bool {
        self.decision.spoiler()
    }
}

/// Build everything needed to publish a post, without touching Telegram.
pub fn build_plan(
    state: &AppState,
    channel: &Channel,
    post: &Post,
    queue_id: i64,
    decision: Decision,
) -> Result<PublishPlan> {
    let context = build_context(
        channel,
        post,
        decision.nsfw,
        decision.spoiler(),
        state.cfg.caption_limit,
    );
    let caption = state.renderer.render(channel, &context)?;
    let flags: Vec<bool> = post.media.iter().map(|m| m.is_video).collect();
    let groups = plan_messages(&flags);
    let spoiler = decision.spoiler();

    let messages = groups
        .iter()
        .map(|group| PlannedMessage {
            shape: group.shape,
            media: group
                .indices
                .iter()
                .filter_map(|index| post.media.get(*index))
                .enumerate()
                .map(|(position, media)| OutgoingMedia {
                    path: state.blobs.path_for(&media.blob_hash, &media.ext),
                    kind: if media.is_video {
                        MediaKind::Video
                    } else {
                        MediaKind::Photo
                    },
                    // Telegram renders a single captioned album item as the album
                    // caption; captions on several items break the layout.
                    caption: if group.caption && position == 0 && !caption.is_empty() {
                        Some(caption.text.clone())
                    } else {
                        None
                    },
                    spoiler,
                    thumb: media
                        .poster_hash
                        .as_ref()
                        .map(|hash| state.blobs.path_for(hash, "jpg")),
                })
                .collect(),
        })
        .filter(|message: &PlannedMessage| !message.media.is_empty())
        .collect();

    Ok(PublishPlan {
        channel: channel.clone(),
        post: post.clone(),
        queue_id,
        decision,
        context,
        caption,
        messages,
    })
}

#[derive(Debug, Default)]
pub struct SendOutcome {
    pub message_ids: Vec<i32>,
    pub chat_id: Option<i64>,
    pub chat_username: Option<String>,
    pub error: Option<TgError>,
}

/// Send every planned message. Stops at the first failure, keeping what went out.
pub async fn send_plan(
    plan: &PublishPlan,
    transport: &dyn Transport,
    policy: RetryPolicy,
) -> SendOutcome {
    let opts = SendOptions {
        silent: plan.channel.silent,
        protect: plan.channel.protect,
    };
    let chat = plan.channel.tg_id.clone();
    let mut outcome = SendOutcome::default();

    for message in &plan.messages {
        let media = message.media.clone();
        let batch = policy
            .run(|| transport.send_batch(&chat, media.clone(), opts))
            .await;
        match batch {
            Ok(sent) => {
                outcome.chat_id = outcome.chat_id.or(sent.chat_id);
                outcome.chat_username = outcome.chat_username.or(sent.chat_username);
                outcome.message_ids.extend(sent.message_ids);
            }
            Err(err) => {
                outcome.error = Some(err);
                return outcome;
            }
        }
    }
    outcome
}

#[derive(Debug)]
pub enum PublishOutcome {
    NotDue(String),
    EmptyQueue,
    AllSkipped(Vec<String>),
    Published {
        queue_id: i64,
        post_id: i64,
        message_ids: Vec<i32>,
    },
    DryRun(Box<PublishPlan>),
}

impl PublishOutcome {
    pub fn describe(&self) -> String {
        match self {
            PublishOutcome::NotDue(reason) => format!("not due: {reason}"),
            PublishOutcome::EmptyQueue => "queue is empty".to_string(),
            PublishOutcome::AllSkipped(reasons) => {
                format!(
                    "nothing publishable ({} skipped): {}",
                    reasons.len(),
                    reasons.join("; ")
                )
            }
            PublishOutcome::Published {
                queue_id,
                post_id,
                message_ids,
            } => format!(
                "published queue #{queue_id} (post #{post_id}), message ids {:?}",
                message_ids
            ),
            PublishOutcome::DryRun(plan) => format!("dry run\n{}", plan.describe()),
        }
    }
}
/// How many queued rows one policy pass looks at before asking for more.
const CANDIDATE_PAGE: i64 = 60;

/// Publish the next post for a channel (dry run still selects and renders).
pub async fn publish_channel(
    state: &AppState,
    channel: &Channel,
    dry_run: bool,
    force: bool,
) -> Result<PublishOutcome> {
    let now = Utc::now();
    let sent_today = {
        let since = scheduler::local_day_start(channel, now);
        let channel_id = channel.id;
        state
            .db
            .call(move |conn| queue_db::sends_since(conn, channel_id, since))
            .await?
    };
    if !force && !scheduler::slot_allowed(channel, now, sent_today) {
        return Ok(PublishOutcome::NotDue(scheduler::slot_reason(
            channel, now, sent_today,
        )));
    }

    let rules = state.db.call(|conn| tag_db::TagRules::load(conn)).await?;
    let want = channel.posts_per_slot.max(1) as usize;
    let mut candidates: Vec<(crate::model::QueueItem, Post, Decision, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut saw_queued = false;

    // Fill the *slot*, not just the first page: a veto marks the row `skipped`,
    // which removes it from the queued set, so the next pass sees fresh rows
    // instead of the run giving up after one page of unusable candidates.
    while candidates.len() < want {
        let channel_id = channel.id;
        let page = state
            .db
            .call(move |conn| {
                queue_db::list(conn, channel_id, &[QueueState::Queued], CANDIDATE_PAGE)
            })
            .await?;
        if page.is_empty() {
            break;
        }
        saw_queued = true;
        let mut progressed = false;
        for item in page {
            let post_id = item.post_id;
            let (post, revision) = match state
                .db
                .call(move |conn| crate::db::media_merges::publish_snapshot(conn, post_id))
                .await
            {
                Ok(snapshot) => snapshot,
                Err(err) => {
                    tracing::warn!(post = post_id, "cannot load queued post: {err}");
                    continue;
                }
            };

            let decision = if post.state == crate::model::PostState::Rejected {
                Decision {
                    verdict: nsfw::Verdict::Reject("post was rejected by the curator".into()),
                    nsfw: post.rating.is_nsfw(),
                    reason: "post was rejected by the curator".into(),
                }
            } else {
                nsfw::evaluate(&post, channel, &rules)
            };

            if let nsfw::Verdict::Reject(reason) = &decision.verdict {
                let queue_id = item.id;
                let reason_owned = reason.clone();
                state
                    .db
                    .call(move |conn| queue_db::skip_with_reason(conn, queue_id, &reason_owned))
                    .await?;
                skipped.push(format!("queue #{queue_id}: {reason}"));
                progressed = true;
                continue;
            }
            candidates.push((item, post, decision, revision));
            progressed = true;
            if candidates.len() >= want {
                break;
            }
        }
        if !progressed {
            break;
        }
    }

    if !saw_queued {
        return Ok(PublishOutcome::EmptyQueue);
    }
    if candidates.is_empty() {
        return Ok(PublishOutcome::AllSkipped(skipped));
    }

    let cooldown = channel.artist_cooldown.max(0) as usize;
    let (recent_authors, recent_tags) = {
        let channel_id = channel.id;
        state
            .db
            .call(move |conn| {
                let authors = queue_db::recent_authors(conn, channel_id, cooldown.max(1) as i64)?;
                let tags = queue_db::recent_tags(conn, channel_id, 3)?;
                Ok((authors, tags))
            })
            .await?
    };
    let selection = Selection {
        artist_cooldown: cooldown,
        ..Selection::default()
    }
    .with_history(recent_authors, recent_tags);

    let snapshot: Vec<Candidate> = candidates
        .iter()
        .map(|(item, post, _, _)| Candidate {
            queue_id: item.id,
            post_id: item.post_id,
            author_id: post.author.as_ref().map(|a| a.id),
            tags: post.tags.clone(),
            priority: item.priority,
            pinned: item.pinned,
            added_at: item.added_at,
        })
        .collect();

    // keep the thread rng out of the await chain so the future stays Send
    let index = {
        let mut rng = rand::rng();
        selection.choose(&snapshot, &mut rng)
    };
    let Some(index) = index else {
        return Ok(PublishOutcome::EmptyQueue);
    };
    let (item, post, decision, revision) = candidates.swap_remove(index);

    let plan = build_plan(state, channel, &post, item.id, decision)?;
    execute_plan(state, plan, revision, dry_run).await
}

/// Publish one specific queue item, ignoring the channel's schedule.
///
/// Used by the web "publish now" button; the policy checks still apply and a
/// successful send advances `next_due_at` exactly like a scheduled slot.
pub async fn publish_item(
    state: &AppState,
    queue_id: i64,
    dry_run: bool,
) -> Result<PublishOutcome> {
    let item = state
        .db
        .call(move |conn| queue_db::get(conn, queue_id))
        .await?;
    if item.state != QueueState::Queued {
        return Ok(PublishOutcome::NotDue(format!(
            "queue #{queue_id} is {}, not queued",
            item.state
        )));
    }
    let channel = {
        let id = item.channel_id;
        state
            .db
            .call(move |conn| crate::db::channels::get(conn, id))
            .await?
    };
    let (post, revision) = {
        let id = item.post_id;
        state
            .db
            .call(move |conn| crate::db::media_merges::publish_snapshot(conn, id))
            .await?
    };
    if post.state == crate::model::PostState::Rejected {
        return Ok(PublishOutcome::AllSkipped(vec![format!(
            "queue #{queue_id}: the post was rejected by the curator"
        )]));
    }

    let rules = state.db.call(|conn| tag_db::TagRules::load(conn)).await?;
    let decision = nsfw::evaluate(&post, &channel, &rules);
    if let nsfw::Verdict::Reject(reason) = &decision.verdict {
        let reason = reason.clone();
        let note = format!("queue #{queue_id}: {reason}");
        state
            .db
            .call(move |conn| queue_db::skip_with_reason(conn, queue_id, &reason))
            .await?;
        return Ok(PublishOutcome::AllSkipped(vec![note]));
    }

    let plan = build_plan(state, &channel, &post, queue_id, decision)?;
    execute_plan(state, plan, revision, dry_run).await
}

/// Lease a queue item, send its planned messages and record the outcome.
async fn execute_plan(
    state: &AppState,
    plan: PublishPlan,
    revision: String,
    dry_run: bool,
) -> Result<PublishOutcome> {
    if dry_run {
        return Ok(PublishOutcome::DryRun(Box::new(plan)));
    }
    let channel = &plan.channel;
    let queue_id = plan.queue_id;
    let post_id = plan.post.id;
    let transport = state.require_transport()?;
    let Some(_token) = crate::queue::lease::acquire_for_gallery(
        state,
        queue_id,
        channel.tg_id.clone(),
        post_id,
        revision,
    )
    .await?
    else {
        return Ok(PublishOutcome::NotDue(
            "item is no longer queued or its outgoing gallery changed".to_string(),
        ));
    };

    let outcome = send_plan(&plan, transport.as_ref(), RetryPolicy::default()).await;
    let channel_id = channel.id;
    let caption_text = plan.caption.text.clone();

    if outcome.error.is_none() {
        let ids = outcome.message_ids.clone();
        let joined = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sent_at = Utc::now();
        let jitter = rand::rng().random_range(0..=channel.jitter_secs.max(0));
        let next_due = scheduler::next_due(channel, sent_at, jitter);
        let published_ids = ids.clone();
        let target = channel.tg_id.clone();
        state
            .db
            .call(move |conn| {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                queue_db::mark_sent(&tx, queue_id, &joined, sent_at)?;
                crate::db::channels::record_success(&tx, channel_id, sent_at, next_due)?;
                crate::db::reputation::record_publication(
                    &tx,
                    &plan.post,
                    &target,
                    outcome.chat_id,
                    outcome.chat_username.as_deref(),
                    &published_ids,
                )?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        audit::record_success(
            state,
            audit::SendAttempt {
                channel_id,
                queue_id,
                post_id,
                caption: Some(&caption_text),
                message_ids: &ids,
            },
        )
        .await?;
        return Ok(PublishOutcome::Published {
            queue_id,
            post_id,
            message_ids: ids,
        });
    }

    let error = outcome.error.clone().expect("checked above");
    let partial = !outcome.message_ids.is_empty();

    if partial {
        // Some messages are live: the result is ambiguous, never re-send blindly.
        let detail = format!(
            "partial delivery: {} message(s) sent before: {error}",
            outcome.message_ids.len()
        );
        let ids = outcome
            .message_ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        state
            .db
            .call(move |conn| {
                queue_db::mark_partial(
                    conn,
                    queue_id,
                    &detail,
                    &ids,
                    outcome.chat_id,
                    outcome.chat_username.as_deref(),
                )
            })
            .await?;
    } else if error.is_retryable() {
        crate::queue::lease::release(state, queue_id).await?;
    } else {
        let detail = error.to_string();
        state
            .db
            .call(move |conn| queue_db::mark_failed(conn, queue_id, &detail))
            .await?;
    }

    let failures =
        audit::record_failure(state, channel_id, queue_id, post_id, &error.to_string()).await?;
    if failures >= 3 {
        tracing::error!(
            channel = channel_id,
            "pausing channel after {failures} consecutive failures"
        );
    }
    Err(crate::error::Error::Telegram(error.to_string()))
}

#[derive(Debug, Clone)]
pub struct PreviewItem {
    pub queue_id: i64,
    pub post_id: i64,
    pub title: String,
    pub caption: String,
    pub nsfw: bool,
    pub spoiler: bool,
    pub media: usize,
    pub messages: usize,
    pub reason: String,
    /// When this slot would fire if nobody interferes.
    pub eta: chrono::DateTime<Utc>,
}

/// One queued item with the slot the scheduler would give it.
#[derive(Debug, Clone, PartialEq)]
pub struct Projected {
    pub queue_id: i64,
    pub post_id: i64,
    pub slot: chrono::DateTime<Utc>,
}

/// What would go out, and when, if nobody interferes.
///
/// The order is the same seeded `Selection::simulate` that `preview` shows; the
/// slots come from `scheduler::upcoming_slots` (interval, active window, daily
/// cap). Posts the channel's policy would veto never take a slot, so they are
/// absent: an ETA is a plan, not a promise.
pub async fn project_queue(
    state: &AppState,
    channel: &Channel,
    count: usize,
) -> Result<Vec<Projected>> {
    Ok(plan_ahead(state, channel, count)
        .await?
        .into_iter()
        .map(|(item, _post, _decision, slot)| Projected {
            queue_id: item.id,
            post_id: item.post_id,
            slot,
        })
        .collect())
}

/// The queued items that would go out, in order, each with its slot.
async fn plan_ahead(
    state: &AppState,
    channel: &Channel,
    count: usize,
) -> Result<
    Vec<(
        crate::model::QueueItem,
        Post,
        Decision,
        chrono::DateTime<Utc>,
    )>,
> {
    let items = {
        let channel_id = channel.id;
        state
            .db
            .call(move |conn| queue_db::list(conn, channel_id, &[QueueState::Queued], 200))
            .await?
    };
    let rules = state.db.call(|conn| tag_db::TagRules::load(conn)).await?;

    let mut candidates: Vec<(crate::model::QueueItem, Post, Decision)> = Vec::new();
    for item in items {
        let post_id = item.post_id;
        let Ok(post) = state
            .db
            .call(move |conn| crate::db::media_merges::outgoing_post(conn, post_id))
            .await
        else {
            continue;
        };
        let decision = nsfw::evaluate(&post, channel, &rules);
        if decision.is_allowed() {
            candidates.push((item, post, decision));
        }
    }

    let cooldown = channel.artist_cooldown.max(0) as usize;
    let (recent_authors, recent_tags) = {
        let channel_id = channel.id;
        state
            .db
            .call(move |conn| {
                let authors = queue_db::recent_authors(conn, channel_id, cooldown.max(1) as i64)?;
                let tags = queue_db::recent_tags(conn, channel_id, 3)?;
                Ok((authors, tags))
            })
            .await?
    };
    let selection = Selection {
        artist_cooldown: cooldown,
        ..Selection::default()
    }
    .with_history(recent_authors, recent_tags);

    let snapshot: Vec<Candidate> = candidates
        .iter()
        .map(|(item, post, _)| Candidate {
            queue_id: item.id,
            post_id: item.post_id,
            author_id: post.author.as_ref().map(|a| a.id),
            tags: post.tags.clone(),
            priority: item.priority,
            pinned: item.pinned,
            added_at: item.added_at,
        })
        .collect();

    let seed = channel.id as u64 ^ 0x5eed;
    let order = selection.simulate(&snapshot, seed, count.min(candidates.len()));

    let now = Utc::now();
    let sent_today = {
        let since = scheduler::local_day_start(channel, now);
        let channel_id = channel.id;
        state
            .db
            .call(move |conn| queue_db::sends_since(conn, channel_id, since))
            .await?
    };
    let slots = scheduler::upcoming_slots(channel, now, sent_today, order.len());

    let mut out = Vec::new();
    for (index, queue_id) in order.iter().enumerate() {
        // fewer slots than items (cap reached, window closed): stop there
        let Some(slot) = slots.get(index) else {
            break;
        };
        if let Some((item, post, decision)) = candidates.iter().find(|(i, _, _)| i.id == *queue_id)
        {
            out.push((item.clone(), post.clone(), decision.clone(), *slot));
        }
    }
    Ok(out)
}

/// Render what the next `count` slots would publish (deterministic per channel).
pub async fn preview_channel(
    state: &AppState,
    channel: &Channel,
    count: usize,
) -> Result<Vec<PreviewItem>> {
    let mut out = Vec::new();
    for (item, post, decision, slot) in plan_ahead(state, channel, count).await? {
        let plan = build_plan(state, channel, &post, item.id, decision.clone())?;
        out.push(PreviewItem {
            queue_id: item.id,
            post_id: post.id,
            title: post
                .title
                .clone()
                .or_else(|| post.description.clone())
                .unwrap_or_else(|| format!("{} / {}", post.source, post.source_id)),
            caption: plan.caption.text.clone(),
            nsfw: decision.nsfw,
            spoiler: decision.spoiler(),
            media: plan.media_count(),
            messages: plan.messages.len(),
            reason: decision.reason.clone(),
            eta: slot,
        });
    }
    Ok(out)
}

/// Slots for a channel: how long until the next one is due.
pub fn next_slot_hint(channel: &Channel, now: chrono::DateTime<Utc>) -> String {
    match channel.next_due_at {
        Some(due) if due > now => format!("in {}", format_duration(due - now)),
        Some(_) => "due now".to_string(),
        None => "first slot on the next tick".to_string(),
    }
}

pub fn format_duration(delta: Duration) -> String {
    let minutes = delta.num_minutes();
    if minutes < 1 {
        return format!("{}s", delta.num_seconds().max(0));
    }
    let hours = minutes / 60;
    if hours < 1 {
        return format!("{minutes}m");
    }
    let days = hours / 24;
    if days < 1 {
        return format!("{}h {:02}m", hours, minutes % 60);
    }
    format!("{}d {}h", days, hours % 24)
}

/// How an estimated slot reads in a listing, in **local** time: `14:30` for
/// today, `Tue 14:30` within a week, `03-04 14:30` beyond it.
///
/// One formatter, so the queue page, the post page, the preview and the CLI all
/// put the same instant in front of the user.
pub fn slot_label(slot: chrono::DateTime<Utc>, now: chrono::DateTime<Utc>) -> String {
    let local = slot.with_timezone(&chrono::Local);
    let today = now.with_timezone(&chrono::Local).date_naive();
    let days = (local.date_naive() - today).num_days();
    match days {
        0 => local.format("%H:%M").to_string(),
        1..=6 => local.format("%a %H:%M").to_string(),
        _ => local.format("%m-%d %H:%M").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn slot_labels_are_relative_to_today() {
        let now = Utc.with_ymd_and_hms(2025, 5, 1, 12, 0, 0).unwrap();
        let today = now + Duration::hours(2);
        let tomorrow = now + Duration::days(1);
        let next_week = now + Duration::days(9);

        // today is just the time, so the column stays narrow
        assert_eq!(slot_label(today, now).len(), 5, "HH:MM");
        assert_ne!(slot_label(tomorrow, now).len(), 5, "a day is named");
        assert_eq!(slot_label(next_week, now).len(), 11, "MM-DD HH:MM");
        // the label is the *local* time, not the UTC instant
        assert_eq!(
            slot_label(today, now),
            today
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        );
    }
}
