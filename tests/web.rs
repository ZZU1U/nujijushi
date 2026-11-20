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
use nujijushi::model::{Channel, NsfwPolicy, PostState, QueueState, Rating};
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
async fn importing_pasted_encoded_newlines_queues_each_link() {
    let h = harness();
    let first = "https://www.pixiv.net/artworks/123";
    let second = "https://example.test/search?q=a%0Ab";
    let pasted = format!("{first}%0A{second}");
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", &h.ws.csrf)
        .append_pair("targets", &pasted)
        .finish();
    let response = h.send(h.post("/import", &form)).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let jobs = h.jobs().await;
    let targets: Vec<_> = jobs
        .iter()
        .rev()
        .map(|job| job.payload.as_deref().unwrap())
        .collect();
    assert_eq!(targets, [first, second]);
}

#[tokio::test]
async fn direct_web_import_is_approved_when_the_job_creates_it() {
    let h = harness();
    let artwork = tempfile::tempdir().unwrap();
    image::RgbImage::new(8, 8)
        .save(artwork.path().join("sample.png"))
        .unwrap();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", &h.ws.csrf)
        .append_pair("targets", artwork.path().to_str().unwrap())
        .finish();
    assert_eq!(
        h.send(h.post("/import", &form)).await.status(),
        StatusCode::SEE_OTHER
    );
    let reports = h.run_worker_once().await;
    let post_id = reports[0].post_ids()[0];
    let post = h
        .state
        .db
        .with(|conn| nujijushi::db::posts::get(conn, post_id))
        .unwrap();
    assert_eq!(post.state, PostState::Approved);
}

/// One job row of a rendered page, so a test can assert about *that* job.
fn job_row(html: &str, id: i64) -> &str {
    let start = html
        .find(&format!("data-job=\"{id}\""))
        .unwrap_or_else(|| panic!("no row for job {id}: {html}"));
    let end = start + html[start..].find("</tr>").expect("row is closed");
    &html[start..end]
}

