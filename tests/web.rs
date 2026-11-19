//! Web actions: publish and storage upload become jobs, `/api/status`, `/jobs`.
//!
//! The point of these tests is that a POST returns *before* Telegram is touched
//! and that the worker then does the send exactly once.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use nujijushi::config::Config;
use nujijushi::db::jobs as job_db;
use nujijushi::db::posts::NewMedia;
use nujijushi::jobs::JobReport;
use nujijushi::model::{Channel, NsfwPolicy, QueueState, Rating};
use nujijushi::source::{NormalizedPost, PostAuthor};
use nujijushi::state::AppState;
use nujijushi::tg::Transport;
use nujijushi::tg::mock::MockTransport;
use nujijushi::web::{WebState, auth, routes};

const UI_TOKEN: &str = "test-ui-token";

fn config() -> String {
    format!(
        r#"
[telegram]
bot_token = "1:test"
[web]
token = "{UI_TOKEN}"
[access]
admins = [1]
"#
    )
}

struct Harness {
    _dir: tempfile::TempDir,
    state: AppState,
    mock: Arc<MockTransport>,
    ws: Arc<WebState>,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config()).unwrap();
    let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    let mock = Arc::new(MockTransport::new());
    let transport: Arc<dyn Transport> = mock.clone();
    let state = AppState::open(cfg, Some(transport)).unwrap();
    let ws = WebState::new(state.clone()).unwrap();
    Harness {
        _dir: dir,
        state,
        mock,
        ws,
    }
}

