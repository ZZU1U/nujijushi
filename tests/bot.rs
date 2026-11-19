//! Bot interface: access control, commands, discovery and Telegram-post ingest.
//! Everything runs against the mock transport, so no network and no real token.

use std::sync::Arc;

use nujijushi::bot::Bot;
use nujijushi::config::Config;
use nujijushi::db::posts::NewMedia;
use nujijushi::model::{Channel, NsfwPolicy, QueueState, Rating};
use nujijushi::source::{NormalizedPost, PostAuthor};
use nujijushi::state::AppState;
use nujijushi::tg::mock::MockTransport;
use nujijushi::tg::{
    BotFile, BotMessage, ChatInfo, CopiedMessage, FileKind, MemberChange, TgError, Transport,
    UpdateKind,
};

const ADMIN: i64 = 1;
const GUEST: i64 = 99;

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockTransport>,
}

fn harness(mock: MockTransport, config: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config).unwrap();
    let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    let mock = Arc::new(mock);
    let transport: Arc<dyn Transport> = mock.clone();
    let state = AppState::open(cfg, Some(transport)).unwrap();
    Harness {
        _dir: dir,
        state,
        mock,
    }
}

const OPEN_CONFIG: &str = r#"
[telegram]
bot_token = "1:test"
[access]
admins = [1]
public = true
allow_import = true
allow_browse = true
allow_queue = true
allow_publish = true
allow_curate = true
import_quota_per_day = 20
"#;

fn user(id: i64) -> nujijushi::tg::BotUser {
    nujijushi::tg::BotUser {
        id,
        username: Some(format!("user{id}")),
        first_name: Some("Test".into()),
        is_bot: false,
    }
}

fn private_message(from: i64, id: i32, body: &str) -> BotMessage {
    BotMessage {
        chat_id: from,
        chat_is_private: true,
        message_id: id,
        from: Some(user(from)),
        text: Some(body.to_string()),
        caption: None,
        files: Vec::new(),
        media_group_id: None,
        forward_from_chat_id: None,
        forward_from_chat_title: None,
        forward_from_chat_username: None,
        forward_from_message_id: None,
    }
}

fn photo(file_id: &str) -> BotFile {
    BotFile {
        file_id: file_id.to_string(),
        size: 1024,
        kind: FileKind::Photo,
        mime: Some("image/jpeg".into()),
    }
}

impl Harness {
    async fn send(&self, message: BotMessage) {
        let bot = Bot::new(&self.state).await.unwrap();
        bot.handle(nujijushi::tg::BotUpdate {
            id: 1,
            kind: UpdateKind::Message(message),
        })
        .await
        .unwrap();
    }

    async fn send_update(&self, kind: UpdateKind) {
        let bot = Bot::new(&self.state).await.unwrap();
        bot.handle(nujijushi::tg::BotUpdate { id: 1, kind })
            .await
            .unwrap();
    }

    fn last_text(&self) -> String {
        self.mock.last_text().unwrap_or_default()
    }

    fn command_names(&self, id: i64) -> Vec<&'static str> {
        self.mock
            .command_menu(Some(id))
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    async fn job(&self) -> Option<nujijushi::db::jobs::Job> {
        let jobs = self
            .state
            .db
            .call(|conn| nujijushi::db::jobs::list(conn, 5))
            .await
            .unwrap();
        jobs.into_iter().next()
    }

    async fn job_count(&self) -> usize {
        self.state
            .db
            .call(|conn| nujijushi::db::jobs::list(conn, 20))
            .await
            .unwrap()
            .len()
    }

    async fn add_channel(&self, alias: &str, policy: NsfwPolicy) -> Channel {
        let mut channel = Channel::new("Test Channel", "@test_channel");
        channel.alias = alias.into();
        channel.nsfw_policy = policy;
        channel.active_from = "00:00".into();
        channel.active_to = "23:59".into();
        channel.tz = "UTC".into();
        channel.jitter_secs = 0;
        let to_insert = channel.clone();
        let id = self
            .state
            .db
            .call(move |conn| nujijushi::db::channels::insert(conn, &to_insert))
            .await
            .unwrap();
        let mut stored = channel;
        stored.id = id;
        stored
    }

