//! End-to-end flows: import from disk, queue, publish against a mock transport.

use std::sync::Arc;

use nujijushi::bot::queue_alerts;
use nujijushi::config::Config;
use nujijushi::db::posts::NewMedia;
use nujijushi::model::{Channel, NsfwPolicy, PostState, QueueState, Rating};
use nujijushi::source::{NormalizedPost, PostAuthor};
use nujijushi::state::AppState;
use nujijushi::tg::mock::MockTransport;
use nujijushi::tg::publish::{PublishOutcome, publish_channel};
use nujijushi::tg::{AdminStatus, TgError, Transport};

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockTransport>,
}

fn harness(mock: MockTransport) -> Harness {
    harness_with_alerts(mock, false)
}

fn harness_with_alerts(mock: MockTransport, queue_alerts: bool) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    cfg.queue_alerts = queue_alerts;
    cfg.access.admins = vec![17, 9, 17, -1];
    let mock = Arc::new(mock);
    let transport: Arc<dyn Transport> = mock.clone();
    let state = AppState::open(cfg, Some(transport)).unwrap();
    Harness {
        _dir: dir,
        state,
        mock,
    }
}

impl Harness {
    async fn add_channel(&self, policy: NsfwPolicy) -> Channel {
        let mut channel = Channel::new("Test", "@test_channel");
        channel.nsfw_policy = policy;
        channel.interval_minutes = 30;
        channel.active_from = "00:00".into();
        channel.active_to = "23:59".into();
        channel.jitter_secs = 0;
        let id = self
            .state
            .db
            .call({
                let channel = channel.clone();
                move |conn| nujijushi::db::channels::insert(conn, &channel)
            })
            .await
            .unwrap();
        let mut stored = channel;
        stored.id = id;
        stored
    }

    async fn add_post(
        &self,
        label: &str,
        media: usize,
        is_video: bool,
        tags: &[&str],
        rating: Rating,
    ) -> i64 {
        let mut post = NormalizedPost::new("pixiv", label);
        post.url = Some(format!("https://www.pixiv.net/artworks/{label}"));
        post.title = Some(format!("Artwork {label}"));
        post.description = Some("A test description with <angle> brackets & ampersands.".into());
        post.rating = rating;
        post.tags = tags.iter().map(|t| t.to_string()).collect();
        post.author = Some(PostAuthor {
            source_id: format!("artist-{label}"),
            name: Some("Test Artist".into()),
            handle: Some("tester".into()),
            url: Some("https://www.pixiv.net/users/1".into()),
        });
        let media: Vec<NewMedia> = (0..media)
            .map(|index| NewMedia {
                blob_hash: format!("{label}-{index}").repeat(4),
                ext: if is_video { "mp4".into() } else { "png".into() },
                mime: None,
                width: Some(800),
                height: Some(600),
                bytes: 1024,
                is_video,
                poster_hash: None,
                sensitive: false,
                meta_json: None,
            })
            .collect();
        let created = label.to_string();
        let (post_id, _) = self
            .state
            .db
            .call(move |conn| nujijushi::db::posts::upsert(conn, &post, &media))
            .await
            .unwrap();
        let _ = created;
        post_id
    }

    async fn queue(&self, post_id: i64, channel: &Channel) {
        let channel = channel.clone();
        let outcome = nujijushi::queue::enqueue_checked(&self.state, &channel, post_id, 0)
            .await
            .unwrap();
        assert!(
            matches!(outcome, nujijushi::queue::EnqueueOutcome::Queued(_)),
            "unexpected enqueue outcome: {outcome:?}"
        );
    }