#[tokio::test]
async fn post_detail_shows_tag_provenance_and_can_undo_a_source_removal() {
    use nujijushi::db::tags;

    let h = harness();
    let post_id = h.post_only("42").await;
    let tag_id = h
        .state
        .db
        .call(move |conn| {
            tags::add_manual(conn, post_id, &["original".into(), "favorite".into()])?;
            Ok(tags::provenance(conn, post_id)?[0].id)
        })
        .await
        .unwrap();
    let path = format!("/posts/{post_id}");
    let action = format!("{path}/tags");
    let html = body_text(h.send(h.get(&path)).await).await;
    assert!(
        html.contains("aria-label=\"Remove source tag original\""),
        "{html}"
    );
    assert!(html.contains("<span class=\"badge\">source</span>"));
    assert!(html.contains("<span class=\"badge\">manual</span>"));
    assert!(html.contains("name=\"tags\" value=\"original, favorite\""));

    let edit = format!("csrf={}&notes=reviewed", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&path, &edit)).await.status(),
        StatusCode::SEE_OTHER
    );
    let invalid = h
        .send(h.post(&action, &format!("csrf=bad&tag_id={tag_id}&action=remove")))
        .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let html = body_text(h.send(h.get(&path)).await).await;
    assert!(html.contains("Remove source tag original"));

    let remove = format!("csrf={}&tag_id={tag_id}&action=remove", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&action, &remove)).await.status(),
        StatusCode::SEE_OTHER
    );
    let html = body_text(h.send(h.get(&path)).await).await;
    assert!(html.contains("removed source tags"));
    assert!(html.contains("Undo removal of source tag original"));
    assert!(!html.contains("Remove source tag original"));
    assert!(html.contains("name=\"tags\" value=\"original, favorite\""));

    let undo = format!("csrf={}&tag_id={tag_id}&action=undo", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&action, &undo)).await.status(),
        StatusCode::SEE_OTHER
    );
    let html = body_text(h.send(h.get(&path)).await).await;
    assert!(html.contains("Remove source tag original"));
    assert!(!html.contains("removed source tags"));
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
async fn web_review_validates_rewards_and_queues_new_suggestions_atomically() {
    use nujijushi::db::{posts, reputation};

    let mut h = harness();
    Arc::make_mut(&mut h.state.cfg).guest_reputation = true;
    h.ws = WebState::new(h.state.clone()).unwrap();
    let review_post = h.post_only("900").await;
    let queue_post = h.post_only("901").await;
    h.state
        .db
        .call(move |conn| {
            reputation::register_new(conn, review_post, 90)?;
            reputation::register_new(conn, queue_post, 90)?;
            Ok(())
        })
        .await
        .unwrap();

    let path = format!("/posts/{review_post}");
    let html = body_text(h.send(h.get(&path)).await).await;
    assert!(html.contains("Guest approval reward"));
    for form in [
        "csrf=wrong&state=approved&approval_points=17".to_owned(),
        format!("csrf={}&state=approved&approval_points=21", h.ws.csrf),
    ] {
        assert_eq!(
            h.send(h.post(&path, &form)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        h.state
            .db
            .with(|conn| Ok(posts::get(conn, review_post)?.state))
            .unwrap(),
        PostState::New
    );
    let form = format!("csrf={}&state=approved&approval_points=17", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&path, &form)).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, 90))
            .unwrap(),
        1700
    );

    let channel = h.add_channel().await;
    let form = format!(
        "csrf={}&action=queue&sel_{queue_post}=on&channel={}",
        h.ws.csrf, channel.id
    );
    for _ in 0..2 {
        assert_eq!(
            h.send(h.post("/posts", &form)).await.status(),
            StatusCode::SEE_OTHER
        );
    }
    assert_eq!(
        h.state
            .db
            .with(|conn| Ok(posts::get(conn, queue_post)?.state))
            .unwrap(),
        PostState::Approved
    );
    assert_eq!(
        h.state
            .db
            .with(|conn| reputation::balance(conn, 90))
            .unwrap(),
        2700
    );
    assert!(
        h.mock.texts().is_empty(),
        "the web request must not send Telegram DMs"
    );
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
    // one job as the worker left it (a failed single-target import) and one
    // publish in flight — neither has anything to track
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
    // a folder import really does walk many posts, so it *does* get a bar
    let folder = h
        .state
        .db
        .call(|conn| {
            let id = job_db::create(
                conn,
                nujijushi::jobs::KIND_IMPORT,
                &format!("https://x.test/{}", "folder".repeat(300)),
                0,
            )?;
            job_db::progress(conn, id, 3, 8)?;
            job_db::set_state(conn, id, "running", None)?;
            Ok(id)
        })
        .await
        .unwrap();
    let watch = h
        .state
        .db
        .call(|conn| job_db::create(conn, nujijushi::jobs::KIND_WATCH_CHECK, "1", 0))
        .await
        .unwrap();

    let response = h.send(h.get("/jobs")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;

    assert!(html.contains("publish next"), "kind label: {html}");
    assert!(job_row(&html, watch).contains(">watch check</span>"));
    assert!(html.contains("class=\"job-table-scroll\""));
    let tracked = job_row(&html, folder);
    assert!(tracked.contains("class=\"hint job-target\""));
    assert!(tracked.contains(&"folder".repeat(300)));
    assert!(
        tracked.contains("class=\"progress\""),
        "a multi-post import gets a bar: {tracked}"
    );
    assert!(tracked.contains("3/8"), "with its counts: {tracked}");
    // `1/1` on a one-target job said nothing, so those rows show a dash instead
    for (id, what) in [(failed, "a failed import"), (running, "a queued publish")] {
        let row = job_row(&html, id);
        assert!(
            !row.contains("class=\"progress\""),
            "{what} has nothing to track: {row}"
        );
        assert!(row.contains("—"), "{what} row: {row}");
    }
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
    // live progress only exists for a job that walks many posts, so the import
    // page needs one of those to have a bar to repaint
    h.state
        .db
        .call(|conn| {
            let id = job_db::create(
                conn,
                nujijushi::jobs::KIND_IMPORT,
                "https://x.test/folder",
                0,
            )?;
            job_db::progress(conn, id, 1, 4)?;
            job_db::set_state(conn, id, "running", None)?;
            Ok(id)
        })
        .await
        .unwrap();

    let watch = h
        .state
        .db
        .call(|conn| job_db::create(conn, nujijushi::jobs::KIND_WATCH_CHECK, "1", 0))
        .await
        .unwrap();
    let dashboard = body_text(h.send(h.get("/")).await).await;
    assert!(dashboard.contains("class=\"compact dashboard-jobs\""));
    assert!(job_row(&dashboard, watch).contains(">watch check</span>"));
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

#[tokio::test]
async fn both_pager_links_preserve_all_filters() {
    use nujijushi::db::{posts, queue};

    let h = harness();
    let channel = h.add_channel().await;
    let search = "sea + stars & moon?";
    let source = "archive & studio";
    let tag = "Night & Sky";
    let matching = h
        .state
        .db
        .with(|conn| {
            let mut matching = Vec::new();
            for index in 0..25 {
                let mut post = NormalizedPost::new(source, index.to_string());
                post.description = Some(search.into());
                post.tags = vec![tag.into()];
                let (id, _) = posts::upsert(conn, &post, &[])?;
                posts::set_state(conn, id, PostState::Approved)?;
                let queue_id = queue::enqueue_new(conn, id, channel.id, 0)?.unwrap();
                queue::mark_sent(conn, queue_id, "7", chrono::Utc::now())?;
                matching.push(id);
            }
            Ok(matching)
        })
        .unwrap();
    let nonmatching = h.post_only("unrelated").await;
    let filters = [
        ("search", search),
        ("state", "approved"),
        ("source", source),
        ("nsfw", "no"),
        ("sent", "yes"),
        ("tag", tag),
    ];
    let path = format!(
        "/posts?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(filters)
            .finish()
    );
    let link = |html: &str, rel: &str, page: &str| {
        let href = html
            .split_once(&format!("rel=\"{rel}\" href=\""))
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0;
        let href = htmlescape::decode_html(href).unwrap();
        let parsed: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(href.strip_prefix("/posts?").unwrap().as_bytes()).collect();
        assert_eq!(parsed.len(), filters.len() + 1);
        for (key, value) in filters {
            assert_eq!(parsed[key], value);
        }
        assert_eq!(parsed["page"], page);
        href
    };

    let first = body_text(h.send(h.get(&path)).await).await;
    assert!(first.contains("25 post(s)"));
    assert_eq!(first.matches("class=\"post-link\"").count(), 24);
    let second = body_text(h.send(h.get(&link(&first, "next", "2"))).await).await;
    assert!(second.contains("25 post(s)"));
    assert_eq!(second.matches("class=\"post-link\"").count(), 1);
    assert!(second.contains(&format!("href=\"/posts/{}\"", matching[0])));
    assert!(!second.contains(&format!("href=\"/posts/{nonmatching}\"")));
    assert!(!second.contains("rel=\"next\""));
    let back = body_text(h.send(h.get(&link(&second, "prev", "1"))).await).await;
    assert!(back.contains("25 post(s)"));
    assert_eq!(back.matches("class=\"post-link\"").count(), 24);
    assert!(!back.contains("rel=\"prev\""));
}

/// A rejected post is the curator's "no", so the grid hides it — but an explicit
/// state filter can always ask for it back, and the count agrees with the cards.
#[tokio::test]
async fn the_grid_hides_rejected_posts_until_asked() {
    let h = harness();
    let kept = h.post_only("1").await;
    let rejected = h.post_only("2").await;
    h.state
        .db
        .call(move |conn| nujijushi::db::posts::set_state(conn, rejected, PostState::Rejected))
        .await
        .unwrap();
    let card = |id: i64| format!("href=\"/posts/{id}\"");
    let counted = |n: usize| format!("{n} post(s)");

    let html = body_text(h.send(h.get("/posts")).await).await;
    assert!(html.contains(&card(kept)), "{html}");
    assert!(
        !html.contains(&card(rejected)),
        "a rejected post is hidden: {html}"
    );
    assert!(html.contains(&counted(1)), "the count agrees: {html}");
    assert!(
        html.contains("not rejected"),
        "the filter says what it does: {html}"
    );

    let all = body_text(h.send(h.get("/posts?state=all")).await).await;
    assert!(all.contains(&card(rejected)) && all.contains(&card(kept)));
    assert!(all.contains(&counted(2)), "{all}");

    let only = body_text(h.send(h.get("/posts?state=rejected")).await).await;
    assert!(only.contains(&card(rejected)), "{only}");
    assert!(
        !only.contains(&card(kept)),
        "rejected-only means rejected only: {only}"
    );
    assert!(only.contains(&counted(1)), "{only}");
    assert!(
        only.contains("<option value=\"rejected\" selected"),
        "the select keeps the choice: {only}"
    );
}

/// The post page says how much is stored for the post, not just how many files.
#[tokio::test]
async fn the_post_page_totals_the_media_size() {
    let h = harness();
    let sizes = [400_000i64, 350_000, 500_000, 250_000];
    let media: Vec<NewMedia> = sizes
        .iter()
        .enumerate()
        .map(|(i, bytes)| NewMedia {
            blob_hash: format!("big-{i}-").repeat(8),
            ext: "png".into(),
            mime: None,
            width: Some(100),
            height: Some(100),
            bytes: *bytes,
            is_video: false,
            poster_hash: None,
            sensitive: false,
            meta_json: None,
        })
        .collect();
    let post = NormalizedPost::new("pixiv", "sized");
    let post_id = h
        .state
        .db
        .call(move |conn| nujijushi::db::posts::upsert(conn, &post, &media))
        .await
        .unwrap()
        .0;

    let total = nujijushi::model::format_bytes(sizes.iter().sum::<i64>() as u64);
    let html = body_text(h.send(h.get(&format!("/posts/{post_id}"))).await).await;
    assert!(
        html.contains(&format!("{} item(s) · {total}", sizes.len())),
        "the heading totals the media: {html}"
    );
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

#[cfg(unix)]
#[tokio::test]
async fn watched_source_checks_persist_discovery_retries_and_curation() {
    use nujijushi::db::{posts, watches};
    use nujijushi::jobs::worker::JobWorker;
    use std::os::unix::fs::PermissionsExt;

    let mut h = harness();
    let base = h._dir.path();
    let script = base.join("fake-gallery-dl");
    std::fs::write(
        &script,
        r#"#!/bin/sh
set -eu
fixture_dir=$(dirname "$0")
scan=false
staging=
for arg in "$@"; do
    if [ "$staging" = 'next' ]; then staging="$arg"; fi
    if [ "$arg" = '-d' ]; then staging='next'; fi
    if [ "$arg" = '--dump-json' ]; then scan=true; fi
    target="$arg"
done
if "$scan"; then
    printf '%s\n' "$@" > "$fixture_dir/listing-args"
    cat "$fixture_dir/listing.json"
else
    id=$(basename "$target")
    printf '%s\n' "$id" >> "$fixture_dir/downloads"
    cp "$fixture_dir/source.png" "$staging/image.png"
    printf '{"id":%s,"category":"pixiv","tags":["source_tag"]}' "$id" > "$staging/image.png.json"
    if [ -f "$fixture_dir/fail-$id" ]; then exit 1; fi
fi
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    image::RgbImage::from_pixel(16, 16, image::Rgb([10, 20, 30]))
        .save(base.join("source.png"))
        .unwrap();
    Arc::make_mut(&mut h.state.cfg).gallery_dl = script.to_str().unwrap().into();
    h.ws = WebState::new(h.state.clone()).unwrap();

    let save = format!(
        "csrf={}&name=Pixiv+Art&url=https%3A%2F%2Fwww.pixiv.net%2Fen%2Fusers%2F123&initial_batch=2",
        h.ws.csrf
    );
    assert_eq!(
        h.send(h.post("/watches", &save.replace(&h.ws.csrf, "bad")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.send(h.post("/watches", &save)).await.status(),
        StatusCode::SEE_OTHER
    );
    let watch_id = h
        .state
        .db
        .with(|conn| Ok(watches::list(conn)?[0].id))
        .unwrap();
    let check_url = format!("/watches/{watch_id}/check");
    let form = format!("csrf={}", h.ws.csrf);
    let check = |h: &Harness| h.post(&check_url, &form);
    let body = |ids: &[(u32, u32)]| {
        let records: Vec<_> = ids.iter().map(|(id, day)| serde_json::json!([
            3, "https://i.pximg.net/image.png", {"id":id,"category":"pixiv","date":format!("2025-01-{day:02}T00:00:00Z")}
        ])).collect();
        serde_json::to_vec(&records).unwrap()
    };
    std::fs::write(base.join("listing.json"), body(&[(3, 3), (2, 2), (1, 1)])).unwrap();
    std::fs::write(base.join("fail-2"), "").unwrap();

    assert_eq!(h.send(check(&h)).await.status(), StatusCode::SEE_OTHER);
    let second = h.send(check(&h)).await;
    assert_eq!(Harness::flash(&second), "A check is already active");
    assert!(!base.join("listing-args").exists());
    let html = body_text(h.send(h.get("/watches")).await).await;
    assert!(html.contains("Pixiv Art") && html.contains("data-watch-action"));
    let status = body_text(h.send(h.get("/api/status")).await).await;
    assert!(status.contains("\"watch_jobs\"") && status.contains("\"watch_check\""));

    let worker = JobWorker::new(h.state.clone());
    assert_eq!(worker.drain().await.unwrap(), 1);
    let args = std::fs::read_to_string(base.join("listing-args")).unwrap();
    assert!(args.contains("--post-range\n1-2\n"));
    assert!(
        std::fs::read_to_string(base.join("downloads"))
            .unwrap()
            .contains("2\n3\n")
    );
    let post_id = h
        .state
        .db
        .with(|conn| {
            assert!(posts::find_by_source_id(conn, "pixiv", "1")?.is_none());
            assert!(posts::find_by_source_id(conn, "pixiv", "2")?.is_none());
            assert_eq!(watches::pending_count(conn, watch_id)?, 1);
            let id = posts::find_by_source_id(conn, "pixiv", "3")?.unwrap();
            posts::set_state(conn, id, PostState::Approved)?;
            nujijushi::db::tags::add_manual(conn, id, &["curated".into()])?;
            Ok(id)
        })
        .unwrap();
    let html = body_text(h.send(h.get("/watches")).await).await;
    assert!(html.contains("1 added · 0 skipped (already archived) · 1 failed · 1 pending"));

    let toggle_url = format!("/watches/{watch_id}/toggle");
    assert_eq!(
        h.send(h.post(&toggle_url, "csrf=bad&action=pause"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let pause = h
        .send(h.post(&toggle_url, &format!("{form}&action=pause")))
        .await;
    assert_eq!(Harness::flash(&pause), "Watch paused");
    assert_eq!(h.send(check(&h)).await.status(), StatusCode::BAD_REQUEST);
    let paused_page = body_text(h.send(h.get("/watches")).await).await;
    assert!(paused_page.contains("data-watch-paused disabled"));
    assert!(paused_page.contains(">Resume</button>"));

    std::fs::remove_file(base.join("fail-2")).unwrap();
    let already_archived = h.post_only("6").await;
    h.state
        .db
        .with(|conn| posts::set_notes(conn, already_archived, Some("curator note")))
        .unwrap();
    std::fs::write(
        base.join("listing.json"),
        body(&[(6, 5), (5, 4), (4, 3), (3, 3), (5, 4)]),
    )
    .unwrap();
    let reopened = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();
    reopened
        .db
        .with(|conn| {
            let watch = watches::get(conn, watch_id)?;
            assert!(!watch.enabled);
            assert!(watch.checkpoint.is_some());
            assert_eq!(watches::pending_count(conn, watch_id)?, 1);
            Ok(())
        })
        .unwrap();
    let resume = h
        .send(h.post(&toggle_url, &format!("{form}&action=resume")))
        .await;
    assert_eq!(Harness::flash(&resume), "Watch resumed");
    let worker = JobWorker::new(reopened);
    assert_eq!(h.send(check(&h)).await.status(), StatusCode::SEE_OTHER);
    assert_eq!(worker.drain().await.unwrap(), 1);
    let downloads = std::fs::read_to_string(base.join("downloads")).unwrap();
    assert_eq!(downloads, "2\n3\n4\n5\n2\n");
    let html = body_text(h.send(h.get("/watches")).await).await;
    assert!(html.contains("3 added · 1 skipped (already archived) · 0 failed"));
    assert!(
        std::fs::read_to_string(base.join("listing-args"))
            .unwrap()
            .contains("--date-after\n")
    );
    h.state
        .db
        .with(|conn| {
            let post = posts::get(conn, post_id)?;
            assert_eq!(post.state, PostState::Approved);
            assert!(post.tags.contains(&"curated".into()));
            assert_eq!(
                posts::get(conn, already_archived)?.notes.as_deref(),
                Some("curator note")
            );
            assert_eq!(watches::pending_count(conn, watch_id)?, 0);
            assert!(nujijushi::db::queue::for_post(conn, post_id)?.is_empty());
            Ok(())
        })
        .unwrap();

    let _response = h.send(check(&h)).await;
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(base.join("downloads")).unwrap(),
        downloads
    );

    let checkpoint = h
        .state
        .db
        .with(|conn| Ok(watches::get(conn, watch_id)?.checkpoint))
        .unwrap();
    std::fs::write(
        base.join("listing.json"),
        br#"[[-1,{"error":"AuthRequired","message":"hidden"}]]"#,
    )
    .unwrap();
    let _response = h.send(check(&h)).await;
    assert_eq!(worker.drain().await.unwrap(), 1);
    let html = body_text(h.send(h.get("/watches")).await).await;
    assert!(html.contains("gallery-dl feed scan failed (AuthRequired)"));
    assert_eq!(
        h.state
            .db
            .with(|conn| Ok(watches::get(conn, watch_id)?.checkpoint))
            .unwrap(),
        checkpoint
    );

    let remove_url = format!("/watches/{watch_id}/remove");
    assert_eq!(
        h.send(h.post(&remove_url, &form)).await.status(),
        StatusCode::SEE_OTHER
    );
    assert!(h.state.db.with(|conn| posts::get(conn, post_id)).is_ok());
}

fn artwork() -> image::RgbImage {
    image::RgbImage::from_fn(180, 144, |x, y| {
        let color = match x / 20 {
            0 | 4 => 230,
            2 | 5 | 8 => 45,
            _ => 130,
        };
        image::Rgb([color - (y / 12) as u8, color, color])
    })
}

fn encode_image(img: image::RgbImage, format: image::ImageFormat) -> Vec<u8> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::from(img)
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}

fn archive_test_images(state: &AppState, source: &str, id: &str, files: &[(&[u8], &str)]) -> i64 {
    let mut post = NormalizedPost::new(source, id);
    post.title = Some(format!("{source} artwork"));
    post.url = Some(format!("https://example.test/{source}/{id}"));
    let mut items = Vec::new();
    for (bytes, ext) in files {
        let blob = state.blobs.ingest_bytes(bytes, ext).unwrap();
        state.blobs.write_thumb(&blob.hash, bytes, 512).unwrap();
        items.push(NewMedia {
            blob_hash: blob.hash,
            ext: blob.ext,
            mime: None,
            width: blob.width,
            height: blob.height,
            bytes: blob.bytes,
            is_video: false,
            poster_hash: None,
            sensitive: false,
            meta_json: None,
        });
    }
    state
        .db
        .with(|conn| nujijushi::db::posts::upsert(conn, &post, &items))
        .unwrap()
        .0
}

#[tokio::test]
async fn duplicate_inbox_indexes_and_dismisses_cross_source_media_pairs() {
    use nujijushi::db::{duplicates, posts, queue};
    let h = harness();
    let channel = h.add_channel().await;
    let pattern = artwork();
    let original = encode_image(pattern.clone(), image::ImageFormat::Png);
    let resized =
        image::imageops::resize(&pattern, 270, 216, image::imageops::FilterType::Triangle);
    let recompressed = encode_image(resized, image::ImageFormat::Jpeg);
    let unrelated = encode_image(
        image::imageops::flip_horizontal(&pattern),
        image::ImageFormat::Png,
    );
    let blank = encode_image(
        image::RgbImage::from_pixel(30, 200, image::Rgb([50, 90, 120])),
        image::ImageFormat::Png,
    );
    let mut animated = Vec::new();
    image::codecs::gif::GifEncoder::new(&mut animated)
        .encode_frames([
            image::Frame::new(image::DynamicImage::ImageRgb8(pattern).to_rgba8()),
            image::Frame::new(image::RgbaImage::new(180, 144)),
        ])
        .unwrap();

    let left = archive_test_images(
        &h.state,
        "pixiv",
        "100",
        &[(&blank, "png"), (&original, "png")],
    );
    let right = archive_test_images(&h.state, "twitter", "102", &[(&recompressed, "jpg")]);
    let exact = archive_test_images(&h.state, "danbooru", "103", &[(&original, "png")]);
    archive_test_images(&h.state, "local", "104", &[(&unrelated, "png")]);
    archive_test_images(&h.state, "twitter", "105", &[(&animated, "gif")]);

    h.state
        .db
        .with(|conn| {
            posts::set_state(conn, left, PostState::Approved)?;
            queue::enqueue(conn, left, channel.id, 0)?;
            Ok(())
        })
        .unwrap();
    let page = body_text(h.send(h.get("/duplicates")).await).await;
    assert!(page.contains("data-duplicates-indexing"));
    nujijushi::duplicates::index_batch(&h.state).await.unwrap();
    let pairs = h.state.db.with(|conn| duplicates::pairs(conn, 0)).unwrap();
    assert_eq!(pairs.first(), Some(&(left, exact)));
    assert!(pairs.contains(&(left, right)));
    assert_eq!(pairs.len(), 3, "{pairs:?}");
    let html = body_text(h.send(h.get("/duplicates")).await).await;
    assert!(html.contains("Exact file") && html.contains("Visually similar"));
    assert!(html.contains("Image 2") && html.contains("approved") && html.contains("queued"));
    assert!(html.contains("https:&#x2f;&#x2f;example.test&#x2f;twitter&#x2f;102"));
    let reject = format!("csrf=bad&left={left}&right={right}");
    assert_eq!(
        h.send(h.post("/duplicates/dismiss", &reject))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let confirm = format!("csrf={}&left={right}&right={left}", h.ws.csrf);
    assert_eq!(
        h.send(h.post("/duplicates/dismiss", &confirm))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let reopened = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();
    let updated = archive_test_images(
        &reopened,
        "twitter",
        "102",
        &[(&unrelated, "png"), (&recompressed, "jpg")],
    );
    assert_eq!(updated, right);
    nujijushi::duplicates::index_batch(&reopened).await.unwrap();
    reopened
        .db
        .with(|conn| {
            assert!(!duplicates::pairs(conn, 0)?.contains(&(left, right)));
            assert_eq!(posts::get(conn, left)?.state, PostState::Approved);
            assert_eq!(queue::for_post(conn, left)?[0].2, QueueState::Queued);
            assert_eq!(
                duplicates::evidence(conn, right, exact)?[0].left_position,
                2
            );
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn watched_scan_does_not_delay_a_later_publish_job() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = harness();
    let base = h._dir.path();
    let script = base.join("blocked-gallery-dl");
    std::fs::write(
        &script,
        r#"#!/bin/sh
dir=$(dirname "$0")
: > "$dir/watch-started"
while [ ! -f "$dir/watch-release" ]; do sleep 0.05; done
printf '[]\n'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    Arc::make_mut(&mut h.state.cfg).gallery_dl = script.to_str().unwrap().into();
    h.state
        .db
        .with(|conn| {
            nujijushi::db::watches::create(
                conn,
                "Artist",
                "https://www.pixiv.net/users/10/artworks",
                10,
            )
        })
        .unwrap();
    let (watch_id, _) = nujijushi::jobs::create_watch_check_job(&h.state, 1)
        .await
        .unwrap();

    let channel = h.add_channel().await;
    let (_, queue_id) = h.queue_a_post("95", &channel).await;
    let worker = nujijushi::jobs::worker::JobWorker::new(h.state.clone());
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        worker
            .run(async {
                let _ = stop_rx.await;
            })
            .await
            .unwrap();
    });
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        while !base.join("watch-started").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let id = nujijushi::jobs::create_publish_item_job(&h.state, queue_id)
            .await
            .unwrap()
            .unwrap();
        while h.job_state(id).await != "done" {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(h.job_state(watch_id).await, "running");
    assert_eq!(h.mock.send_count(), 1);
    std::fs::write(base.join("watch-release"), "").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while h.job_state(watch_id).await != "done" {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    stop_tx.send(()).unwrap();
    task.await.unwrap();
}

fn tagger_harness(port: u16) -> Harness {
    let mut h = harness();
    let cfg = Arc::make_mut(&mut h.state.cfg);
    cfg.tag_suggestions.enabled = true;
    cfg.tag_suggestions.tagger_port = std::num::NonZeroU16::new(port).unwrap();
    h.ws = WebState::new(h.state.clone()).unwrap();
    h
}

async fn add_tagger_post(h: &Harness, source_id: &str) -> i64 {
    let mut post = NormalizedPost::new("local", source_id);
    post.tags = vec!["hidden_source".into(), "original".into()];
    post.raw_json = Some(r#"{"tag_string_copyright":"source_series"}"#.into());
    let media = [
        (b"still-one".as_slice(), false),
        (b"video".as_slice(), true),
        (b"still-two".as_slice(), false),
        (b"animated".as_slice(), false),
    ]
    .iter()
    .enumerate()
    .map(|(index, &(bytes, is_video))| {
        let hash = format!("{source_id}{index}deadbeef");
        let ext = if is_video { "mp4" } else { "png" };
        let path = h.state.blobs.path_for(&hash, ext);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        NewMedia {
            blob_hash: hash,
            ext: ext.into(),
            mime: None,
            width: None,
            height: None,
            bytes: bytes.len() as i64,
            is_video,
            poster_hash: None,
            sensitive: false,
            meta_json: None,
        }
    })
    .collect::<Vec<_>>();
    h.state
        .db
        .call(move |conn| Ok(nujijushi::db::posts::upsert(conn, &post, &media)?.0))
        .await
        .unwrap()
}

#[tokio::test]
async fn tag_suggestions_review_is_explicit_and_preserves_existing_tags_and_removals() {
    use axum::{
        Router,
        body::{Bytes, to_bytes},
        response::IntoResponse,
        routing::post,
    };
    use nujijushi::db::{posts, tag_suggestions, tags};
    use nujijushi::jobs::worker::JobWorker;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let companion = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut h = tagger_harness(companion.local_addr().unwrap().port());
    Arc::make_mut(&mut h.state.cfg).proxy_url =
        Some(format!("http://{}", proxy.local_addr().unwrap()));
    h.ws = WebState::new(h.state.clone()).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let handler = {
        let requests = requests.clone();
        move |body: Bytes| {
            let requests = requests.clone();
            async move {
                requests.fetch_add(1, Ordering::Relaxed);
                match body.as_ref() {
                    b"still-one" => "3861\tpikachu\t0.96\n3901\thidden_source\t0.93\n3902\tunknown_character\t0.92\n3903\tweak_character\t0.7\n3904\tother_guess\t0.8\n3905\tpikachu_alias\t0.89\n3906\toriginal\t0.88\n".into_response(),
                    b"still-two" => "3861\tpikachu\t0.97\n3902\tunknown_character\t0.85\n3903\tweak_character\t0.84\n".into_response(),
                    b"animated" => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
                    _ => panic!("unexpected image request"),
                }
            }
        }
    };
    let server = tokio::spawn(async move {
        axum::serve(companion, Router::new().route("/v1/predict", post(handler)))
            .await
            .unwrap()
    });
    let post_id = add_tagger_post(&h, "tagged01").await;
    h.state
        .db
        .with(|conn| {
            let source = tags::provenance(conn, post_id)?
                .into_iter()
                .find(|tag| tag.name == "hidden_source")
                .unwrap();
            assert!(tags::suppress_source(conn, post_id, source.id)?);
            tags::set_alias(conn, "pikachu_alias", "pikachu")?;
            tags::set_alias(conn, "variant_spelling", "fixed_character")
        })
        .unwrap();

    let path = format!("/posts/{post_id}/tag-suggestions");
    let generate = format!("csrf={}&action=generate", h.ws.csrf);
    let mut anonymous = h.post(&path, &generate);
    anonymous.headers_mut().remove(header::COOKIE);
    assert!(h.send(anonymous).await.status().is_redirection());
    assert_eq!(
        h.send(h.post(&path, "action=generate&csrf=bad"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let (first, second) = tokio::join!(
        h.send(h.post(&path, &generate)),
        h.send(h.post(&path, &generate))
    );
    assert_eq!(first.status(), StatusCode::SEE_OTHER);
    assert_eq!(second.status(), StatusCode::SEE_OTHER);
    assert!(h.jobs().await.len() == 1);
    assert_eq!(requests.load(Ordering::Relaxed), 0);
    let job = &h.jobs().await[0];
    assert_eq!(job.state, "pending");

    h.state
        .db
        .with(|conn| {
            for _ in 0..26 {
                let id = job_db::create(conn, "test", "", 0)?;
                job_db::set_state(conn, id, "done", None)?;
            }
            Ok(())
        })
        .unwrap();
    let status = h
        .send(h.get(&format!("/api/status?tag_post={post_id}")))
        .await;
    let body = to_bytes(status.into_body(), usize::MAX).await.unwrap();
    let status: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(status["tag_suggestion_job"]["id"], job.id);
    assert!(
        status["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["id"] != job.id)
    );

    assert_eq!(JobWorker::new(h.state.clone()).drain().await.unwrap(), 1);
    assert_eq!(h.job_state(job.id).await, "done");
    assert_eq!(requests.load(Ordering::Relaxed), 3);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), proxy.accept())
            .await
            .is_err()
    );
    let detail = h.send(h.get(&format!("/posts/{post_id}"))).await;
    let html = String::from_utf8(
        to_bytes(detail.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("href=\"#media-1\">Image 1"), "{html}");
    assert!(html.contains("href=\"#media-3\">Image 3"));
    assert!(!html.contains("href=\"#media-2\">Image 2"));
    assert!(html.contains("Low confidence"));
    let review = h
        .state
        .db
        .with(|conn| {
            assert_eq!(tags::for_post(conn, post_id)?, vec!["original"]);
            Ok(tag_suggestions::review(conn, &posts::get(conn, post_id)?)?.unwrap())
        })
        .unwrap();
    assert_eq!(
        review
            .suggestions
            .iter()
            .map(|item| item.tag.as_str())
            .collect::<Vec<_>>(),
        [
            "pikachu",
            "unknown_character",
            "weak_character",
            "source_series",
            "pokemon"
        ]
    );
    assert_eq!(review.suggestions[0].images, [1, 3]);
    assert_eq!(review.suggestions[1].images, [1]);
    assert_eq!(review.suggestions[2].images, [3]);
    let resolve = |id, action, input| {
        format!(
            "csrf={}&job_id={}&suggestion_id={id}&action={action}&tag={input}",
            h.ws.csrf, job.id
        )
    };
    let character = review.suggestions[0].id;
    let status = h
        .send(h.post(&path, &resolve(character, "accept", "hidden_source")))
        .await
        .status();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    h.state
        .db
        .with(|conn| tags::add_manual(conn, post_id, &["keep_manual".into()]))
        .unwrap();
    let saved = h
        .send(h.post(&path, &resolve(character, "accept", "Variant+Spelling")))
        .await;
    assert_eq!(saved.status(), StatusCode::SEE_OTHER);
    let discarded = h
        .send(h.post(
            &path,
            &resolve(review.suggestions[4].id, "discard", "pokemon"),
        ))
        .await;
    assert_eq!(discarded.status(), StatusCode::SEE_OTHER);
    h.state
        .db
        .with(|conn| {
            assert_eq!(
                tags::for_post(conn, post_id)?,
                vec!["original", "keep_manual", "fixed_character"]
            );
            assert_eq!(tags::removals(conn, post_id)?[0].name, "hidden_source");
            let review = tag_suggestions::review(conn, &posts::get(conn, post_id)?)?.unwrap();
            assert_eq!(review.suggestions.len(), 3);
            let mut updated = NormalizedPost::new("local", "tagged01");
            updated.tags = vec!["hidden_source".into(), "original".into()];
            updated.raw_json = Some("{}".into());
            posts::upsert(conn, &updated, &[])?;
            assert!(
                tag_suggestions::review(conn, &posts::get(conn, post_id)?)?
                    .unwrap()
                    .stale
            );
            Ok(())
        })
        .unwrap();
    let stale = h
        .send(h.post(&path, &resolve(review.suggestions[2].id, "accept", "stale")))
        .await;
    assert_eq!(stale.status(), StatusCode::BAD_REQUEST);
    server.abort();
}

#[tokio::test]
async fn weak_character_guesses_fill_three_slots_and_use_no_franchise_lookup() {
    use axum::{Router, routing::post};
    use nujijushi::db::{posts, tag_suggestions};
    use nujijushi::jobs::worker::JobWorker;

    let companion = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut h = tagger_harness(companion.local_addr().unwrap().port());
    let config = Arc::make_mut(&mut h.state.cfg);
    config.tag_suggestions.online_fallback = true;
    config.proxy_url = Some(format!("http://{}", proxy.local_addr().unwrap()));
    let router = Router::new().route(
        "/v1/predict",
        post(|| async {
            "3861\tpikachu\t0.85\n3902\tsecond_guess\t0.61\n3903\tthird_guess\t0.5\n3904\tfourth_guess\t0.4\n"
        }),
    );
    let server = tokio::spawn(async move { axum::serve(companion, router).await.unwrap() });
    let post_id = add_tagger_post(&h, "weak-guesses").await;
    nujijushi::jobs::create_tag_suggestions_job(&h.state, post_id)
        .await
        .unwrap();
    assert_eq!(JobWorker::new(h.state.clone()).drain().await.unwrap(), 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(40), proxy.accept())
            .await
            .is_err()
    );
    h.state
        .db
        .with(|conn| {
            let review = tag_suggestions::review(conn, &posts::get(conn, post_id)?)?.unwrap();
            assert_eq!(review.job.state, "done");
            assert_eq!(
                review
                    .suggestions
                    .iter()
                    .map(|item| item.tag.as_str())
                    .collect::<Vec<_>>(),
                ["pikachu", "second_guess", "third_guess", "source_series"],
            );
            for item in &review.suggestions[..3] {
                assert!(item.evidence.contains("Low confidence"));
                assert_eq!(item.images, [1]);
            }
            Ok(())
        })
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn restarted_suggestion_worker_fails_abandoned_jobs_and_keeps_pending_requests() {
    let h = tagger_harness(8790);
    let abandoned = add_tagger_post(&h, "abandoned01").await;
    let pending = add_tagger_post(&h, "pending001").await;
    let (abandoned, _) = nujijushi::jobs::create_tag_suggestions_job(&h.state, abandoned)
        .await
        .unwrap();
    let (pending, _) = nujijushi::jobs::create_tag_suggestions_job(&h.state, pending)
        .await
        .unwrap();
    h.state
        .db
        .with(|conn| {
            assert_eq!(job_db::claim_pending(conn, 1)?[0].id, abandoned);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        nujijushi::jobs::worker::reconcile(&h.state).await.unwrap(),
        1
    );
    assert_eq!(h.job_state(abandoned).await, "failed");
    assert_eq!(h.job_state(pending).await, "pending");
}

#[tokio::test]
async fn a_slow_tagger_keeps_its_archive_lock_and_does_not_delay_publishing() {
    use nujijushi::jobs::worker::{JobWorker, reconcile};
    use std::time::Duration;
    use tokio::sync::Notify;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let h = tagger_harness(listener.local_addr().unwrap().port());
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let handler = {
        let started = started.clone();
        let release = release.clone();
        move |body: axum::body::Bytes| {
            let started = started.clone();
            let release = release.clone();
            async move {
                if body.as_ref() == b"still-one" {
                    started.notify_one();
                    release.notified().await;
                }
                ""
            }
        }
    };
    let server = tokio::spawn(async move {
        let router = axum::Router::new().route("/v1/predict", axum::routing::post(handler));
        axum::serve(listener, router).await.unwrap();
    });
    let post = add_tagger_post(&h, "slowtag01").await;
    let waiting = add_tagger_post(&h, "waiting01").await;
    let (slow, _) = nujijushi::jobs::create_tag_suggestions_job(&h.state, post)
        .await
        .unwrap();
    let (waiting, _) = nujijushi::jobs::create_tag_suggestions_job(&h.state, waiting)
        .await
        .unwrap();
    let worker = JobWorker::new(h.state.clone());
    let active = tokio::spawn(async move { worker.drain().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let other = AppState::open((*h.state.cfg).clone(), Some(h.mock.clone())).unwrap();
    assert_eq!(reconcile(&other).await.unwrap(), 0);
    let (_, duplicate) = nujijushi::jobs::create_tag_suggestions_job(&other, post)
        .await
        .unwrap();
    assert!(!duplicate);
    let channel = h.add_channel().await;
    let (_, queue_id) = h.queue_a_post("95", &channel).await;
    let publish = nujijushi::jobs::create_publish_item_job(&other, queue_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), JobWorker::new(other).drain())
            .await
            .unwrap()
            .unwrap(),
        1
    );
    assert_eq!(h.job_state(publish).await, "done");
    assert_eq!(h.mock.send_count(), 1);
    assert_eq!(h.job_state(slow).await, "running");
    assert_eq!(h.job_state(waiting).await, "pending");
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), active)
            .await
            .unwrap()
            .unwrap(),
        1
    );
    assert_eq!(h.job_state(slow).await, "done");
    server.abort();
}

#[tokio::test]
async fn gallery_merges_keep_the_post_and_can_restore_source_panels_after_reopening() {
    use nujijushi::db::{duplicates, media_merges, mirrors, posts, queue, reputation, tags};
    use nujijushi::jobs::{self, worker::JobWorker};
    use nujijushi::source::PostFile;

    let mut h = harness();
    let channel = h.add_channel().await;
    let mut imported = NormalizedPost::new("twitter", "panels");
    imported.url = Some("https://x.test/artist/status/123".into());
    imported.tags = vec!["original".into()];
    for i in 0..5 {
        let path = h._dir.path().join(format!("panel-{i}.png"));
        image::RgbaImage::from_pixel(8 + i, 8, image::Rgba([i as u8 * 40, 130, 10, 250]))
            .save(&path)
            .unwrap();
        imported.files.push(PostFile {
            path,
            is_video: false,
            sensitive: i == 1,
            meta: Some(format!(r#"{{"index":{i}}}"#)),
        });
    }
    let outcome = jobs::import_posts(&h.state, vec![imported.clone()], None, None)
        .await
        .unwrap();
    let id = outcome.post_ids[0];
    let get_post = || h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let originals = get_post();
    let queue_id = h
        .state
        .db
        .with(|conn| {
            tags::add_manual(conn, id, &["favorite".into()])?;
            let queue_id = queue::enqueue_new(conn, id, channel.id, 0)?.unwrap();
            queue::mark_sent(conn, queue_id, "481", chrono::Utc::now())?;
            mirrors::enqueue(conn, id, "local")?;
            mirrors::mark_done(conn, id, "local", Some("past-copy"), 125)?;
            Ok(queue_id)
        })
        .unwrap();
    let path = format!("/posts/{id}/media");
    let form = |media: &[nujijushi::model::MediaItem], selected: &[usize]| {
        let version = media_merges::gallery_version(media);
        let mut body = format!("csrf={}&action=merge&gallery_version={version}", h.ws.csrf);
        for &index in selected {
            body.push_str(&format!("&sel_{}=on", media[index].id));
        }
        body
    };
    let page = body_text(h.send(h.get(&format!("/posts/{id}"))).await).await;
    assert!(page.contains("Merge side by side"));
    assert_eq!(
        h.send(h.post(&path, "csrf=wrong&action=merge"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.send(h.post(&path, &form(&originals.media, &[0, 2])))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let first_merge = form(&originals.media, &[1, 0]);
    let mut anonymous = h.post(&path, &first_merge);
    anonymous.headers_mut().remove(header::COOKIE);
    assert!(h.send(anonymous).await.status().is_redirection());
    assert_eq!(
        h.send(h.post(&path, &first_merge)).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        h.send(h.post(&path, &first_merge)).await.status(),
        StatusCode::SEE_OTHER
    );
    let pending = h
        .state
        .db
        .with(|conn| media_merges::latest_job(conn, id))
        .unwrap()
        .unwrap();
    assert_eq!(pending.state, "pending");
    assert_eq!(get_post().media, originals.media);
    let status = body_text(h.send(h.get(&format!("/api/status?merge_post={id}"))).await).await;
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["media_merge_job"]["id"], pending.id);
    assert_eq!(status["media_merge_job"]["progress"], 0);
    assert_eq!(status["media_merge_job"]["total"], 2);
    let worker = JobWorker::new(h.state.clone());
    assert_eq!(worker.drain().await.unwrap(), 1);
    let partial = get_post();
    assert_eq!(partial.media.len(), 4);
    assert_eq!(partial.media[0].width, Some(17));
    assert!(partial.media[0].sensitive);
    assert_eq!(partial.media[1].blob_hash, originals.media[2].blob_hash);
    assert!(h.state.blobs.thumb_exists(&partial.media[0].blob_hash));
    let done = body_text(h.send(h.get(&format!("/posts/{id}"))).await).await;
    assert!(
        done.contains("Undo merge") && done.contains("Merged 2 images"),
        "{done}"
    );
    assert!(done.contains("data-role=\"counts\">2/2"));

    let panel = std::fs::read(&imported.files[0].path).unwrap();
    let other = encode_image(
        image::RgbImage::from_pixel(7, 8, image::Rgb([7, 8, 9])),
        image::ImageFormat::Png,
    );
    let other_id = archive_test_images(
        &h.state,
        "pixiv",
        "matching-panel",
        &[(&panel, "png"), (&other, "png")],
    );
    let check_retained_match = || {
        h.state
            .db
            .with(|conn| {
                assert!(duplicates::pairs(conn, 0)?.contains(&(id, other_id)));
                assert!(
                    duplicates::evidence(conn, id, other_id)?
                        .iter()
                        .any(|evidence| evidence.exact
                            && evidence.left_hash == originals.media[0].blob_hash)
                );
                Ok(())
            })
            .unwrap();
    };
    check_retained_match();
    let other_post = h.state.db.with(|conn| posts::get(conn, other_id)).unwrap();
    assert_eq!(
        h.send(h.post(
            &format!("/posts/{other_id}/media"),
            &form(&other_post.media, &[0, 1]),
        ))
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    check_retained_match();
    h.state
        .db
        .with(|conn| {
            assert!(duplicates::dismiss(conn, id, other_id)?);
            assert!(!duplicates::pairs(conn, 0)?.contains(&(id, other_id)));
            Ok(())
        })
        .unwrap();

    assert_eq!(
        h.send(h.post(&path, &form(&partial.media, &[2, 3])))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    let two_merges = get_post();
    assert_eq!(
        two_merges
            .media
            .iter()
            .map(|item| item.width.unwrap())
            .collect::<Vec<_>>(),
        [17, 10, 23]
    );
    h.state
        .db
        .with(|conn| {
            assert_eq!(
                media_merges::undo_ids(conn, id)?
                    .iter()
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>(),
                [0, 2]
            );
            let source_media: Vec<_> = originals.media.iter().map(NewMedia::from).collect();
            let duplicate = reputation::match_suggestion(
                conn,
                &h.state.cfg.access,
                "local",
                "copy-of-panels",
                &source_media,
                91,
            )?;
            assert_eq!(duplicate.unwrap().post_id, Some(id));
            Ok(())
        })
        .unwrap();
    let gallery_before_refresh = get_post().media;
    imported.files[4].sensitive = true;
    imported.files[4].meta = Some(r#"{"refreshed":true}"#.into());
    let updated = jobs::import_posts(&h.state, vec![imported.clone()], None, None)
        .await
        .unwrap();
    assert_eq!(updated.gallery_edits_reset, 0);
    let kept = get_post();
    assert_eq!(kept.media.len(), 3);
    assert_eq!(kept.media[2].blob_hash, gallery_before_refresh[2].blob_hash);
    assert!(kept.media[2].sensitive);
    assert_eq!(
        h.send(h.post(&path, &form(&kept.media, &[0, 1, 2])))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    let combined = get_post();
    assert_eq!(combined.media.len(), 1);
    assert_eq!(combined.media[0].width, Some(50));
    assert_eq!(combined.url, originals.url);
    assert_eq!(combined.state, originals.state);
    assert_eq!(combined.tags, ["favorite", "original"]);
    let rules = h.state.db.with(|conn| tags::TagRules::load(conn)).unwrap();
    let decision = nujijushi::nsfw::evaluate(&combined, &channel, &rules);
    let preview =
        nujijushi::tg::publish::build_plan(&h.state, &channel, &combined, queue_id, decision)
            .unwrap();
    assert_eq!(preview.messages[0].media.len(), 1);
    assert_eq!(
        preview.messages[0].media[0].path,
        h.state.blobs.path_for(&combined.media[0].blob_hash, "png")
    );
    h.state
        .db
        .with(|conn| {
            let sent = queue::get(conn, queue_id)?;
            assert_eq!(sent.state, QueueState::Sent);
            assert_eq!(sent.tg_message_ids.as_deref(), Some("481"));
            assert_eq!(
                mirrors::get(conn, id, "local")?
                    .unwrap()
                    .external_id
                    .as_deref(),
                Some("past-copy")
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(h.mock.send_count(), 0);

    let reopened = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();
    reopened.db.with(|conn| reopened.blobs.gc(conn)).unwrap();
    for source in &originals.media {
        assert!(
            reopened
                .blobs
                .path_for(&source.blob_hash, &source.ext)
                .is_file()
        );
    }
    let undo_id = reopened
        .db
        .with(|conn| media_merges::undo_ids(conn, id))
        .unwrap()[0]
        .1;
    h.ws = WebState::new(reopened).unwrap();
    let version = media_merges::gallery_version(&combined.media);
    let undo = format!(
        "csrf={}&action=undo&merge_id={undo_id}&gallery_version={version}",
        h.ws.csrf
    );
    assert_eq!(
        h.send(h.post(&path, &undo)).await.status(),
        StatusCode::SEE_OTHER
    );
    let restored = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    assert_eq!(
        restored
            .media
            .iter()
            .map(|item| item.blob_hash.as_str())
            .collect::<Vec<_>>(),
        originals
            .media
            .iter()
            .map(|item| item.blob_hash.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        restored.media[4].meta_json.as_deref(),
        Some(r#"{"refreshed":true}"#)
    );
    assert!(restored.media[4].sensitive);
    let version = media_merges::gallery_version(&restored.media);
    assert!(
        jobs::create_merge_media_job(
            &h.state,
            id,
            restored.media[..2].iter().map(|item| item.id).collect(),
            version,
            None,
        )
        .await
        .unwrap()
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    imported.files.swap(0, 1);
    let refreshed = jobs::import_posts(&h.state, vec![imported], None, None)
        .await
        .unwrap();
    assert_eq!(refreshed.post_ids, [id]);
    assert_eq!(refreshed.gallery_edits_reset, 1);
    assert!(refreshed.summary().contains("source gallery changed"));
    let refreshed = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    assert_eq!(refreshed.media.len(), 5);
    assert_eq!(refreshed.media[0].blob_hash, originals.media[1].blob_hash);
    assert!(
        h.state
            .db
            .with(|conn| media_merges::undo_ids(conn, id))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_and_stale_merge_jobs_leave_the_current_gallery_intact() {
    use nujijushi::db::{media_merges, posts};
    use nujijushi::jobs::{self, worker::JobWorker};

    let h = harness();
    let panel = |height| {
        encode_image(
            image::RgbImage::from_pixel(8, height, image::Rgb([12, 34, 56])),
            image::ImageFormat::Png,
        )
    };
    let left = panel(8);
    let wrong_height = panel(10);
    let changed = panel(12);
    let id = archive_test_images(
        &h.state,
        "twitter",
        "fails",
        &[(&left, "png"), (&wrong_height, "png")],
    );
    let get = || h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let enqueue = |post: nujijushi::model::Post| {
        let version = media_merges::gallery_version(&post.media);
        let ids = post.media.iter().map(|item| item.id).collect();
        jobs::create_merge_media_job(&h.state, id, ids, version, None)
    };
    let latest_job = || {
        h.state
            .db
            .with(|conn| media_merges::latest_job(conn, id))
            .unwrap()
            .unwrap()
    };
    let original = get();
    let worker = JobWorker::new(h.state.clone());
    assert!(enqueue(original.clone()).await.unwrap());
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(latest_job().state, "failed");
    assert!(latest_job().message.unwrap().contains("same height"));
    assert_eq!(get().media, original.media);
    assert!(enqueue(original).await.unwrap());
    assert_eq!(
        archive_test_images(
            &h.state,
            "twitter",
            "fails",
            &[(&changed, "png"), (&left, "png")]
        ),
        id
    );
    let replaced = get();
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(latest_job().state, "failed");
    assert!(latest_job().message.unwrap().contains("gallery changed"));
    assert_eq!(get().media, replaced.media);
}

#[tokio::test]
async fn fit_photo_is_reversible_and_merge_output_can_use_the_same_settings() {
    use nujijushi::db::{media_merges, posts};
    use nujijushi::jobs::worker::JobWorker;

    let mut h = harness();
    let left = encode_image(
        image::RgbImage::from_pixel(190, 8, image::Rgb([250, 40, 30])),
        image::ImageFormat::Png,
    );
    let right = encode_image(
        image::RgbImage::from_pixel(250, 8, image::Rgb([30, 70, 250])),
        image::ImageFormat::Png,
    );
    let inputs = [(left.as_slice(), "png"), (right.as_slice(), "png")];
    let post_id = archive_test_images(&h.state, "twitter", "panorama", &inputs);
    let post_url = format!("/posts/{post_id}");
    let action_url = format!("{post_url}/media");
    let get = |state: &AppState| state.db.with(|conn| posts::get(conn, post_id)).unwrap();
    let original = get(&h.state);
    let first = &original.media[0];
    let version = media_merges::gallery_version(&original.media);
    let page = body_text(h.send(h.get(&post_url)).await).await;
    assert!(page.contains("Compress image"));
    assert!(page.contains("Exceeds the photo upload limits"));
    let form = format!(
        "action=compress&media_id={}&gallery_version={version}&output=fit",
        first.id
    );
    assert_eq!(
        h.send(h.post(&action_url, &format!("csrf=wrong&{form}")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let form = format!("csrf={}&{form}", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&action_url, &format!("{form}&quality=101")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.send(h.post(&action_url, &format!("{form}&quality=85")))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let worker = JobWorker::new(h.state.clone());
    assert_eq!(worker.drain().await.unwrap(), 1);
    let compressed = get(&h.state);
    let preview = &compressed.media[0];
    assert_eq!(
        (preview.ext.as_str(), preview.width, preview.height),
        ("jpg", Some(190), Some(10))
    );
    assert_eq!(compressed.media[1].blob_hash, original.media[1].blob_hash);
    let status = body_text(
        h.send(h.get(&format!("/api/status?merge_post={post_id}")))
            .await,
    )
    .await;
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["media_merge_job"]["kind"], "compress_media");
    assert_eq!(status["media_merge_job"]["state"], "done");
    let page = body_text(h.send(h.get(&post_url)).await).await;
    assert!(page.contains("Undo compression"));

    assert_eq!(
        archive_test_images(&h.state, "twitter", "panorama", &inputs),
        post_id
    );
    assert_eq!(get(&h.state).media[0].blob_hash, preview.blob_hash);
    let reopened = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();
    reopened.db.with(|conn| reopened.blobs.gc(conn)).unwrap();
    assert!(
        reopened
            .blobs
            .path_for(&first.blob_hash, &first.ext)
            .is_file()
    );
    let current = get(&reopened);
    h.ws = WebState::new(reopened).unwrap();
    let version = media_merges::gallery_version(&current.media);
    let undo = format!(
        "csrf={}&action=undo_compression&gallery_version={version}&media_id={}",
        h.ws.csrf, current.media[0].id
    );
    assert_eq!(
        h.send(h.post(&action_url, &undo)).await.status(),
        StatusCode::SEE_OTHER
    );
    let restored = get(&h.state);
    assert_eq!(restored.media[0].blob_hash, first.blob_hash);
    let version = media_merges::gallery_version(&restored.media);
    let merge_form = format!(
        "csrf={}&action=merge&output=fit&quality=90&gallery_version={version}&sel_{}=on&sel_{}=on",
        h.ws.csrf, restored.media[0].id, restored.media[1].id
    );
    assert_eq!(
        h.send(h.post(&action_url, &merge_form)).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    let merged = get(&h.state);
    assert_eq!(merged.media.len(), 1);
    assert_eq!(
        (
            merged.media[0].ext.as_str(),
            merged.media[0].width,
            merged.media[0].height
        ),
        ("jpg", Some(440), Some(22))
    );
    assert!(
        body_text(h.send(h.get(&post_url)).await)
            .await
            .contains("Undo merge")
    );
}