    async fn add_post(&self, label: &str, media: usize, tags: &[&str]) -> i64 {
        let mut post = NormalizedPost::new("pixiv", label);
        post.title = Some(format!("Artwork {label}"));
        post.description = Some("A test description.".into());
        post.rating = Rating::Sfw;
        post.tags = tags.iter().map(|t| t.to_string()).collect();
        post.author = Some(PostAuthor {
            source_id: format!("artist-{label}"),
            name: Some("Test Artist".into()),
            handle: None,
            url: None,
        });
        let media: Vec<NewMedia> = (0..media)
            .map(|index| NewMedia {
                blob_hash: format!("{label}-{index}").repeat(8),
                ext: "png".into(),
                mime: None,
                width: Some(800),
                height: Some(600),
                bytes: 1024,
                is_video: false,
                poster_hash: None,
                sensitive: false,
                meta_json: None,
            })
            .collect();
        let (post_id, _) = self
            .state
            .db
            .call(move |conn| nujijushi::db::posts::upsert(conn, &post, &media))
            .await
            .unwrap();
        post_id
    }
}

#[tokio::test]
async fn reputation_reviews_and_queueing_use_admin_permissions() {
    use nujijushi::db::{posts, reputation, users};
    use nujijushi::model::PostState;

    let h = harness(
        MockTransport::new(),
        &format!("{OPEN_CONFIG}\n[bot]\nguest_reputation = true"),
    );
    h.send(private_message(GUEST, 1, "/whoami")).await;
    assert!(h.last_text().contains("Reputation: 0"));
    let post = h.add_post("5", 1, &["original"]).await;
    h.state
        .db
        .call(move |conn| reputation::register_new(conn, post, GUEST))
        .await
        .unwrap();
    let channel = h.add_channel("main", NsfwPolicy::Reject).await;

    h.send(private_message(GUEST, 2, &format!("/approve {post}")))
        .await;
    assert!(h.last_text().contains("only an admin"));
    h.send(private_message(GUEST, 3, &format!("/push main {post}")))
        .await;
    assert!(h.last_text().contains("approve the post first"));
    h.state
        .db
        .call(move |conn| posts::set_rating(conn, post, Rating::Explicit))
        .await
        .unwrap();
    h.send(private_message(ADMIN, 4, &format!("/push main {post}")))
        .await;
    assert!(h.last_text().contains("skipped"));
    assert_eq!(
        h.state
            .db
            .with(|conn| Ok(posts::get(conn, post)?.state))
            .unwrap(),
        PostState::New
    );
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        0
    );

    h.state
        .db
        .call(move |conn| posts::set_rating(conn, post, Rating::Sfw))
        .await
        .unwrap();
    h.send(private_message(ADMIN, 5, &format!("/push main {post}")))
        .await;
    assert!(h.last_text().contains("queued as"));
    assert_eq!(
        h.state
            .db
            .with(|conn| Ok(posts::get(conn, post)?.state))
            .unwrap(),
        PostState::Approved
    );
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        1000
    );
    assert_eq!(h.mock.send_count(), 0);

    nujijushi::bot::reputation_notifications::check(&h.state).await;
    assert!(
        h.mock
            .texts()
            .iter()
            .any(|(chat, text)| chat == &GUEST.to_string() && text.contains("+10 · balance 10"))
    );

    h.send(private_message(
        ADMIN,
        6,
        &format!("/approve --points 12 {post}"),
    ))
    .await;
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        1200
    );
    h.mock
        .as_ref()
        .clone()
        .with_text_failure(TgError::Permanent("DM failed".into()));
    let before = h.mock.texts().len();
    nujijushi::bot::reputation_notifications::check(&h.state).await;
    nujijushi::bot::reputation_notifications::check(&h.state).await;
    assert_eq!(h.mock.texts().len(), before);
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        1200
    );

    h.state
        .db
        .call(move |conn| users::set_role(conn, GUEST, "admin"))
        .await
        .unwrap();
    h.send(private_message(GUEST, 7, "/whoami")).await;
    assert!(h.last_text().contains("Reputation: ∞"));
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        1200
    );
    assert_eq!(
        h.state
            .db
            .with(|conn| nujijushi::db::queue::count_state(conn, channel.id, QueueState::Queued))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn guest_links_and_forwards_find_deliveries_across_channels() {
    use nujijushi::db::{channels, reputation};
    use nujijushi::queue::EnqueueOutcome;
    use nujijushi::tg::publish::{PublishOutcome, publish_item};

    let mock = MockTransport::new().with_chat(ChatInfo {
        id: -100123,
        title: Some("Public".into()),
        username: Some("test_channel".into()),
        is_channel: true,
        is_private: false,
    });
    let h = harness(
        mock,
        &format!("{OPEN_CONFIG}\n[bot]\nguest_reputation = true"),
    );
    let public = h.add_channel("public", NsfwPolicy::Allow).await;
    let mut private = Channel::new("Private", "-100456");
    private.nsfw_policy = NsfwPolicy::Allow;
    private.id = h
        .state
        .db
        .with(|conn| channels::insert(conn, &private))
        .unwrap();

    let mut sent = Vec::new();
    for (channel, source_id) in [(&public, "1"), (&private, "2")] {
        let post = h.add_post(source_id, 1, &["original"]).await;
        let EnqueueOutcome::Queued(queue) =
            nujijushi::queue::enqueue_checked(&h.state, channel, post, 0)
                .await
                .unwrap()
        else {
            panic!("expected queued post");
        };
        let PublishOutcome::Published { message_ids, .. } =
            publish_item(&h.state, queue, false).await.unwrap()
        else {
            panic!("expected published post");
        };
        sent.push(message_ids[0]);
    }

    let public_url = format!("https://t.me/test_channel/{}", sent[0]);
    for _ in 0..2 {
        let result = nujijushi::jobs::import_target(&h.state, &public_url, Some(GUEST))
            .await
            .unwrap();
        assert!(result.summary().contains(&public_url));
    }
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        -100
    );
    let private_url = format!("https://t.me/c/456/{}", sent[1]);
    let mut forward = private_message(GUEST, 994, "");
    forward.forward_from_chat_id = Some(-100456);
    forward.forward_from_message_id = Some(sent[1]);
    forward.files = vec![photo("not-downloaded")];
    h.send(forward).await;
    assert!(h.last_text().contains(&private_url));
    assert!(h.mock.downloads().is_empty());
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, GUEST))
            .unwrap(),
        -200
    );

    let before = h.mock.texts().len();
    let also = AppState::open(h.state.cfg.as_ref().clone(), h.state.transport.clone()).unwrap();
    tokio::join!(
        nujijushi::bot::reputation_notifications::check(&h.state),
        nujijushi::bot::reputation_notifications::check(&also),
    );
    let texts = h.mock.texts();
    assert_eq!(texts.len() - before, 2);
    assert!(
        texts
            .iter()
            .any(|(_, text)| text.contains(&private_url) && text.contains("Reputation −1"))
    );
    assert!(
        texts
            .iter()
            .any(|(_, text)| text.contains(&public_url) && text.contains("Reputation −1"))
    );
}