    async fn queue_state(&self, channel: &Channel) -> Vec<(QueueState, Option<String>)> {
        let channel_id = channel.id;
        self.state
            .db
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT state, last_error FROM queue WHERE channel_id = ?1 ORDER BY id",
                )?;
                let rows = stmt.query_map([channel_id], |row| {
                    Ok((
                        QueueState::parse(&row.get::<_, String>(0)?),
                        row.get::<_, Option<String>>(1)?,
                    ))
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            })
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn publishes_an_album_with_a_rendered_caption() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let post_id = harness
        .add_post("1", 3, false, &["original", "landscape"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;

    let outcome = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "got {outcome:?}"
    );

    let sends = harness.mock.sends();
    assert_eq!(sends.len(), 1, "one album for three photos");
    let send = &sends[0];
    assert_eq!(send.chat, "@test_channel");
    assert_eq!(send.media.len(), 3);
    // exactly one caption, on the first item
    assert!(send.media[0].caption.is_some());
    assert!(send.media[1..].iter().all(|m| m.caption.is_none()));
    let caption = send.media[0].caption.clone().unwrap();
    assert!(
        caption.contains("<b>Test Artist</b> (tester)"),
        "caption: {caption}"
    );
    assert!(
        !caption.contains('@'),
        "no Telegram mention in a caption: {caption}"
    );
    assert!(
        caption.contains("#original #landscape"),
        "caption: {caption}"
    );
    assert!(
        caption.contains("&lt;angle&gt;"),
        "html must be escaped: {caption}"
    );
    assert!(caption.contains("via <a href=\"https://www.pixiv.net/artworks/1\">Pixiv</a>"));

    let states = harness.queue_state(&channel).await;
    assert_eq!(states[0].0, QueueState::Sent);

    let audits = harness
        .state
        .db
        .call(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM sends WHERE ok = 1", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(
        audits, 1,
        "the send audit commits with the queue state (daily cap)"
    );

    let channel_id = channel.id;
    let stored = harness
        .state
        .db
        .call(move |conn| nujijushi::db::channels::get(conn, channel_id))
        .await
        .unwrap();
    assert!(stored.last_sent_at.is_some());
    assert!(stored.next_due_at.is_some());
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let post_id = harness
        .add_post("1", 2, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;

    let outcome = publish_channel(&harness.state, &channel, true, true)
        .await
        .unwrap();
    match outcome {
        PublishOutcome::DryRun(plan) => {
            assert_eq!(plan.media_count(), 2);
            assert!(plan.describe().contains("channel  #"));
        }
        other => panic!("expected a dry run, got {other:?}"),
    }
    assert_eq!(harness.mock.send_count(), 0);
    assert_eq!(harness.queue_state(&channel).await[0].0, QueueState::Queued);
}

#[tokio::test]
async fn nsfw_policy_reject_skips_the_item() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Reject).await;
    let post_id = harness
        .add_post("1", 1, false, &["explicit_content"], Rating::Explicit)
        .await;
    // force it into the queue despite policy, to test the send-time gate
    let channel_id = channel.id;
    harness
        .state
        .db
        .call(move |conn| nujijushi::db::queue::enqueue(conn, post_id, channel_id, 0))
        .await
        .unwrap();

    let outcome = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::AllSkipped(_)),
        "got {outcome:?}"
    );
    assert_eq!(harness.mock.send_count(), 0);
    let states = harness.queue_state(&channel).await;
    assert_eq!(states[0].0, QueueState::Skipped);
    assert!(states[0].1.as_deref().unwrap().contains("nsfw"));
}

#[tokio::test]
async fn spoiler_is_set_for_nsfw_channels_that_ask_for_it() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Spoil).await;
    let post_id = harness
        .add_post("1", 1, false, &["nude"], Rating::Explicit)
        .await;
    harness.queue(post_id, &channel).await;

    publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    let sends = harness.mock.sends();
    assert!(sends[0].media[0].spoiler, "spoiler flag must be set");
    let caption = sends[0].media[0].caption.clone().unwrap();
    assert!(caption.contains("#nsfw"), "caption: {caption}");
}