impl Harness {
    async fn add_channel(&self) -> Channel {
        let mut channel = Channel::new("Test", "@test_channel");
        channel.nsfw_policy = NsfwPolicy::Allow;
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

    /// One post with one photo, already sitting in the channel's queue.
    /// One post with one photo, not queued anywhere.
    async fn post_only(&self, label: &str) -> i64 {
        let mut post = NormalizedPost::new("pixiv", label);
        post.url = Some(format!("https://www.pixiv.net/artworks/{label}"));
        post.title = Some(format!("Artwork {label}"));
        post.rating = Rating::Sfw;
        post.tags = vec!["original".into()];
        post.author = Some(PostAuthor {
            source_id: "artist".into(),
            name: Some("Test Artist".into()),
            handle: Some("tester".into()),
            url: None,
        });
        let media = vec![NewMedia {
            blob_hash: format!("{label}-0").repeat(4),
            ext: "png".into(),
            mime: None,
            width: Some(800),
            height: Some(600),
            bytes: 1024,
            is_video: false,
            poster_hash: None,
            sensitive: false,
            meta_json: None,
        }];
        let (post_id, _) = self
            .state
            .db
            .call(move |conn| nujijushi::db::posts::upsert(conn, &post, &media))
            .await
            .unwrap();
        post_id
    }

    /// The same post, already sitting in the channel's queue.
    async fn queue_a_post(&self, label: &str, channel: &Channel) -> (i64, i64) {
        let post_id = self.post_only(label).await;
        let channel = channel.clone();
        let outcome = nujijushi::queue::enqueue_checked(&self.state, &channel, post_id, 0)
            .await
            .unwrap();
        let queue_id = match outcome {
            nujijushi::queue::EnqueueOutcome::Queued(id) => id,
            other => panic!("unexpected enqueue outcome: {other:?}"),
        };
        (post_id, queue_id)
    }

    fn get(&self, path: &str) -> Request<Body> {
        Request::get(path)
            .header(
                header::COOKIE,
                format!("{}={}", auth::COOKIE, self.ws.session),
            )
            .body(Body::empty())
            .unwrap()
    }

    fn post(&self, path: &str, body: &str) -> Request<Body> {
        Request::post(path)
            .header(
                header::COOKIE,
                format!("{}={}", auth::COOKIE, self.ws.session),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn send(&self, request: Request<Body>) -> axum::response::Response {
        routes::router(self.ws.clone())
            .oneshot(request)
            .await
            .unwrap()
    }

    /// Follow a POST through to its redirect target's flash message.
    fn flash(response: &axum::response::Response) -> String {
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        location
            .split_once("flash=")
            .map(|(_, raw)| {
                raw.replace('+', " ")
                    .replace("%23", "#")
                    .replace("%20", " ")
            })
            .unwrap_or_else(|| location.to_string())
    }

    async fn jobs(&self) -> Vec<job_db::Job> {
        self.state
            .db
            .call(|conn| job_db::list(conn, 50))
            .await
            .unwrap()
    }

    async fn job_state(&self, id: i64) -> String {
        self.state
            .db
            .call(move |conn| job_db::get(conn, id))
            .await
            .unwrap()
            .expect("job exists")
            .state
    }

    /// Run the worker's path for one job (claim + run), like the daemon does.
    async fn run_worker_once(&self) -> Vec<JobReport> {
        let jobs = self
            .state
            .db
            .call(|conn| job_db::claim_pending(conn, 4))
            .await
            .unwrap();
        let mut reports = Vec::new();
        for job in jobs {
            reports.push(nujijushi::jobs::run_job(&self.state, &job).await.unwrap());
        }
        reports
    }
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn publish_returns_at_once_and_the_worker_sends_it() {
    let h = harness();
    let channel = h.add_channel().await;
    h.queue_a_post("1", &channel).await;

    let response = h
        .send(h.post(
            "/publish",
            &format!("csrf={}&channel_id={}&back=/", h.ws.csrf, channel.id),
        ))
        .await;

    // the request is over before Telegram is touched
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(h.mock.send_count(), 0, "no upload inside the request");
    assert!(
        Harness::flash(&response).contains("queued as job #"),
        "flash: {}",
        Harness::flash(&response)
    );

    let jobs = h.jobs().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, nujijushi::jobs::KIND_PUBLISH_CHANNEL);
    assert_eq!(
        jobs[0].payload.as_deref(),
        Some(channel.id.to_string().as_str())
    );
    assert_eq!(jobs[0].state, "pending");

    // the worker sends exactly once and closes the job
    let reports = h.run_worker_once().await;
    assert_eq!(reports.len(), 1);
    assert_eq!(h.mock.send_count(), 1, "one album, one send");
    assert_eq!(h.job_state(jobs[0].id).await, "done");
    let stored = h
        .state
        .db
        .call({
            let id = channel.id;
            move |conn| nujijushi::db::channels::get(conn, id)
        })
        .await
        .unwrap();
    assert!(stored.last_sent_at.is_some(), "the slot is consumed");
}

#[tokio::test]
async fn a_second_click_does_not_queue_a_second_publish() {
    let h = harness();
    let channel = h.add_channel().await;
    h.queue_a_post("1", &channel).await;
    let body = format!("csrf={}&channel_id={}&back=/", h.ws.csrf, channel.id);

    let first = h.send(h.post("/publish", &body)).await;
    assert_eq!(first.status(), StatusCode::SEE_OTHER);
    let second = h.send(h.post("/publish", &body)).await;
    assert_eq!(second.status(), StatusCode::SEE_OTHER);
    assert!(
        Harness::flash(&second).contains("already queued or running"),
        "flash: {}",
        Harness::flash(&second)
    );

    let jobs = h.jobs().await;
    assert_eq!(jobs.len(), 1, "one job per channel while it is pending");

    // and the send still happens only once
    h.run_worker_once().await;
    assert_eq!(h.mock.send_count(), 1);
}

#[tokio::test]
async fn a_single_queue_item_can_be_published_as_a_job() {
    let h = harness();
    let channel = h.add_channel().await;
    let (post_id, queue_id) = h.queue_a_post("1", &channel).await;
    h.queue_a_post("2", &channel).await;

    let response = h
        .send(h.post(
            "/publish",
            &format!("csrf={}&queue_id={}&back=/", h.ws.csrf, queue_id),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let jobs = h.jobs().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, nujijushi::jobs::KIND_PUBLISH_ITEM);
    assert_eq!(
        jobs[0].payload.as_deref(),
        Some(queue_id.to_string().as_str())
    );

    h.run_worker_once().await;
    assert_eq!(h.mock.send_count(), 1);
    assert_eq!(h.job_state(jobs[0].id).await, "done");

    // the item that was asked for went out, the other one is still queued
    let states = h
        .state
        .db
        .call({
            let channel_id = channel.id;
            move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT post_id, state FROM queue WHERE channel_id = ?1 ORDER BY id",
                )?;
                let rows = stmt.query_map([channel_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row?);
                }
                Ok(out)
            }
        })
        .await
        .unwrap();
    let sent: Vec<i64> = states
        .iter()
        .filter(|(_, state)| QueueState::parse(state) == QueueState::Sent)
        .map(|(post, _)| *post)
        .collect();
    assert_eq!(sent, vec![post_id]);
}

#[tokio::test]
async fn a_csrf_failure_queues_nothing() {
    let h = harness();
    let channel = h.add_channel().await;
    h.queue_a_post("1", &channel).await;

    let response = h
        .send(h.post("/publish", &format!("csrf=wrong&channel_id={}", channel.id)))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(h.jobs().await.is_empty(), "no job for a forged request");
    assert_eq!(h.mock.send_count(), 0);
}

/// The drain button queues an upload sweep, so it must check the token too: a
/// request that only carries the session cookie must not start it.
#[tokio::test]
async fn a_forged_storage_upload_is_refused() {
    let h = harness();
    for body in ["csrf=wrong", ""] {
        let response = h.send(h.post("/storage/drain", body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body: {body}");
    }
    assert!(h.jobs().await.is_empty(), "no job for a forged request");
}

#[tokio::test]
async fn storage_upload_is_queued_instead_of_running_in_the_request() {
    let h = harness();
    let response = h
        .send(h.post("/storage/drain", &format!("csrf={}", h.ws.csrf)))
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    let jobs = h.jobs().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, nujijushi::jobs::KIND_STORAGE_DRAIN);
    assert_eq!(jobs[0].state, "pending");

    // running it with no sink configured is a no-op that still closes the job
    h.run_worker_once().await;
    let job = h
        .state
        .db
        .call({
            let id = jobs[0].id;
            move |conn| job_db::get(conn, id)
        })
        .await
        .unwrap()
        .expect("job exists");
    assert_eq!(job.state, "done");
    assert!(
        job.message.unwrap_or_default().contains("mirror"),
        "the job says what it did"
    );
}

#[tokio::test]
async fn status_reports_jobs_and_queue_depth() {
    let h = harness();
    let channel = h.add_channel().await;
    h.queue_a_post("1", &channel).await;
    h.queue_a_post("2", &channel).await;
    let job_id = nujijushi::jobs::create_publish_channel_job(&h.state, channel.id)
        .await
        .unwrap()
        .expect("queued");

    let response = h.send(h.get("/api/status")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();

    assert!(body["now"].is_string(), "got {body}");
    let jobs = body["jobs"].as_array().expect("jobs array");
    let job = jobs
        .iter()
        .find(|job| job["id"] == job_id)
        .unwrap_or_else(|| panic!("job {job_id} missing: {body}"));
    assert_eq!(job["kind"], "publish_channel");
    assert_eq!(job["kind_label"], "publish next");
    assert_eq!(job["state"], "pending");
    assert_eq!(job["active"], true);
    assert_eq!(job["payload"], channel.id.to_string());

    let channels = body["channels"].as_array().expect("channels array");
    assert_eq!(channels.len(), 1);
    let view = &channels[0];
    assert_eq!(view["id"], channel.id);
    assert_eq!(
        view["alias"], "test_channel",
        "the stored alias, without the @"
    );
    assert_eq!(view["queued"], 2, "two posts waiting");
    assert_eq!(view["publishing"], true, "a publish job is pending");
    assert_eq!(view["paused"], false);
}

#[tokio::test]
async fn status_is_not_readable_without_the_session_cookie() {
    let h = harness();
    let response = routes::router(h.ws.clone())
        .oneshot(Request::get("/api/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(location.starts_with("/login"), "got {location}");
}

#[tokio::test]
async fn the_jobs_page_lists_progress_and_a_retry_button() {
    let h = harness();
    // one job as the worker left it (a failed import) and one publish in flight
    let failed = h
        .state
        .db
        .call(|conn| {
            let id = job_db::create(conn, nujijushi::jobs::KIND_IMPORT, "https://x.test/a", 0)?;
            job_db::set_state(conn, id, "failed", Some("boom"))?;
            Ok(id)
        })
        .await
        .unwrap();
    let channel = h.add_channel().await;
    let running = nujijushi::jobs::create_publish_channel_job(&h.state, channel.id)
        .await
        .unwrap()
        .expect("queued");

    let response = h.send(h.get("/jobs")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;

    assert!(html.contains(&format!("data-job=\"{failed}\"")), "{html}");
    assert!(html.contains(&format!("data-job=\"{running}\"")), "{html}");
    assert!(html.contains("publish next"), "kind label: {html}");
    assert!(html.contains("class=\"progress\""), "progress bar: {html}");
    assert!(
        html.contains("action=\"/jobs/retry\""),
        "retry form: {html}"
    );
    assert!(html.contains(&format!("name=\"csrf\" value=\"{}\"", h.ws.csrf)));
    assert!(
        html.contains("imports run in parallel")
            || html.contains("import run in parallel")
            || html.contains("in\n    parallel")
            || html.contains("in parallel"),
        "concurrency hint: {html}"
    );
}

#[tokio::test]
async fn retrying_a_failed_import_queues_a_new_job() {
    let h = harness();
    let failed = h
        .state
        .db
        .call(|conn| {
            let id = job_db::create(conn, nujijushi::jobs::KIND_IMPORT, "https://x.test/a", 0)?;
            job_db::set_state(conn, id, "failed", Some("boom"))?;
            Ok(id)
        })
        .await
        .unwrap();

    let response = h
        .send(h.post("/jobs/retry", &format!("csrf={}&id={failed}", h.ws.csrf)))
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        Harness::flash(&response).contains("re-queued"),
        "flash: {}",
        Harness::flash(&response)
    );

    let jobs = h.jobs().await;
    assert_eq!(jobs.len(), 2, "the history keeps both attempts");
    let retried = jobs.iter().find(|job| job.id != failed).unwrap();
    assert_eq!(retried.kind, nujijushi::jobs::KIND_IMPORT);
    assert_eq!(retried.payload.as_deref(), Some("https://x.test/a"));
    assert_eq!(retried.state, "pending");
}

#[tokio::test]
async fn a_publish_job_is_not_retryable() {
    let h = harness();
    let channel = h.add_channel().await;
    let id = h
        .state
        .db
        .call({
            let channel_id = channel.id;
            move |conn| {
                let id = job_db::create(
                    conn,
                    nujijushi::jobs::KIND_PUBLISH_CHANNEL,
                    &channel_id.to_string(),
                    0,
                )?;
                job_db::set_state(conn, id, "failed", Some("chat not found"))?;
                Ok(id)
            }
        })
        .await
        .unwrap();

    let response = h
        .send(h.post("/jobs/retry", &format!("csrf={}&id={id}", h.ws.csrf)))
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        Harness::flash(&response).contains("only imports can be retried"),
        "flash: {}",
        Harness::flash(&response)
    );
    assert_eq!(h.jobs().await.len(), 1, "nothing new was queued");
}

#[tokio::test]
async fn the_dashboard_and_import_pages_expose_the_live_hooks() {
    let h = harness();
    let channel = h.add_channel().await;
    nujijushi::jobs::create_publish_channel_job(&h.state, channel.id)
        .await
        .unwrap()
        .expect("queued");

    let dashboard = body_text(h.send(h.get("/")).await).await;
    assert!(
        dashboard.contains(&format!("data-channel=\"{}\"", channel.id)),
        "channel hook: {dashboard}"
    );
    assert!(
        dashboard.contains("data-role=\"publishing\""),
        "{dashboard}"
    );
    assert!(dashboard.contains("data-role=\"queued\""), "{dashboard}");
    assert!(
        dashboard.contains("href=\"/jobs\""),
        "link to /jobs: {dashboard}"
    );

    let import = body_text(h.send(h.get("/import")).await).await;
    assert!(import.contains("data-job="), "job hook: {import}");
    assert!(import.contains("class=\"progress\""), "progress: {import}");
    assert!(import.contains("data-live-hint"), "{import}");
}

/// Retrying a job that is still pending or running would fetch and store the
/// same target twice, so only a settled failure may be retried.
#[tokio::test]
async fn retrying_an_unfinished_import_is_refused() {
    let h = harness();
    let running = h
        .state
        .db
        .call(|conn| {
            let id = job_db::create(conn, nujijushi::jobs::KIND_IMPORT, "https://x.test/a", 0)?;
            job_db::set_state(conn, id, "running", None)?;
            Ok(id)
        })
        .await
        .unwrap();

    let response = h
        .send(h.post("/jobs/retry", &format!("csrf={}&id={running}", h.ws.csrf)))
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        Harness::flash(&response).contains("only failed jobs can be retried"),
        "flash: {}",
        Harness::flash(&response)
    );
    assert_eq!(
        h.jobs().await.len(),
        1,
        "no second import of the same target"
    );
}

/// The grid is about what is still to publish: delivered posts are hidden unless
/// asked for, and the filter says so.
#[tokio::test]
async fn the_grid_hides_sent_posts_until_asked() {
    let h = harness();
    let channel = h.add_channel().await;
    let (sent_post, queue_id) = h.queue_a_post("1", &channel).await;
    let waiting = h.post_only("2").await;
    h.state
        .db
        .call(move |conn| nujijushi::db::queue::mark_sent(conn, queue_id, "5", chrono::Utc::now()))
        .await
        .unwrap();

    let card = |id: i64| format!("href=\"/posts/{id}\"");

    let html = body_text(h.send(h.get("/posts")).await).await;
    assert!(html.contains(&card(waiting)), "unsent post is listed");
    assert!(
        !html.contains(&card(sent_post)),
        "sent post is hidden by default"
    );
    assert!(html.contains("hide sent"), "the filter says what it does");

    let all = body_text(h.send(h.get("/posts?sent=all")).await).await;
    assert!(all.contains(&card(sent_post)) && all.contains(&card(waiting)));

    let only = body_text(h.send(h.get("/posts?sent=yes")).await).await;
    assert!(only.contains(&card(sent_post)));
    assert!(!only.contains(&card(waiting)), "sent-only means sent only");
}

/// A post that a non-admin submitted says so, with their handle; an admin's own
/// import stays quiet.
#[tokio::test]
async fn a_guest_submission_is_badged_in_the_grid() {
    let h = harness();
    let guest_post = h.post_only("1").await;
    let admin_post = h.post_only("2").await;
    h.state
        .db
        .call(move |conn| {
            conn.execute(
                "INSERT INTO users (tg_id, username, first_name, role, created_at)
                 VALUES (7, 'fan', NULL, 'guest', '2026-01-01T00:00:00Z'),
                        (8, 'boss', NULL, 'admin', '2026-01-01T00:00:00Z')",
                [],
            )?;
            nujijushi::db::posts::set_submitted_by(conn, guest_post, 7)?;
            nujijushi::db::posts::set_submitted_by(conn, admin_post, 8)
        })
        .await
        .unwrap();

    let html = body_text(h.send(h.get("/posts")).await).await;
    assert!(html.contains("via @fan"), "the guest is named: {html}");
    assert_eq!(
        html.matches("badge amber").count(),
        1,
        "exactly the guest's post is badged: {html}"
    );
    assert!(
        !html.contains("via @boss") && !html.contains("via <"),
        "an admin (or an absent submitter) needs no badge: {html}"
    );

    let detail = body_text(h.send(h.get(&format!("/posts/{guest_post}"))).await).await;
    assert!(detail.contains("@fan"));
    assert!(detail.contains("not an admin"));
}

/// A queue that is not moving is explained: no heartbeat means the scheduler is
/// not running (`serve` never schedules).
#[tokio::test]
async fn the_dashboard_says_when_the_scheduler_is_not_running() {
    let h = harness();
    let html = body_text(h.send(h.get("/")).await).await;
    assert!(
        html.contains("nothing is publishing on a schedule"),
        "banner missing: {html}"
    );
    assert!(html.contains("nujijushi run"), "the hint names the command");

    h.state
        .db
        .set_setting(
            nujijushi::queue::scheduler::TICK_KEY,
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
    let html = body_text(h.send(h.get("/")).await).await;
    assert!(
        !html.contains("nothing is publishing on a schedule"),
        "a fresh heartbeat settles the banner: {html}"
    );

    let status: serde_json::Value =
        serde_json::from_str(&body_text(h.send(h.get("/api/status")).await).await).unwrap();
    assert_eq!(status["scheduler"]["stale"], false);
    assert!(status["scheduler"]["age_secs"].is_number());
}

/// The queue and the post page answer "when does this actually go out?" with the
/// same projection: 30-minute channel, two queued posts → now and in 30 minutes.
#[tokio::test]
async fn the_queue_and_post_pages_show_when_a_post_goes_out() {
    let h = harness();
    let channel = h.add_channel().await;
    let (post_id, _) = h.queue_a_post("1", &channel).await;
    h.queue_a_post("2", &channel).await;

    let html = body_text(
        h.send(h.get(&format!("/channels/{}/queue", channel.id)))
            .await,
    )
    .await;
    assert!(
        html.contains(">est.<") || html.contains("est."),
        "the column is labelled: {html}"
    );
    assert!(
        html.contains("in 30m"),
        "the second queued post is 30 minutes out: {html}"
    );

    let detail = body_text(h.send(h.get(&format!("/posts/{post_id}"))).await).await;
    assert!(
        detail.contains("est. "),
        "the post page says when it would go out: {detail}"
    );
}