#[tokio::test]
async fn guest_import_becomes_a_job_and_reports_back() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness.mock.script_copy(
        "@night_art",
        42,
        CopiedMessage {
            chat_id: GUEST,
            message_id: 900,
            media_group_id: None,
            caption: Some("quiet harbour #art #nsfw".into()),
            files: vec![photo("f1")],
        },
    );

    harness
        .send(private_message(GUEST, 10, "https://t.me/night_art/42"))
        .await;

    let job = harness.job().await.expect("a job was created");
    assert_eq!(job.payload.as_deref(), Some("https://t.me/night_art/42"));
    assert_eq!(job.submitted_by, Some(GUEST), "the submitter is recorded");
    assert!(harness.last_text().contains("Importing"));

    // the worker picks it up and finishes it
    let job = harness
        .state
        .db
        .call(move |conn| nujijushi::db::jobs::claim_pending(conn, 1))
        .await
        .unwrap()
        .pop()
        .expect("claimed");
    let outcome = nujijushi::jobs::run_job(&harness.state, &job)
        .await
        .unwrap();
    let nujijushi::jobs::JobReport::Import(outcome) = outcome else {
        panic!("an import job reports an import outcome");
    };
    assert_eq!(outcome.post_ids.len(), 1);

    let post = harness
        .state
        .db
        .call({
            let id = outcome.post_ids[0];
            move |conn| nujijushi::db::posts::get(conn, id)
        })
        .await
        .unwrap();
    assert_eq!(post.source, "telegram");
    assert_eq!(post.source_id, "night_art/42");
    assert_eq!(post.rating, Rating::Explicit, "#nsfw in the caption");
    assert!(post.tags.contains(&"art".to_string()));
    assert_eq!(post.media.len(), 1);
    assert!(post.url.as_deref().unwrap().contains("t.me/night_art/42"));

    // the temporary copy is deleted and the submitter is told what happened
    assert_eq!(harness.mock.deleted(), vec![(GUEST.to_string(), 900)]);
    assert!(
        harness.last_text().contains("imported"),
        "got: {}",
        harness.last_text()
    );
}