#[tokio::test]
async fn transient_failures_keep_the_item_queued() {
    // RetryPolicy::default() makes 4 attempts; give it 4 transient failures.
    let mock = MockTransport::with_failures(vec![
        TgError::Transient("blip".into()),
        TgError::Transient("blip".into()),
        TgError::Transient("blip".into()),
        TgError::Transient("blip".into()),
    ]);
    let harness = harness(mock);
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let post_id = harness
        .add_post("1", 1, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;

    let result = publish_channel(&harness.state, &channel, false, true).await;
    assert!(result.is_err(), "publish should surface the failure");
    let states = harness.queue_state(&channel).await;
    assert_eq!(
        states[0].0,
        QueueState::Queued,
        "transient failures retry later"
    );

    let channel_id = channel.id;
    let stored = harness
        .state
        .db
        .call(move |conn| nujijushi::db::channels::get(conn, channel_id))
        .await
        .unwrap();
    assert_eq!(stored.consecutive_failures, 1);
    assert!(!stored.paused);
}

#[tokio::test]
async fn ambiguous_send_failures_become_unknown_and_are_not_retried() {
    // A timeout after Telegram accepted the request must not be re-sent: the
    // item is unconfirmed, and only an operator can resolve it.
    let mock = MockTransport::with_script(vec![Some(TgError::Uncertain(
        "timeout after the request was sent".into(),
    ))]);
    let harness = harness(mock);
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let post_id = harness
        .add_post("1", 1, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;

    let result = publish_channel(&harness.state, &channel, false, true).await;
    assert!(result.is_err(), "an unconfirmed send is not a success");
    assert_eq!(
        harness.mock.send_calls(),
        1,
        "the ambiguous send must be attempted exactly once"
    );
    assert_eq!(
        harness.mock.send_count(),
        0,
        "no message id was confirmed, so nothing is recorded as sent"
    );
    let states = harness.queue_state(&channel).await;
    assert_eq!(
        states[0].0,
        QueueState::Unknown,
        "an unconfirmed send is surfaced for review"
    );
    assert!(
        states[0]
            .1
            .as_deref()
            .unwrap()
            .contains("send state unknown"),
        "got {:?}",
        states[0].1
    );

    let before = harness.mock.send_calls();
    let again = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    assert!(matches!(again, PublishOutcome::EmptyQueue), "got {again:?}");
    assert_eq!(
        harness.mock.send_calls(),
        before,
        "a later pass must never pick the unconfirmed item up again"
    );
}

#[tokio::test]
async fn permanent_failures_mark_the_item_failed_and_pause_after_three() {
    let mock = MockTransport::with_failures(vec![
        TgError::Permanent("chat not found".into()),
        TgError::Permanent("chat not found".into()),
        TgError::Permanent("chat not found".into()),
    ]);
    let harness = harness(mock);
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    for index in 0..3 {
        let post_id = harness
            .add_post(&index.to_string(), 1, false, &["original"], Rating::Sfw)
            .await;
        harness.queue(post_id, &channel).await;
        let _ = publish_channel(&harness.state, &channel, false, true).await;
    }
    let states = harness.queue_state(&channel).await;
    assert!(states.iter().all(|(state, _)| *state == QueueState::Failed));

    let channel_id = channel.id;
    let stored = harness
        .state
        .db
        .call(move |conn| nujijushi::db::channels::get(conn, channel_id))
        .await
        .unwrap();
    assert_eq!(stored.consecutive_failures, 3);
    assert!(stored.paused, "circuit breaker must pause the channel");
}

#[tokio::test]
async fn partial_delivery_is_never_resent_blindly() {
    // 12 photos = album of 10 + album of 2. The second message fails permanently.
    let mock = MockTransport::with_script(vec![None, Some(TgError::Permanent("too long".into()))]);
    let harness = harness_with_alerts(mock, true);
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let post_id = harness
        .add_post("1", 12, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;

    let result = publish_channel(&harness.state, &channel, false, true).await;
    assert!(result.is_err());
    let states = harness.queue_state(&channel).await;
    assert_eq!(
        states[0].0,
        QueueState::Unknown,
        "partial delivery must surface as unknown, not sent or failed"
    );
    assert!(states[0].1.as_deref().unwrap().contains("partial delivery"));
    let channel_id = channel.id;
    let queue_id = harness
        .state
        .db
        .call(move |conn| {
            nujijushi::db::channels::set_paused(conn, channel_id, true)?;
            Ok(nujijushi::db::queue::for_post(conn, post_id)?[0].0)
        })
        .await
        .unwrap();

    queue_alerts::check(&harness.state).await;
    let restarted = AppState::open(
        (*harness.state.cfg).clone(),
        harness.state.transport.clone(),
    )
    .unwrap();
    queue_alerts::check(&restarted).await;
    let texts = harness.mock.texts();
    assert_eq!(texts.len(), 2);
    for (_, text) in texts {
        assert!(text.contains("Unconfirmed send"));
        assert!(text.contains(&channel.alias));
        assert!(text.contains(&format!("Queue #{queue_id} · post #{post_id}")));
        assert!(text.contains("admin dashboard under Unconfirmed sends"));
    }
    assert_eq!(
        harness.queue_state(&channel).await[0].0,
        QueueState::Unknown
    );

    // and a later pass must not pick it up again
    let before = harness.mock.send_count();
    let again = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    assert!(matches!(again, PublishOutcome::EmptyQueue), "got {again:?}");
    assert_eq!(harness.mock.send_count(), before);

    harness
        .state
        .db
        .call(move |conn| nujijushi::db::queue::resolve_unknown(conn, queue_id, true, None))
        .await
        .unwrap();
    assert_eq!(harness.queue_state(&channel).await[0].0, QueueState::Sent);
    queue_alerts::check(&harness.state).await;
    assert_eq!(harness.mock.texts().len(), 2);
}

#[tokio::test]
async fn interruped_send_is_reconciled_as_unknown_and_not_resent() {
    let harness = harness_with_alerts(MockTransport::new(), true);
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let post_id = harness
        .add_post("1", 1, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;

    // simulate a crash between "sending" and "sent": stale lease in the past
    let channel_id = channel.id;
    harness
        .state
        .db
        .call(move |conn| {
            conn.execute(
                "UPDATE queue SET state = 'sending', lease_token = 'dead', lease_at = ?1
                 WHERE channel_id = ?2",
                rusqlite::params![
                    nujijushi::db::fmt_ts(chrono::Utc::now() - chrono::Duration::hours(1)),
                    channel_id
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    queue_alerts::check(&harness.state).await;
    assert!(
        harness.mock.texts().is_empty(),
        "a send in flight keeps the queue from alerting"
    );
    let fixed = nujijushi::queue::lease::reconcile_stale(&harness.state)
        .await
        .unwrap();
    assert_eq!(fixed, 1);
    assert_eq!(
        harness.queue_state(&channel).await[0].0,
        QueueState::Unknown
    );
    queue_alerts::check(&harness.state).await;
    assert_eq!(
        harness
            .mock
            .texts()
            .iter()
            .filter(|(_, text)| text.contains("Unconfirmed send"))
            .count(),
        2
    );

    let outcome = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::EmptyQueue),
        "got {outcome:?}"
    );
    assert_eq!(
        harness.mock.send_count(),
        0,
        "never re-send an unconfirmed message"
    );

    harness
        .state
        .db
        .call(move |conn| {
            use nujijushi::db::queue;
            let queue_id = queue::for_post(conn, post_id)?[0].0;
            queue::resolve_unknown(conn, queue_id, false, None)?;
            assert!(queue::acquire(conn, queue_id, "new-attempt")?);
            assert_eq!(
                queue::reconcile_stale(conn, chrono::Utc::now() + chrono::Duration::seconds(1))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    queue_alerts::check(&harness.state).await;
    let review_alerts = harness
        .mock
        .texts()
        .iter()
        .filter(|(_, text)| text.contains("Unconfirmed send"))
        .count();
    assert_eq!(
        review_alerts, 4,
        "a later unconfirmed send on the same item gets a new alert"
    );
}

#[tokio::test]
async fn empty_queue_alerts_survive_restart_and_rearm_even_between_checks() {
    let h = harness_with_alerts(MockTransport::new(), true);
    let channel = h.add_channel(NsfwPolicy::Allow).await;
    h.state
        .db
        .call(|conn| {
            let mut paused = Channel::new("Paused", "@paused");
            paused.paused = true;
            nujijushi::db::channels::insert(conn, &paused)?;
            let mut disabled = Channel::new("Disabled", "@disabled");
            disabled.enabled = false;
            nujijushi::db::channels::insert(conn, &disabled)?;
            Ok(())
        })
        .await
        .unwrap();
    let reopened = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();

    let mut opted_out = reopened.clone();
    Arc::make_mut(&mut opted_out.cfg).queue_alerts = false;
    queue_alerts::check(&opted_out).await;
    assert!(h.mock.texts().is_empty());

    tokio::join!(
        queue_alerts::check(&h.state),
        queue_alerts::check(&reopened)
    );
    queue_alerts::check(&reopened).await;
    let texts = h.mock.texts();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0].0, "9");
    assert_eq!(texts[1].0, "17");
    assert!(
        texts
            .iter()
            .all(|(_, text)| text.contains("Empty queue") && text.contains(&channel.alias))
    );

    let post_id = h.add_post("42", 1, false, &[], Rating::Sfw).await;
    h.queue(post_id, &channel).await;
    let published = publish_channel(&h.state, &channel, false, true)
        .await
        .unwrap();
    assert!(matches!(published, PublishOutcome::Published { .. }));
    drop(reopened);
    let restarted = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();
    queue_alerts::check(&restarted).await;
    queue_alerts::check(&restarted).await;
    assert_eq!(h.mock.texts().len(), 4);
}

#[tokio::test]
async fn failed_alert_is_handled_once_and_other_admins_still_get_it() {
    let mock = MockTransport::new().with_text_failure(TgError::Permanent("chat not found".into()));
    let h = harness_with_alerts(mock, true);
    let channel = h.add_channel(NsfwPolicy::Allow).await;

    queue_alerts::check(&h.state).await;
    queue_alerts::check(&h.state).await;
    let texts = h.mock.texts();
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0].0, "17");

    let post_id = h.add_post("42", 1, false, &[], Rating::Sfw).await;
    h.queue(post_id, &channel).await;
    let published = publish_channel(&h.state, &channel, false, true)
        .await
        .unwrap();
    assert!(matches!(published, PublishOutcome::Published { .. }));
}

#[tokio::test]
async fn import_from_disk_then_publish() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;

    // fixture: two images + a set-level post.json
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.png", "b.png"] {
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([10, 20, 30]));
        img.save(dir.path().join(name)).unwrap();
    }
    std::fs::write(
        dir.path().join("post.json"),
        serde_json::json!({
            "id": "disk-1",
            "artist": "Disk Artist",
            "caption": "imported from a folder",
            "tags": ["original", "test"],
            "source": "pixiv",
            "source_url": "https://www.pixiv.net/artworks/9"
        })
        .to_string(),
    )
    .unwrap();

    let outcome =
        nujijushi::jobs::import_target(&harness.state, dir.path().to_str().unwrap(), None)
            .await
            .unwrap();
    assert_eq!(outcome.post_ids.len(), 1);
    assert_eq!(outcome.files, 2);

    // blobs are content addressed and thumbnails exist
    let post = harness
        .state
        .db
        .call(move |conn| nujijushi::db::posts::get(conn, outcome.post_ids[0]))
        .await
        .unwrap();
    assert_eq!(post.media.len(), 2);
    assert_eq!(post.state, PostState::Approved);
    for media in &post.media {
        assert!(
            harness
                .state
                .blobs
                .path_for(&media.blob_hash, &media.ext)
                .exists(),
            "blob file missing for {}",
            media.blob_hash
        );
        assert!(harness.state.blobs.thumb_exists(&media.blob_hash));
    }

    harness.queue(post.id, &channel).await;
    let published = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    assert!(matches!(published, PublishOutcome::Published { .. }));
    let sends = harness.mock.sends();
    assert_eq!(sends[0].media.len(), 2);
    let caption = sends[0].media[0].caption.clone().unwrap();
    assert!(caption.contains("Disk Artist"));
    assert!(caption.contains("imported from a folder"));
    assert!(caption.contains("#original #test"));
}

#[tokio::test]
async fn duplicate_import_updates_instead_of_duplicating() {
    let harness = harness(MockTransport::new());
    let dir = tempfile::tempdir().unwrap();
    let img = image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]));
    img.save(dir.path().join("only.png")).unwrap();
    std::fs::write(
        dir.path().join("post.json"),
        serde_json::json!({"id": "dup-1", "artist": "A", "tags": ["x"]}).to_string(),
    )
    .unwrap();

    let first = nujijushi::jobs::import_target(&harness.state, dir.path().to_str().unwrap(), None)
        .await
        .unwrap();
    let second = nujijushi::jobs::import_target(&harness.state, dir.path().to_str().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(first.created, 1);
    assert_eq!(second.created, 0);
    assert_eq!(second.updated, 1);
    assert_eq!(first.post_ids, second.post_ids);

    let total = harness
        .state
        .db
        .call(|conn| nujijushi::db::posts::count(conn, None))
        .await
        .unwrap();
    assert_eq!(total, 1);
}