#[tokio::test]
async fn album_siblings_are_collected_and_the_copies_deleted() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    for (id, file) in [(42, "a"), (43, "b"), (44, "c")] {
        harness.mock.script_copy(
            "@night_art",
            id,
            CopiedMessage {
                chat_id: GUEST,
                message_id: 900 + id,
                media_group_id: Some("group-1".into()),
                caption: Some("album".into()),
                files: vec![photo(file)],
            },
        );
    }

    let outcome =
        nujijushi::jobs::import_target(&harness.state, "https://t.me/night_art/42", Some(GUEST))
            .await
            .unwrap();
    let post = harness
        .state
        .db
        .call({
            let id = outcome.post_ids[0];
            move |conn| nujijushi::db::posts::get(conn, id)
        })
        .await
        .unwrap();
    assert_eq!(post.media.len(), 3, "all three album items were imported");
    assert_eq!(harness.mock.deleted().len(), 3, "every copy is cleaned up");
}

#[tokio::test]
async fn oversized_files_are_skipped_with_a_note() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness.mock.script_copy(
        "@night_art",
        7,
        CopiedMessage {
            chat_id: GUEST,
            message_id: 70,
            media_group_id: None,
            caption: Some("big one".into()),
            files: vec![
                BotFile {
                    file_id: "huge".into(),
                    size: 25 * 1024 * 1024,
                    kind: FileKind::Video,
                    mime: Some("video/mp4".into()),
                },
                photo("small"),
            ],
        },
    );

    let outcome = nujijushi::jobs::import_target(&harness.state, "https://t.me/night_art/7", None)
        .await
        .unwrap();
    let post = harness
        .state
        .db
        .call({
            let id = outcome.post_ids[0];
            move |conn| nujijushi::db::posts::get(conn, id)
        })
        .await
        .unwrap();
    assert_eq!(post.media.len(), 1, "only the small file survives");
    assert!(
        harness
            .mock
            .downloads()
            .iter()
            .all(|(file, _)| file != "huge")
    );
}

#[tokio::test]
async fn unreadable_private_link_fails_with_guidance() {
    // no public fallback here: the failure must surface, not silently degrade
    let config = r#"
[telegram]
bot_token = "1:test"
[access]
admins = [1]
public = true
[import]
public_telegram_fallback = false
"#;
    let harness = harness(MockTransport::new(), config);
    harness
        .mock
        .fail_copy("@night_art", 5, TgError::Permanent("chat not found".into()));

    let error =
        nujijushi::jobs::import_target(&harness.state, "https://t.me/night_art/5", Some(GUEST))
            .await
            .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("cannot read that post"), "got: {text}");
    assert!(
        text.contains("public_telegram_fallback"),
        "points at the switch"
    );
}

#[tokio::test]
async fn guest_cannot_publish_but_admin_can() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    let channel = harness.add_channel("main", NsfwPolicy::Allow).await;
    let post = harness.add_post("1", 1, &["original"]).await;
    nujijushi::queue::enqueue_checked(&harness.state, &channel, post, 0)
        .await
        .unwrap();

    harness
        .send(private_message(GUEST, 1, "/publish main"))
        .await;
    let refusal = harness.last_text();
    assert!(refusal.contains("bot owner"), "guest is refused: {refusal}");
    assert!(
        refusal.contains("admins") && refusal.contains("allow_publish"),
        "the refusal names both switches: {refusal}"
    );
    assert_eq!(harness.mock.send_count(), 0);

    harness
        .send(private_message(ADMIN, 2, "/publish main"))
        .await;
    assert_eq!(harness.mock.send_count(), 1, "admin publishes");
    let states = harness
        .state
        .db
        .call({
            let channel_id = channel.id;
            move |conn| nujijushi::db::queue::list(conn, channel_id, &[QueueState::Sent], 10)
        })
        .await
        .unwrap();
    assert_eq!(states.len(), 1);
}

#[tokio::test]
async fn push_only_queues_never_publishes() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    let channel = harness.add_channel("main", NsfwPolicy::Allow).await;
    let post = harness.add_post("1", 2, &["original"]).await;

    harness
        .send(private_message(GUEST, 1, &format!("/push main {post}")))
        .await;
    assert!(
        harness.last_text().contains("queued"),
        "got: {}",
        harness.last_text()
    );
    assert_eq!(harness.mock.send_count(), 0, "/push must never publish");

    let queued = harness
        .state
        .db
        .call({
            let channel_id = channel.id;
            move |conn| nujijushi::db::queue::count_state(conn, channel_id, QueueState::Queued)
        })
        .await
        .unwrap();
    assert_eq!(queued, 1);
}

#[tokio::test]
async fn import_quota_is_enforced_for_guests() {
    let config = r#"
[telegram]
bot_token = "1:test"
[access]
admins = [1]
public = true
allow_import = true
import_quota_per_day = 1
"#;
    let harness = harness(MockTransport::new(), config);
    harness
        .send(private_message(GUEST, 1, "https://example.com/one"))
        .await;
    harness
        .send(private_message(GUEST, 2, "https://example.com/two"))
        .await;
    assert!(
        harness.last_text().contains("limit reached"),
        "got: {}",
        harness.last_text()
    );
    let jobs = harness
        .state
        .db
        .call(|conn| nujijushi::db::jobs::list(conn, 10))
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "only the first import was accepted");
}

#[tokio::test]
async fn strangers_are_refused_when_the_bot_is_private() {
    let harness = harness(
        MockTransport::new(),
        "[telegram]\nbot_token = \"1:test\"\n[access]\nadmins = [1]\npublic = false\nallow_import = true\n",
    );
    harness
        .send(private_message(GUEST, 1, "https://example.com/one"))
        .await;
    let text = harness.last_text();
    assert!(text.contains("private"), "got: {text}");
    assert!(text.contains(&GUEST.to_string()), "tells them their id");
    assert!(harness.job().await.is_none());
}

#[tokio::test]
async fn banned_users_are_refused_even_when_public() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .state
        .db
        .call(move |conn| nujijushi::db::users::set_role(conn, GUEST, "banned"))
        .await
        .unwrap();
    harness
        .send(private_message(GUEST, 1, "https://example.com/one"))
        .await;
    assert!(harness.last_text().contains("private"));
    assert!(harness.job().await.is_none());
}

#[tokio::test]
async fn discovery_registers_a_disabled_channel_once() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    let change = MemberChange {
        chat_id: -1001234567890,
        title: "Night Art".into(),
        username: Some("night_art".into()),
        is_channel: true,
        became_admin: true,
        by_user: Some(user(ADMIN)),
    };
    harness
        .send_update(UpdateKind::MemberChange(change.clone()))
        .await;

    let channels = harness
        .state
        .db
        .call(|conn| nujijushi::db::channels::list(conn))
        .await
        .unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].alias, "night_art");
    assert!(channels[0].discovered);
    assert!(!channels[0].enabled, "must wait for an explicit enable");
    assert!(
        harness.last_text().contains("night_art"),
        "the admin is told about it"
    );

    // the same update twice must not duplicate the channel
    harness.send_update(UpdateKind::MemberChange(change)).await;
    let channels = harness
        .state
        .db
        .call(|conn| nujijushi::db::channels::list(conn))
        .await
        .unwrap();
    assert_eq!(channels.len(), 1);
}

#[tokio::test]
async fn enabling_a_discovered_channel_makes_it_schedulable() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .send_update(UpdateKind::MemberChange(MemberChange {
            chat_id: -100999,
            title: "Late Night".into(),
            username: None,
            is_channel: true,
            became_admin: true,
            by_user: None,
        }))
        .await;
    let alias = {
        let channels = harness
            .state
            .db
            .call(|conn| nujijushi::db::channels::list(conn))
            .await
            .unwrap();
        channels[0].alias.clone()
    };
    assert_eq!(alias, "late-night", "slugged from the title");

    harness
        .send(private_message(ADMIN, 1, &format!("/enable {alias}")))
        .await;
    let channel = harness
        .state
        .db
        .call({
            let alias = alias.clone();
            move |conn| nujijushi::db::channels::by_alias(conn, &alias)
        })
        .await
        .unwrap()
        .unwrap();
    assert!(channel.enabled);
    assert!(!channel.paused);
}

#[tokio::test]
async fn forwarded_posts_are_imported_inline() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    let mut message = private_message(GUEST, 1, "");
    message.text = None;
    message.caption = Some("from a channel #landscape".into());
    message.files = vec![photo("fw1"), photo("fw2")];
    message.forward_from_chat_id = Some(-100555);
    message.forward_from_chat_title = Some("Source Channel".into());

    harness.send(message).await;
    assert!(
        harness.last_text().contains("Archived"),
        "got: {}",
        harness.last_text()
    );

    let post = harness
        .state
        .db
        .call(|conn| nujijushi::db::posts::cards(conn, &Default::default()))
        .await
        .unwrap()
        .pop()
        .expect("a post exists");
    let full = harness
        .state
        .db
        .call({
            let id = post.id;
            move |conn| nujijushi::db::posts::get(conn, id)
        })
        .await
        .unwrap();
    assert_eq!(full.source, "telegram_forward");
    assert_eq!(full.media.len(), 2);
    assert_eq!(
        full.author.as_ref().and_then(|a| a.name.clone()).as_deref(),
        Some("Source Channel")
    );
    assert!(full.tags.contains(&"landscape".to_string()));
}