#[tokio::test]
async fn blocked_tag_never_reaches_the_queue() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    harness
        .state
        .db
        .call(|conn| {
            nujijushi::db::tags::set_rule(conn, "guro", nujijushi::model::TagRuleAction::Block)
        })
        .await
        .unwrap();
    let post_id = harness
        .add_post("1", 1, false, &["guro"], Rating::Sfw)
        .await;

    let outcome = nujijushi::queue::enqueue_checked(&harness.state, &channel, post_id, 0)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        nujijushi::queue::EnqueueOutcome::Skipped(_)
    ));
    assert!(harness.queue_state(&channel).await.is_empty());
    let _ = PostState::New;
}

#[tokio::test]
async fn admin_check_reports_rights() {
    let mock = MockTransport::new().with_admin(AdminStatus {
        title: Some("Test".into()),
        kind: "administrator".into(),
        is_admin: true,
        can_post: false,
    });
    let harness = harness(mock);
    let status = harness.mock.check_chat("@test_channel").await.unwrap();
    assert!(!status.can_post);
    assert_eq!(harness.mock.me().await.unwrap(), "@mock_bot");
}

async fn queue_id_for(harness: &Harness, channel: &Channel, post_id: i64) -> i64 {
    let channel_id = channel.id;
    harness
        .state
        .db
        .call(move |conn| {
            conn.query_row(
                "SELECT id FROM queue WHERE channel_id = ?1 AND post_id = ?2",
                rusqlite::params![channel_id, post_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn publish_item_sends_that_specific_post() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    let first = harness
        .add_post("1", 1, false, &["original"], Rating::Sfw)
        .await;
    let second = harness
        .add_post("2", 2, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(first, &channel).await;
    harness.queue(second, &channel).await;

    // publish the OLDER queued post explicitly, not whatever the selector picks
    let queue_id = queue_id_for(&harness, &channel, first).await;
    let outcome = nujijushi::tg::publish::publish_item(&harness.state, queue_id, false)
        .await
        .unwrap();
    match &outcome {
        PublishOutcome::Published {
            queue_id: id,
            post_id,
            ..
        } => {
            assert_eq!(*id, queue_id);
            assert_eq!(*post_id, first);
        }
        other => panic!("expected a publish, got {other:?}"),
    }
    let sends = harness.mock.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(
        sends[0].media.len(),
        1,
        "post 1 has one image, post 2 has two"
    );

    let states = harness.queue_state(&channel).await;
    assert_eq!(states[0].0, QueueState::Sent);
    assert_eq!(
        states[1].0,
        QueueState::Queued,
        "the other item is untouched"
    );
}

#[tokio::test]
async fn flush_publishes_queued_posts_in_order_and_keeps_policy_and_held_items() {
    let h = harness(MockTransport::new());
    let mut channel = h.add_channel(NsfwPolicy::Allow).await;
    let channel_id = channel.id;
    channel.next_due_at = Some(chrono::Utc::now() + chrono::Duration::days(3));
    channel.daily_cap = 1;
    channel.blocked_tags = "blocked".into();
    h.state
        .db
        .with(|conn| nujijushi::db::channels::update(conn, &channel))
        .unwrap();

    let mut queue_ids = Vec::new();
    for (label, priority, tags) in [
        ("first", 0, &[][..]),
        ("priority", 50, &[][..]),
        ("pinned", 0, &[][..]),
        ("veto", 100, &["blocked"][..]),
    ] {
        let post = h.add_post(label, 1, false, tags, Rating::Sfw).await;
        let id = h
            .state
            .db
            .with(|conn| nujijushi::db::queue::enqueue_new(conn, post, channel_id, priority))
            .unwrap()
            .unwrap();
        queue_ids.push(id);
    }
    h.state
        .db
        .with(|conn| nujijushi::db::queue::set_pinned(conn, queue_ids[2], true))
        .unwrap();
    let held = h.add_post("held", 1, false, &[], Rating::Sfw).await;
    assert!(matches!(
        nujijushi::queue::hold_for_preview(&h.state, &channel, held)
            .await
            .unwrap(),
        nujijushi::queue::EnqueueOutcome::Queued(_)
    ));

    let report = nujijushi::tg::publish::flush_channel(&h.state, channel_id, None)
        .await
        .unwrap();
    assert_eq!(report.describe(), "3 published · 1 skipped");
    let sends = h.mock.sends();
    assert_eq!(sends.len(), 3);
    for (send, label) in sends.iter().zip(["pinned", "priority", "first"]) {
        assert!(
            send.media[0]
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(label)
        );
    }
    let states: Vec<_> = h
        .queue_state(&channel)
        .await
        .into_iter()
        .map(|(state, _)| state)
        .collect();
    assert_eq!(
        states,
        [
            QueueState::Sent,
            QueueState::Sent,
            QueueState::Sent,
            QueueState::Skipped,
            QueueState::Held
        ]
    );
}

#[tokio::test]
async fn flush_stops_after_a_partial_delivery_and_never_resends_it() {
    let h = harness(MockTransport::with_script(vec![
        None,
        None,
        Some(TgError::Permanent("upload rejected".into())),
    ]));
    let channel = h.add_channel(NsfwPolicy::Allow).await;
    for (label, media) in [("first", 1), ("partial", 12), ("last", 1)] {
        let post = h.add_post(label, media, false, &[], Rating::Sfw).await;
        h.queue(post, &channel).await;
    }
    let error = nujijushi::tg::publish::flush_channel(&h.state, channel.id, None)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("1 published · 0 skipped"));
    assert_eq!(h.mock.send_count(), 2);
    let states = h.queue_state(&channel).await;
    assert_eq!(states[0].0, QueueState::Sent);
    assert_eq!(states[1].0, QueueState::Unknown);
    assert_eq!(states[2].0, QueueState::Queued);

    let report = nujijushi::tg::publish::flush_channel(&h.state, channel.id, None)
        .await
        .unwrap();
    assert_eq!(report.describe(), "1 published · 0 skipped");
    assert_eq!(h.mock.send_count(), 3);
    assert_eq!(h.queue_state(&channel).await[1].0, QueueState::Unknown);
}

#[tokio::test]
async fn publish_item_ignores_the_schedule_but_respects_policy() {
    let harness = harness(MockTransport::new());
    let mut channel = harness.add_channel(NsfwPolicy::Allow).await;
    // far in the future: the scheduler would never pick anything right now
    channel.next_due_at = Some(chrono::Utc::now() + chrono::Duration::days(3));
    // a tag this channel refuses, so the policy gate is what stops that post
    channel.blocked_tags = "explicit_content".into();
    harness
        .state
        .db
        .call({
            let channel = channel.clone();
            move |conn| nujijushi::db::channels::update(conn, &channel)
        })
        .await
        .unwrap();

    let vetoed = harness
        .add_post("bad", 1, false, &["explicit_content"], Rating::Sfw)
        .await;
    // queued by hand: enqueue_checked would already have refused it
    let channel_id = channel.id;
    harness
        .state
        .db
        .call(move |conn| nujijushi::db::queue::enqueue(conn, vetoed, channel_id, 0))
        .await
        .unwrap();
    let queue_id = queue_id_for(&harness, &channel, vetoed).await;
    let outcome = nujijushi::tg::publish::publish_item(&harness.state, queue_id, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::AllSkipped(_)),
        "channel policy still applies: {outcome:?}"
    );
    assert_eq!(harness.mock.send_count(), 0);

    // a policy-clean post goes out even though the channel is not due
    let clean = harness
        .add_post("ok", 1, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(clean, &channel).await;
    let queue_id = queue_id_for(&harness, &channel, clean).await;
    let outcome = nujijushi::tg::publish::publish_item(&harness.state, queue_id, false)
        .await
        .unwrap();
    assert!(
        matches!(outcome, PublishOutcome::Published { .. }),
        "manual publish ignores the schedule: {outcome:?}"
    );
    assert_eq!(harness.mock.send_count(), 1);

    // and a successful slot advances next_due_at like a scheduled one
    let channel_id = channel.id;
    let stored = harness
        .state
        .db
        .call(move |conn| nujijushi::db::channels::get(conn, channel_id))
        .await
        .unwrap();
    assert!(stored.next_due_at.is_some());
    assert!(
        stored.next_due_at.unwrap() < chrono::Utc::now() + chrono::Duration::days(2),
        "next slot was recomputed"
    );
}

/// A page full of vetoes must not starve the slot: the run has to look further
/// than the first batch of queued rows before giving up.
#[tokio::test]
async fn a_slot_reaches_past_a_full_page_of_vetoed_posts() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;

    let mut vetoed = Vec::new();
    for index in 0..60 {
        let post = harness
            .add_post(&format!("veto-{index}"), 1, false, &["art"], Rating::Sfw)
            .await;
        nujijushi::queue::enqueue_checked(&harness.state, &channel, post, 0)
            .await
            .unwrap();
        vetoed.push(post);
    }
    // negative priority sorts it behind the whole page of vetoes
    let keeper = harness
        .add_post("keeper", 1, false, &["art"], Rating::Sfw)
        .await;
    nujijushi::queue::enqueue_checked(&harness.state, &channel, keeper, -1)
        .await
        .unwrap();

    harness
        .state
        .db
        .call(move |conn| {
            for id in vetoed {
                nujijushi::db::posts::set_state(conn, id, PostState::Rejected)?;
            }
            Ok(())
        })
        .await
        .unwrap();

    let outcome = publish_channel(&harness.state, &channel, false, true)
        .await
        .unwrap();
    match outcome {
        PublishOutcome::Published { post_id, .. } => assert_eq!(post_id, keeper),
        other => panic!("expected the 61st post to publish, got {other:?}"),
    }
    assert_eq!(harness.mock.send_count(), 1);
}

/// The projection is the plan: order from the selection, times from the
/// scheduler. A 30-minute channel with no jitter puts three posts exactly 30
/// minutes apart, starting now — and reading it must not send or mark anything.
#[tokio::test]
async fn the_projection_says_when_each_queued_post_would_go_out() {
    let harness = harness(MockTransport::new());
    let channel = harness.add_channel(NsfwPolicy::Allow).await;
    for label in ["1", "2", "3"] {
        let post_id = harness
            .add_post(label, 1, false, &["original"], Rating::Sfw)
            .await;
        harness.queue(post_id, &channel).await;
    }

    let plan = nujijushi::tg::publish::project_queue(&harness.state, &channel, 10)
        .await
        .unwrap();
    assert_eq!(plan.len(), 3, "every queued item is projected");
    let now = chrono::Utc::now();
    assert!(
        (plan[0].slot - now).num_minutes().abs() <= 1,
        "a due channel starts now: {:?}",
        plan[0].slot
    );
    for pair in plan.windows(2) {
        assert_eq!(
            (pair[1].slot - pair[0].slot).num_minutes(),
            30,
            "one slot every 30 minutes"
        );
    }
    assert_eq!(harness.mock.send_count(), 0, "a projection never sends");
    let states = harness.queue_state(&channel).await;
    assert!(
        states.iter().all(|(state, _)| *state == QueueState::Queued),
        "and never marks anything"
    );
}

/// A paused channel has no ETA at all: nothing would go out.
#[tokio::test]
async fn a_paused_channel_projects_nothing() {
    let harness = harness(MockTransport::new());
    let mut channel = harness.add_channel(NsfwPolicy::Allow).await;
    channel.paused = true;
    let post_id = harness
        .add_post("1", 1, false, &["original"], Rating::Sfw)
        .await;
    harness.queue(post_id, &channel).await;
    let plan = nujijushi::tg::publish::project_queue(&harness.state, &channel, 10)
        .await
        .unwrap();
    assert!(plan.is_empty());
}