#[tokio::test]
async fn links_anywhere_in_a_message_are_imported_and_prose_is_not() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    // a link inside a sentence counts: people paste "look at this: <url>"
    harness
        .send(private_message(GUEST, 1, "look at this: https://t.me/x/1"))
        .await;
    assert_eq!(harness.job_count().await, 1, "a link in a sentence imports");

    harness
        .send(private_message(GUEST, 2, "just chatting, no links here"))
        .await;
    assert_eq!(harness.job_count().await, 1, "prose is not an import");
    assert!(harness.last_text().contains("/help"));

    harness
        .send(private_message(
            GUEST,
            3,
            "https://www.pixiv.net/artworks/1",
        ))
        .await;
    assert_eq!(harness.job_count().await, 2);
}

#[tokio::test]
async fn whoami_always_answers() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness.send(private_message(GUEST, 1, "/whoami")).await;
    let text = harness.last_text();
    assert!(text.contains(&GUEST.to_string()));
    assert!(text.contains("guest"));
}

#[tokio::test]
async fn channels_and_queue_are_readable_for_guests() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    let channel = harness.add_channel("main", NsfwPolicy::Allow).await;
    let post = harness.add_post("1", 1, &["original"]).await;
    nujijushi::queue::enqueue_checked(&harness.state, &channel, post, 0)
        .await
        .unwrap();

    harness.send(private_message(GUEST, 1, "/channels")).await;
    assert!(
        harness.last_text().contains("main"),
        "got: {}",
        harness.last_text()
    );

    harness.send(private_message(GUEST, 2, "/queue main")).await;
    assert!(harness.last_text().contains(&format!("post #{post}")));
}

#[tokio::test]
async fn group_chats_and_bots_are_ignored() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    let mut group_message = private_message(GUEST, 1, "/status");
    group_message.chat_is_private = false;
    harness.send(group_message).await;
    assert!(harness.mock.texts().is_empty());

    let mut bot_message = private_message(GUEST, 2, "/status");
    if let Some(from) = bot_message.from.as_mut() {
        from.is_bot = true;
    }
    harness.send(bot_message).await;
    assert!(harness.mock.texts().is_empty());
    assert!(harness.mock.command_menu(Some(GUEST)).is_none());
}

#[tokio::test]
async fn chat_lookup_is_used_for_validation() {
    let mock = MockTransport::new().with_chat(ChatInfo {
        id: -100777,
        title: Some("Real Channel".into()),
        username: Some("real_channel".into()),
        is_channel: true,
        is_private: false,
    });
    let harness = harness(mock, OPEN_CONFIG);
    let found = harness.mock.get_chat("@real_channel").await.unwrap();
    assert_eq!(found.title.as_deref(), Some("Real Channel"));
    assert!(harness.mock.get_chat("@nope").await.is_err());
}

#[tokio::test]
async fn admins_manage_roles_from_the_chat() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);

    harness
        .send(private_message(ADMIN, 1, "/grant 555 guest"))
        .await;
    let role = harness
        .state
        .db
        .call(|conn| nujijushi::db::users::get(conn, 555))
        .await
        .unwrap()
        .map(|user| user.role);
    assert_eq!(role.as_deref(), Some("guest"));

    harness.send(private_message(ADMIN, 2, "/ban 555")).await;
    let role = harness
        .state
        .db
        .call(|conn| nujijushi::db::users::get(conn, 555))
        .await
        .unwrap()
        .map(|user| user.role);
    assert_eq!(role.as_deref(), Some("banned"));

    // a guest cannot promote anyone, and cannot touch a config admin
    harness
        .send(private_message(GUEST, 3, "/grant 777 admin"))
        .await;
    assert!(harness.last_text().contains("for the bot admin"));
    assert!(
        harness
            .state
            .db
            .call(|conn| nujijushi::db::users::get(conn, 777))
            .await
            .unwrap()
            .is_none(),
        "the refused grant changed nothing"
    );

    harness
        .send(private_message(ADMIN, 4, "/grant 1 banned"))
        .await;
    assert!(harness.last_text().contains("config.toml"));
}

const LOCKED_CONFIG: &str = r#"
[telegram]
bot_token = "1:test"
[access]
admins = [1]
public = false
"#;

#[tokio::test]
async fn guest_help_and_picker_list_only_enabled_commands() {
    let locked = format!("{LOCKED_CONFIG}\nguests = [{GUEST}]");
    let curation_only = format!("{locked}\nallow_import = false\nallow_curate = true");
    let cases: [(&str, &[&str]); 3] = [
        (&locked, &["start", "help", "import", "whoami", "status"]),
        (
            &curation_only,
            &[
                "start", "help", "approve", "reject", "nsfw", "rate", "tag", "whoami", "status",
            ],
        ),
        (
            OPEN_CONFIG,
            &[
                "start", "help", "import", "ls", "show", "push", "queue", "channels", "approve",
                "reject", "nsfw", "rate", "tag", "whoami", "status",
            ],
        ),
    ];
    for (config, expected) in cases {
        let harness = harness(MockTransport::new(), config);
        harness.send(private_message(GUEST, 1, "/start")).await;
        assert_eq!(harness.command_names(GUEST), expected);
        let welcome = harness.last_text();
        assert_eq!(
            welcome.contains("Send me a link"),
            expected.contains(&"import")
        );
        assert_eq!(listed_commands(&welcome), expected);
        harness.send(private_message(GUEST, 2, "/help")).await;
        assert_eq!(listed_commands(&harness.last_text()), expected);
    }
}

fn listed_commands(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("<code>/"))
        .filter_map(|line| line.split([' ', '<']).next())
        .collect()
}

#[tokio::test]
async fn role_changes_refresh_the_picker_before_the_target_returns() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness.send(private_message(GUEST, 1, "/start")).await;
    let guest_commands = harness.command_names(GUEST);
    assert!(!guest_commands.contains(&"publish"));
    assert!(!guest_commands.contains(&"grant"));

    harness
        .send(private_message(ADMIN, 1, &format!("/grant {GUEST} admin")))
        .await;
    assert_eq!(harness.command_names(GUEST), harness.command_names(ADMIN));
    assert!(harness.command_names(GUEST).contains(&"publish"));
    assert!(harness.command_names(GUEST).contains(&"grant"));

    harness
        .send(private_message(ADMIN, 2, &format!("/grant {GUEST} guest")))
        .await;
    assert_eq!(harness.command_names(GUEST), guest_commands);

    harness
        .send(private_message(ADMIN, 3, &format!("/ban {GUEST}")))
        .await;
    assert!(harness.command_names(GUEST).is_empty());
}

#[tokio::test]
async fn publishing_needs_allow_publish_even_for_the_admin() {
    let harness = harness(MockTransport::new(), LOCKED_CONFIG);
    harness
        .send(private_message(ADMIN, 1, "/publish main"))
        .await;
    let text = harness.last_text();
    assert!(text.contains("switched off"), "got: {text}");
    assert!(text.contains("allow_publish"), "names the switch: {text}");
    assert_eq!(harness.mock.send_count(), 0);

    let old_menu = vec![("publish", "publish one post now")];
    harness
        .mock
        .set_commands(None, old_menu.clone())
        .await
        .unwrap();
    harness
        .mock
        .set_commands(Some(ADMIN), old_menu)
        .await
        .unwrap();
    let bot = Bot::new(&harness.state).await.unwrap();
    bot.run(true).await.unwrap();
    assert_eq!(harness.mock.command_menu(None), Some(Vec::new()));
    let menu = harness.command_names(ADMIN);
    assert!(menu.contains(&"grant"));
    assert!(!menu.contains(&"publish"));

    harness.send(private_message(ADMIN, 2, "/help")).await;
    assert_eq!(listed_commands(&harness.last_text()), menu);
}

#[tokio::test]
async fn a_config_admin_is_told_that_config_owns_their_role() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    // config admins win at runtime even if the users row says guest
    harness
        .state
        .db
        .call(|conn| nujijushi::db::users::set_role(conn, ADMIN, "guest"))
        .await
        .unwrap();
    harness.send(private_message(ADMIN, 1, "/whoami")).await;
    assert!(
        harness.last_text().contains("role admin"),
        "got: {}",
        harness.last_text()
    );
}

#[tokio::test]
async fn a_message_with_several_links_queues_every_one() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .send(private_message(
            ADMIN,
            1,
            "https://x.com/a/status/1%0Ahttps://x.com/a/status/2\nt.me/night_art/9\ntelegram.me/night_art/7",
        ))
        .await;

    let jobs = harness
        .state
        .db
        .call(|conn| nujijushi::db::jobs::list(conn, 20))
        .await
        .unwrap();
    assert_eq!(jobs.len(), 4, "every link in the batch becomes a job");
    let payloads: Vec<&str> = jobs
        .iter()
        .filter_map(|job| job.payload.as_deref())
        .collect();
    assert!(
        payloads.contains(&"https://x.com/a/status/1"),
        "{payloads:?}"
    );
    assert!(
        payloads.contains(&"https://x.com/a/status/2"),
        "{payloads:?}"
    );
    assert!(
        payloads.contains(&"https://t.me/night_art/9"),
        "t.me links are normalised: {payloads:?}"
    );
    assert!(
        payloads.contains(&"https://telegram.me/night_art/7"),
        "telegram.me links are normalised too: {payloads:?}"
    );

    let text = harness.last_text();
    assert!(text.contains("4 link(s)"), "got: {text}");
    assert!(text.contains("https://x.com/a/status/2"), "got: {text}");
}

#[tokio::test]
async fn a_batch_cannot_use_more_quota_than_it_has() {
    // guests get 1 import/day here, so the second link must be refused
    let config = r#"
[telegram]
bot_token = "1:test"
[access]
admins = [1]
public = true
allow_import = true
import_quota_per_day = 1
"#;
    let harness = harness(MockTransport::new(), config);
    harness
        .send(private_message(
            GUEST,
            1,
            "/import https://x.com/a/status/1%0ahttps://x.com/a/status/2",
        ))
        .await;

    let jobs = harness
        .state
        .db
        .call(|conn| nujijushi::db::jobs::list(conn, 20))
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "one link fits in the quota, not two");
    let text = harness.last_text();
    assert!(text.contains("Daily import limit reached"), "got: {text}");
}

/// A guest's links land in the archive unseen, so the admins get one DM per
/// batch — with the handle, the count and the links.
#[tokio::test]
async fn a_guest_submission_is_reported_to_the_admins() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .send(private_message(
            GUEST,
            40,
            "https://www.pixiv.net/artworks/123",
        ))
        .await;

    let texts = harness.mock.texts();
    let admin_dms: Vec<&(String, String)> = texts
        .iter()
        .filter(|(chat, _)| chat == &ADMIN.to_string())
        .collect();
    assert_eq!(admin_dms.len(), 1, "one DM per batch: {texts:?}");
    let body = &admin_dms[0].1;
    assert!(body.contains("@user99"), "who submitted: {body}");
    assert!(body.contains("pixiv.net/artworks/123"), "what: {body}");
    assert!(body.contains("not"), "it must ask for a look: {body}");
}

/// Admins import too; they do not need to be told about themselves.
#[tokio::test]
async fn an_admin_submission_does_not_notify_anyone() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .send(private_message(
            ADMIN,
            41,
            "https://www.pixiv.net/artworks/456",
        ))
        .await;

    let texts = harness.mock.texts();
    assert!(
        texts.iter().all(|(_, body)| !body.contains("submitted")),
        "no admin DM for an admin's own import: {texts:?}"
    );
}

/// Once the user is promoted, an import through the same chat stops pinging the
/// admins (the flag follows the *current* role, not the history).
#[tokio::test]
async fn a_promoted_user_no_longer_triggers_the_admin_dm() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .state
        .db
        .call(|conn| nujijushi::db::users::set_role(conn, GUEST, "admin"))
        .await
        .unwrap();
    harness
        .send(private_message(
            GUEST,
            42,
            "https://www.pixiv.net/artworks/789",
        ))
        .await;

    let texts = harness.mock.texts();
    assert!(
        texts.iter().all(|(_, body)| !body.contains("submitted")),
        "a promoted user is an admin now: {texts:?}"
    );
}

/// `/users` shows the effective role. A config admin keeps a `guest` row from
/// their first message, and listing that row read like a broken config.
#[tokio::test]
async fn the_user_listing_shows_a_config_admin_as_admin() {
    let harness = harness(MockTransport::new(), OPEN_CONFIG);
    harness
        .state
        .db
        .call(|conn| nujijushi::db::users::set_role(conn, ADMIN, "guest"))
        .await
        .unwrap();

    harness.send(private_message(ADMIN, 50, "/users")).await;
    let text = harness.last_text();
    assert!(text.contains("admin · @user1"), "got: {text}");
    assert!(text.contains("(config admin)"), "got: {text}");
    assert!(
        !text.contains("guest · @user1"),
        "the stored row must not contradict the config: {text}"
    );
}
