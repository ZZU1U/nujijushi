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
    harness_at("")
}

fn harness_at(base_path: &str) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), config()).unwrap();
    let mut cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    cfg.web_base_path = base_path.into();
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
        Request::get(api_path(path))
            .header(
                header::COOKIE,
                format!("{}={}", auth::COOKIE, self.ws.session),
            )
            .body(Body::empty())
            .unwrap()
    }

    fn post(&self, path: &str, body: &str) -> Request<Body> {
        Request::post(api_path(path))
            .header(
                header::COOKIE,
                format!("{}={}", auth::COOKIE, self.ws.session),
            )
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn bearer(&self, path: &str, credential: &str, form: Option<&str>) -> Request<Body> {
        Request::builder()
            .method(if form.is_some() { "POST" } else { "GET" })
            .uri(format!("{}{path}", self.state.cfg.web_base_path))
            .header(header::AUTHORIZATION, format!("Bearer {credential}"))
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.unwrap_or_default().to_owned()))
            .unwrap()
    }

    async fn send(&self, request: Request<Body>) -> axum::response::Response {
        let response = routes::router(self.ws.clone())
            .oneshot(request)
            .await
            .unwrap();
        let (mut parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            parts.extensions.insert(value);
        }
        axum::response::Response::from_parts(parts, Body::from(bytes))
    }

    async fn page(&self, path: &str) -> serde_json::Value {
        let response = self.send(self.get(path)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        Self::json(&response).clone()
    }

    fn json(response: &axum::response::Response) -> &serde_json::Value {
        response.extensions().get().expect("JSON response")
    }

    fn message(response: &axum::response::Response) -> &str {
        let body = Self::json(response);
        body["message"]
            .as_str()
            .or_else(|| body["error"].as_str())
            .unwrap_or("")
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

fn api_path(path: &str) -> String {
    match path {
        "/" => "/api/v1/dashboard".into(),
        uri if uri.starts_with("/api/") => uri.into(),
        uri => format!("/api/v1{uri}"),
    }
}

fn row(value: &serde_json::Value, id: i64) -> &serde_json::Value {
    value
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
}

fn ids(value: &serde_json::Value) -> Vec<i64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_i64().unwrap())
        .collect()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test]
async fn health_reports_the_running_build_without_a_session() {
    for prefix in ["", "/archive"] {
        let h = harness_at(prefix);
        let response = h
            .send(
                Request::get(format!("{prefix}/healthz"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            Harness::json(&response),
            &serde_json::json!({
                "status": "ok",
                "version": nujijushi::VERSION,
                "revision": option_env!("NUJIJUSHI_REVISION"),
                "dirty": option_env!("NUJIJUSHI_DIRTY").map(|value| value == "true"),
            })
        );
    }
}

#[tokio::test]
async fn bearer_auth_reuses_an_active_import_and_preserves_the_rednote_url() {
    const URL: &str = "https://www.rednote.com/discovery/item/674051740000000007027a15?xsec_token=sample%2Bvalue%3D&xsec_source=pc_feed";
    for prefix in ["", "/archive"] {
        let h = harness_at(prefix);
        let path = "/api/v1/session";
        assert_eq!(
            h.send(h.bearer(path, "wrong-token", None)).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let mut invalid = h.bearer(path, "wrong-token", None);
        invalid.headers_mut().insert(
            header::COOKIE,
            format!("{}={}", auth::COOKIE, h.ws.session)
                .parse()
                .unwrap(),
        );
        assert_eq!(h.send(invalid).await.status(), StatusCode::UNAUTHORIZED);
        let with_proxy_auth = Request::get(format!("{prefix}{path}"))
            .header(header::AUTHORIZATION, "Basic fixture")
            .header(header::COOKIE, format!("{}={}", auth::COOKIE, h.ws.session))
            .body(Body::empty())
            .unwrap();
        assert_eq!(h.send(with_proxy_auth).await.status(), StatusCode::OK);
        let session = h.send(h.bearer(path, UI_TOKEN, None)).await;
        assert_eq!(session.status(), StatusCode::OK);
        assert_eq!(Harness::json(&session)["csrf"], h.ws.csrf);
        assert!(session.headers().get(header::SET_COOKIE).is_none());
        assert_eq!(
            h.send(h.bearer("/api/status", UI_TOKEN, None))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            h.send(h.bearer("/media/abc", UI_TOKEN, None))
                .await
                .status(),
            StatusCode::SEE_OTHER
        );

        let form = |csrf: &str| {
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("csrf", csrf)
                .append_pair("targets", URL)
                .append_pair("reuse_active", "true")
                .finish()
        };
        let path = "/api/v1/import";
        let blocked = h.send(h.bearer(path, UI_TOKEN, Some(&form("wrong")))).await;
        assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);
        assert!(h.jobs().await.is_empty());
        let guest_job = nujijushi::jobs::create_job_for(&h.state, URL, Some(42))
            .await
            .unwrap();
        let form = form(&h.ws.csrf);
        let (first, retry) = tokio::join!(
            h.send(h.bearer(path, UI_TOKEN, Some(&form))),
            h.send(h.bearer(path, UI_TOKEN, Some(&form)))
        );
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        assert_eq!(retry.status(), StatusCode::ACCEPTED);
        assert_eq!(
            Harness::json(&first)["job_ids"],
            Harness::json(&retry)["job_ids"]
        );
        let jobs = h.jobs().await;
        assert_eq!(jobs.len(), 2);
        let job_id = Harness::json(&first)["job_ids"][0].as_i64().unwrap();
        assert_ne!(job_id, guest_job);
        let job = jobs.iter().find(|job| job.id == job_id).unwrap();
        assert_eq!(job.payload.as_deref(), Some(URL));
        assert_eq!(job.submitted_by, None);
    }
}

#[tokio::test]
async fn extension_watch_resolution_and_creation_use_the_admin_watch_rules() {
    let h = harness_at("/archive");
    let profile = "https://twitter.com/Example";
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", &h.ws.csrf)
        .append_pair("url", profile)
        .finish();
    let result = h
        .send(h.bearer("/api/v1/watches/resolve", UI_TOKEN, Some(&form)))
        .await;
    assert_eq!(result.status(), StatusCode::OK);
    let resolved = Harness::json(&result);
    let url = resolved["url"].as_str().unwrap();
    assert_eq!(url, "https://x.com/example/media");
    for name in ["Example", "Second name"] {
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", &h.ws.csrf)
            .append_pair("name", name)
            .append_pair("url", url)
            .finish();
        let created = h
            .send(h.bearer("/api/v1/watches", UI_TOKEN, Some(&form)))
            .await;
        assert_eq!(created.status(), StatusCode::OK);
    }
    let watches = h
        .state
        .db
        .with(|conn| nujijushi::db::watches::list(conn))
        .unwrap();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].name, "Example");
    assert_eq!(watches[0].initial_batch, 10);
    assert!(h.jobs().await.is_empty());
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let jobs = h.jobs().await;
    let targets: Vec<_> = jobs
        .iter()
        .rev()
        .map(|job| job.payload.as_deref().unwrap())
        .collect();
    assert_eq!(targets, [first, second]);
}

#[tokio::test]
async fn rednote_import_job_views_keep_access_tokens_private_on_retry() {
    const URL: &str = "https://www.rednote.com/discovery/item/674051740000000007027a15?xsec_token=private-fixture&xsec_source=app";
    const DISPLAY: &str = "https://www.rednote.com/explore/674051740000000007027a15";
    let h = harness();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", &h.ws.csrf)
        .append_pair("targets", URL)
        .finish();
    let response = h.send(h.post("/import", &form)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let id = Harness::json(&response)["job_ids"][0].as_i64().unwrap();
    let view = h.page(&format!("/jobs/{id}/status")).await;
    assert_eq!(view["payload"], DISPLAY);
    assert!(!view.to_string().contains("private-fixture"));
    h.state
        .db
        .call(move |conn| job_db::set_state(conn, id, "failed", Some("fixture failure")))
        .await
        .unwrap();
    let response = h
        .send(h.post("/jobs/retry", &format!("csrf={}&id={id}", h.ws.csrf)))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let new_id = Harness::json(&response)["job_ids"][0].as_i64().unwrap();
    assert_eq!(
        h.page(&format!("/jobs/{new_id}/status")).await["payload"],
        DISPLAY
    );
    let jobs = h.jobs().await;
    let retried = jobs.iter().find(|job| job.id == new_id).unwrap();
    assert_eq!(retried.payload.as_deref(), Some(URL));
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
        StatusCode::ACCEPTED
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
#[tokio::test]
async fn post_details_can_be_edited_and_cleared_across_source_refreshes() {
    use nujijushi::db::posts;

    let h = harness();
    h.add_channel().await;
    let post_id = h.post_only("42").await;
    let path = format!("/posts/{post_id}");
    let page = h.page(&path).await;
    assert_eq!(page["post"]["title"], "Artwork 42");
    assert!(page["post"]["description"].is_null());

    let denied = h
        .send(h.post(&path, "csrf=bad&title=Blocked&description=Blocked"))
        .await;
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    let post = || h.state.db.with(|conn| posts::get(conn, post_id)).unwrap();
    assert_eq!(post().title.as_deref(), Some("Artwork 42"));
    assert!(post().description.is_none());

    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", &h.ws.csrf)
        .append_pair("title", "Artwork 42")
        .append_pair(
            "description",
            "A curated description & notes.\r\n\r\n</textarea><script>alert(1)</script>",
        )
        .finish();
    assert_eq!(h.send(h.post(&path, &form)).await.status(), StatusCode::OK);
    let description = "A curated description & notes.\n\n</textarea><script>alert(1)</script>";
    assert_eq!(post().description.as_deref(), Some(description));
    let page = h.page(&path).await;
    assert_eq!(page["post"]["description"], description);
    let caption = page["captions"][0]["caption"].as_str().unwrap();
    assert!(caption.contains("A curated description"));
    assert!(!caption.contains("<script>"));

    let refresh = |title: &str| {
        let mut source = NormalizedPost::new("pixiv", "42");
        source.title = Some(title.into());
        source.description = Some("A fresh description from the source".into());
        h.state
            .db
            .with(|conn| posts::upsert(conn, &source, &[]))
            .unwrap()
    };
    assert_eq!(refresh("Refreshed source title"), (post_id, false));
    let refreshed = post();
    assert_eq!(refreshed.title.as_deref(), Some("Refreshed source title"));
    assert_eq!(refreshed.description.as_deref(), Some(description));

    let edit_title = format!("csrf={}&title=Curated+title", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&path, &edit_title)).await.status(),
        StatusCode::OK
    );
    assert_eq!(post().description.as_deref(), Some(description));
    let clear_description = format!("csrf={}&description=+", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&path, &clear_description)).await.status(),
        StatusCode::OK
    );
    assert_eq!(refresh("Another source title"), (post_id, false));
    let edited = post();
    assert_eq!(edited.title.as_deref(), Some("Curated title"));
    assert!(edited.description.is_none());
    assert_eq!(edited.state, refreshed.state);
    assert_eq!(edited.url, refreshed.url);
    assert_eq!(edited.media, refreshed.media);
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
    let page = h.page(&path).await;
    let tag = row(&page["post"]["tags"], tag_id);
    assert_eq!(tag["source"], true);
    assert_eq!(tag["manual"], true);
    assert_eq!(page["post"]["manual_tags"], "original, favorite");

    let edit = format!("csrf={}&notes=reviewed", h.ws.csrf);
    assert_eq!(h.send(h.post(&path, &edit)).await.status(), StatusCode::OK);
    let invalid = h
        .send(h.post(&action, &format!("csrf=bad&tag_id={tag_id}&action=remove")))
        .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        row(&h.page(&path).await["post"]["tags"], tag_id)["source"],
        true
    );

    let remove = format!("csrf={}&tag_id={tag_id}&action=remove", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&action, &remove)).await.status(),
        StatusCode::OK
    );
    let removed = h.page(&path).await;
    assert_eq!(
        row(&removed["post"]["tag_removals"], tag_id)["name"],
        "original"
    );
    assert_eq!(row(&removed["post"]["tags"], tag_id)["source"], false);
    assert_eq!(removed["post"]["manual_tags"], "original, favorite");

    let undo = format!("csrf={}&tag_id={tag_id}&action=undo", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&action, &undo)).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        row(&h.page(&path).await["post"]["tags"], tag_id)["source"],
        true
    );
    assert!(
        h.page(&path).await["post"]["tag_removals"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn post_tag_edits_are_targeted_and_remove_both_origins() {
    use nujijushi::db::tags;

    let h = harness();
    let post = h.post_only("targeted-tags").await;
    let original = h
        .state
        .db
        .with(|conn| {
            tags::set_alias(conn, "hero", "main_character")?;
            tags::add_manual(conn, post, &["original".into(), "favorite".into()])?;
            Ok(tags::provenance(conn, post)?[0].id)
        })
        .unwrap();
    let path = format!("/posts/{post}");
    let endpoint = format!("{path}/tags");
    let csrf = &h.ws.csrf;
    let add = format!("csrf={csrf}&action=add&tags=%23Hero%2C+Night+Sky%0Ahero");
    assert_eq!(
        h.send(h.post(&endpoint, &add)).await.status(),
        StatusCode::OK
    );
    let page = h.page(&path).await;
    assert_eq!(
        page["post"]["manual_tags"],
        "original, favorite, main_character, night_sky"
    );
    let invalid = format!("csrf={csrf}&action=add&tags=keep_out%2C{}", "a".repeat(49));
    assert_eq!(
        h.send(h.post(&endpoint, &invalid)).await.status(),
        StatusCode::BAD_REQUEST
    );
    let remove = format!("csrf={csrf}&action=remove_tag&tag_id={original}");
    assert_eq!(
        h.send(h.post(&endpoint, &remove)).await.status(),
        StatusCode::OK
    );
    let removed = h.page(&path).await;
    assert_eq!(removed["post"]["tag_removals"][0]["id"], original);
    assert_eq!(
        removed["post"]["manual_tags"],
        "favorite, main_character, night_sky"
    );
    assert_eq!(removed["post"]["tags"].as_array().unwrap().len(), 3);
    let undo = format!("csrf={csrf}&action=undo&tag_id={original}");
    assert_eq!(
        h.send(h.post(&endpoint, &undo)).await.status(),
        StatusCode::OK
    );
    let restored = h.page(&path).await;
    let tag = row(&restored["post"]["tags"], original);
    assert_eq!(tag["source"], true);
    assert_eq!(tag["manual"], false);
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(h.mock.send_count(), 0, "no upload inside the request");
    assert!(
        Harness::message(&response).contains("queued as job #"),
        "flash: {}",
        Harness::message(&response)
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
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let second = h.send(h.post("/publish", &body)).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert!(
        Harness::message(&second).contains("already queued or running"),
        "flash: {}",
        Harness::message(&second)
    );

    let jobs = h.jobs().await;
    assert_eq!(jobs.len(), 1, "one job per channel while it is pending");

    // and the send still happens only once
    h.run_worker_once().await;
    assert_eq!(h.mock.send_count(), 1);
}

#[tokio::test]
async fn channel_flush_is_a_csrf_checked_deduplicated_background_job() {
    let h = harness();
    let channel = h.add_channel().await;
    h.queue_a_post("1", &channel).await;
    h.queue_a_post("2", &channel).await;
    let path = format!("/channels/{}/flush", channel.id);
    let queue_path = format!("/channels/{}/queue/status", channel.id);
    assert_eq!(
        h.send(h.post(&path, "csrf=invalid")).await.status(),
        StatusCode::BAD_REQUEST
    );
    let body = format!("csrf={}", h.ws.csrf);
    let response = h.send(h.post(&path, &body)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let id = Harness::json(&response)["job_ids"][0].as_i64().unwrap();
    assert_eq!(h.send(h.post(&path, &body)).await.status(), StatusCode::OK);
    assert_eq!(h.jobs().await.len(), 1);
    assert_eq!(h.mock.send_count(), 0);
    let queued = h.page(&queue_path).await;
    assert_eq!(queued["queued"], 2);
    assert_eq!(queued["jobs"][0]["kind"], "flush_channel");
    assert_eq!(queued["jobs"][0]["id"], id);

    let worker = nujijushi::jobs::worker::JobWorker::new(h.state.clone());
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(h.mock.send_count(), 2);
    let finished = h.page(&queue_path).await;
    assert_eq!(finished["queued"], 0);
    assert_eq!(finished["jobs"][0]["state"], "done");
    assert_eq!(finished["jobs"][0]["progress"], 2);
    assert_eq!(finished["jobs"][0]["total"], 2);
    assert_eq!(finished["jobs"][0]["message"], "2 published · 0 skipped");
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);

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
async fn post_detail_links_to_confirmed_telegram_deliveries_after_requeue() {
    use nujijushi::db::{channels, queue};
    use nujijushi::tg::publish::{PublishOutcome, publish_item};

    let h = harness();
    let mut public = h.add_channel().await;
    let private = Channel::new("Private channel", "-1001234567890");
    let private_id = h
        .state
        .db
        .with(|conn| channels::insert(conn, &private))
        .unwrap();
    let (post_id, public_queue) = h.queue_a_post("telegram-links", &public).await;
    let private_queue = h
        .state
        .db
        .with(|conn| queue::enqueue_new(conn, post_id, private_id, 0))
        .unwrap()
        .unwrap();
    let path = format!("/posts/{post_id}");
    assert_eq!(h.page(&path).await["deliveries"], serde_json::json!([]));

    let PublishOutcome::Published {
        message_ids: public_ids,
        ..
    } = publish_item(&h.state, public_queue, false).await.unwrap()
    else {
        panic!("public delivery failed");
    };
    let PublishOutcome::Published {
        message_ids: private_ids,
        ..
    } = publish_item(&h.state, private_queue, false).await.unwrap()
    else {
        panic!("private delivery failed");
    };
    let expected = serde_json::json!([
        {"channel_id": public.id, "channel": "Test", "url": format!("https://t.me/test_channel/{}", public_ids[0])},
        {"channel_id": private_id, "channel": "Private channel", "url": format!("https://t.me/c/1234567890/{}", private_ids[0])}
    ]);
    assert_eq!(h.page(&path).await["deliveries"], expected);

    public.tg_id = "@moved_channel".into();
    h.state
        .db
        .with(|conn| {
            channels::update(conn, &public)?;
            queue::requeue(conn, public_queue)
        })
        .unwrap();
    assert_eq!(h.page(&path).await["deliveries"], expected);
}

#[tokio::test]
async fn editor_state_can_move_between_verdicts_and_archive_but_cannot_reset_to_new() {
    let h = harness();
    let post = h.post_only("post-state").await;
    let path = format!("/posts/{post}");
    let detail = h.page(&path).await;
    assert_eq!(detail["post"]["state"], "new");
    assert_eq!(
        detail["states"],
        serde_json::json!(["approved", "rejected", "archived"])
    );
    for state in ["approved", "archived", "rejected", "approved"] {
        assert_eq!(
            h.send(h.post(&path, &format!("csrf={}&state={state}", h.ws.csrf)))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(h.page(&path).await["post"]["state"], state);
    }
    let reset = format!("csrf={}&state=new&title=Changed", h.ws.csrf);
    assert_eq!(
        h.send(h.post(&path, &reset)).await.status(),
        StatusCode::BAD_REQUEST
    );
    let detail = h.page(&path).await;
    assert_eq!(detail["post"]["state"], "approved");
    assert_eq!(detail["post"]["title"], "Artwork post-state");
}

#[tokio::test]
async fn remove_all_tags_clears_both_origins_and_source_tags_can_be_restored() {
    use nujijushi::db::{posts, tags};

    let h = harness();
    let post = h.post_only("untag").await;
    let other = h.post_only("untag-other").await;
    h.state
        .db
        .with(|conn| tags::add_manual(conn, post, &["original".into(), "favorite".into()]))
        .unwrap();
    let path = format!("/posts/{post}/tags");
    assert_eq!(
        h.send(h.post(&path, "csrf=bad&action=remove_all"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let form = format!("csrf={}&action=remove_all", h.ws.csrf);
    assert_eq!(h.send(h.post(&path, &form)).await.status(), StatusCode::OK);
    assert_eq!(h.send(h.post(&path, &form)).await.status(), StatusCode::OK);

    let source_tag = h
        .state
        .db
        .with(|conn| {
            assert!(tags::provenance(conn, post)?.is_empty());
            assert_eq!(tags::for_post(conn, other)?, ["original"]);
            let mut updated = NormalizedPost::new("pixiv", "untag");
            updated.tags = vec!["original".into(), "newly_imported".into()];
            posts::upsert(conn, &updated, &[])?;
            assert_eq!(tags::for_post(conn, post)?, ["newly_imported"]);
            let removed = tags::removals(conn, post)?;
            assert_eq!(removed.len(), 1);
            assert_eq!(removed[0].name, "original");
            Ok(removed[0].id)
        })
        .unwrap();
    let undo = format!("csrf={}&action=undo&tag_id={source_tag}", h.ws.csrf);
    assert_eq!(h.send(h.post(&path, &undo)).await.status(), StatusCode::OK);
    h.state
        .db
        .with(|conn| {
            let restored = tags::provenance(conn, post)?
                .into_iter()
                .find(|tag| tag.id == source_tag)
                .unwrap();
            assert!(restored.source);
            assert!(!restored.manual);
            Ok(())
        })
        .unwrap();
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
    assert_eq!(h.page(&path).await["guest_review"], true);
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
    assert_eq!(h.send(h.post(&path, &form)).await.status(), StatusCode::OK);
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
            StatusCode::OK
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);

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

    let page = h.page("/jobs").await;
    let jobs = &page["jobs"];
    assert_eq!(row(jobs, watch)["kind_label"], "watch check");
    assert_eq!(row(jobs, running)["kind_label"], "publish next");
    let tracked = row(jobs, folder);
    assert!(
        tracked["payload"]
            .as_str()
            .unwrap()
            .contains(&"folder".repeat(300))
    );
    assert_eq!(tracked["track"], true);
    assert_eq!(tracked["progress"], 3);
    assert_eq!(tracked["total"], 8);
    for id in [failed, running] {
        assert_eq!(row(jobs, id)["track"], false);
    }
    assert_eq!(row(jobs, failed)["retryable"], true);
    assert_eq!(page["parallel"], h.state.cfg.max_concurrent_imports);
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
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(
        Harness::message(&response).contains("re-queued"),
        "flash: {}",
        Harness::message(&response)
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
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        Harness::message(&response).contains("only imports can be retried"),
        "flash: {}",
        Harness::message(&response)
    );
    assert_eq!(h.jobs().await.len(), 1, "nothing new was queued");
}

#[tokio::test]
async fn initial_views_and_status_share_job_data() {
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
    let dashboard = h.page("/").await;
    let import = h.page("/import").await;
    let job = row(&dashboard["jobs"], watch);
    assert_eq!(job["kind_label"], "watch check");
    let polled = h.page(&format!("/jobs/{watch}/status")).await;
    for field in ["id", "kind", "state", "progress", "total", "retryable"] {
        assert_eq!(job[field], row(&import["jobs"], watch)[field]);
        assert_eq!(job[field], polled[field]);
    }
    assert_eq!(row(&dashboard["channels"], channel.id)["queued"], 0);
    assert!(
        import["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["progress"] == 1 && job["total"] == 4)
    );
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
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        Harness::message(&response).contains("only failed jobs can be retried"),
        "flash: {}",
        Harness::message(&response)
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

    assert_eq!(ids(&h.page("/posts").await["cards"]), vec![waiting]);
    let mut all = ids(&h.page("/posts?sent=all").await["cards"]);
    all.sort();
    assert_eq!(all, vec![sent_post, waiting]);
    assert_eq!(
        ids(&h.page("/posts?sent=yes").await["cards"]),
        vec![sent_post]
    );
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
    let link = |data: &serde_json::Value, key: &str, page: &str| {
        let href = data[key].as_str().unwrap();
        let parsed: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(href.strip_prefix("/posts?").unwrap().as_bytes()).collect();
        assert_eq!(parsed.len(), filters.len() + 2);
        assert_eq!(parsed["per_page"], "24");
        for (key, value) in filters {
            assert_eq!(parsed[key], value);
        }
        assert_eq!(parsed["page"], page);
        href.to_string()
    };

    let first = h.page(&path).await;
    assert_eq!(first["total"], 25);
    assert_eq!(ids(&first["cards"]).len(), 24);
    let second = h.page(&link(&first, "next_page", "2")).await;
    assert_eq!(second["total"], 25);
    assert_eq!(ids(&second["cards"]), vec![matching[0]]);
    assert!(!ids(&second["cards"]).contains(&nonmatching));
    assert!(second["next_page"].is_null());
    let back = h.page(&link(&second, "previous_page", "1")).await;
    assert_eq!(back["total"], 25);
    assert_eq!(ids(&back["cards"]), ids(&first["cards"]));
    assert!(back["previous_page"].is_null());
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
    let page = h.page("/posts").await;
    assert_eq!(ids(&page["cards"]), vec![kept]);
    assert_eq!(page["total"], 1);
    let all = h.page("/posts?state=all").await;
    assert!(ids(&all["cards"]).contains(&rejected));
    assert_eq!(all["total"], 2);
    let only = h.page("/posts?state=rejected").await;
    assert_eq!(ids(&only["cards"]), vec![rejected]);
    assert_eq!(only["total"], 1);
    assert_eq!(only["filters"]["state"], "rejected");
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
    let page = h.page(&format!("/posts/{post_id}")).await;
    assert_eq!(page["media_total"], total);
    assert_eq!(page["media"].as_array().unwrap().len(), sizes.len());
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

    let page = h.page("/posts").await;
    assert_eq!(row(&page["cards"], guest_post)["submitter"], "@fan");
    assert_eq!(row(&page["cards"], guest_post)["submitter_guest"], true);
    assert_eq!(row(&page["cards"], admin_post)["submitter_guest"], false);
    let detail = h.page(&format!("/posts/{guest_post}")).await;
    assert_eq!(detail["submitter"]["label"], "@fan");
    assert_eq!(detail["submitter"]["admin"], false);
}

/// A queue that is not moving is explained: no heartbeat means the scheduler is
/// not running (`serve` never schedules).
#[tokio::test]
async fn the_dashboard_says_when_the_scheduler_is_not_running() {
    let h = harness();
    let page = h.page("/").await;
    assert_eq!(page["scheduler"]["stale"], true);
    let steps = page["setup"]["steps"].as_array().unwrap();
    for title in ["Add a channel", "Import artwork", "Queue a post"] {
        assert!(steps.iter().any(|step| step["title"] == title));
    }

    h.state
        .db
        .set_setting(
            nujijushi::queue::scheduler::TICK_KEY,
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();
    assert_eq!(h.page("/").await["scheduler"]["stale"], false);

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

    let queue = h.page(&format!("/channels/{}/queue", channel.id)).await;
    let items = queue["items"].as_array().unwrap();
    assert!(items.iter().any(|item| item["eta_in"] == "30m"));
    let detail = h.page(&format!("/posts/{post_id}")).await;
    let item = items
        .iter()
        .find(|item| item["post_id"] == post_id)
        .unwrap();
    assert_eq!(detail["captions"][0]["eta_in"], item["eta_in"]);
}

#[cfg(unix)]
#[tokio::test]
async fn fanbox_watch_imports_creator_media_with_session_and_retries_restricted_downloads() {
    use nujijushi::db::{posts, watches};
    use nujijushi::jobs::worker::JobWorker;
    use std::os::unix::fs::PermissionsExt;

    let mut h = harness();
    let base = h._dir.path();
    let script = base.join("fake-fanbox-gallery-dl");
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
    printf '%s\n' "$@" > "$fixture_dir/fanbox-scan-args"
    cat "$fixture_dir/fanbox-list.json"
else
    printf '%s\n' "$@" > "$fixture_dir/fanbox-download-args"
    case "$target" in https://paper-bird.fanbox.cc/posts/*) ;; *) exit 1 ;; esac
    id=$(basename "$target")
    printf '%s\n' "$id" >> "$fixture_dir/fanbox-downloads"
    restricted=false
    if [ -f "$fixture_dir/restrict-$id" ]; then restricted=true; fi
    cp "$fixture_dir/source.png" "$staging/image.png"
    printf '{"category":"fanbox","creatorId":"paper-bird","id":"%s","publishedDatetime":"2025-09-02T14:11:03+09:00","isRestricted":%s,"isCoverImage":%s,"hasAdultContent":true,"user":{"name":"Paper Bird","userId":"720"},"tags":["source-tag"]}' "$id" "$restricted" "$restricted" > "$staging/image.png.json"
fi
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    image::RgbImage::from_pixel(16, 16, image::Rgb([80, 30, 10]))
        .save(base.join("source.png"))
        .unwrap();
    let cookie = h.state.cfg.cookies_dir().join("fanbox.txt");
    std::fs::write(&cookie, "# mock FANBOX session\n").unwrap();
    let cfg = Arc::make_mut(&mut h.state.cfg);
    cfg.gallery_dl = script.to_str().unwrap().into();
    cfg.proxy_url = Some("http://127.0.0.1:9443".into());
    h.ws = WebState::new(h.state.clone()).unwrap();

    let save = |url: &str| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", &h.ws.csrf)
            .append_pair("name", "Paper Bird")
            .append_pair("url", url)
            .append_pair("initial_batch", "5")
            .finish()
    };
    let response = h
        .send(h.post("/watches", &save("https://www.fanbox.cc/@paper-bird")))
        .await;
    assert_eq!(Harness::message(&response), "Watch added");
    let alias = h
        .send(h.post("/watches", &save("https://paper-bird.fanbox.cc/")))
        .await;
    assert_eq!(Harness::message(&alias), "This feed is already watched");
    let page = h.page("/watches").await;
    assert_eq!(
        page["watches"][0]["url"],
        "https://paper-bird.fanbox.cc/posts"
    );
    let watch_id = page["watches"][0]["id"].as_i64().unwrap();
    let check_path = format!("/watches/{watch_id}/check");
    let csrf = format!("csrf={}", h.ws.csrf);
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/watched_gallery_listings.json")).unwrap();
    let list_path = base.join("fanbox-list.json");
    std::fs::write(&list_path, serde_json::to_vec(&fixture["fanbox"]).unwrap()).unwrap();
    let restricted = base.join("restrict-501");
    std::fs::write(&restricted, "").unwrap();

    let worker = JobWorker::new(h.state.clone());
    let first = h.send(h.post(&check_path, &csrf)).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_eq!(worker.drain().await.unwrap(), 1);
    let scan_args = std::fs::read_to_string(base.join("fanbox-scan-args")).unwrap();
    let download_args = std::fs::read_to_string(base.join("fanbox-download-args")).unwrap();
    for args in [&scan_args, &download_args] {
        assert!(args.contains(&format!("--cookies\n{}\n", cookie.display())));
        assert!(args.contains("--proxy\nhttp://127.0.0.1:9443\n"));
        assert!(args.contains("extractor.fanbox.embeds=false\n"));
    }
    assert!(scan_args.contains("--post-range\n1-5\n"));
    assert_eq!(page["watches"].as_array().unwrap().len(), 1);
    let failed = h.page("/watches").await;
    assert_eq!(failed["watches"][0]["job"]["state"], "failed");
    assert!(
        failed["watches"][0]["job"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot access this post")
    );
    h.state
        .db
        .with(|conn| {
            assert_eq!(watches::pending_count(conn, watch_id)?, 1);
            assert!(posts::find_by_source_id(conn, "fanbox", "501")?.is_none());
            assert!(posts::find_by_source_id(conn, "fanbox", "504")?.is_none());
            Ok(())
        })
        .unwrap();

    std::fs::remove_file(restricted).unwrap();
    std::fs::write(&list_path, "[]").unwrap();
    assert_eq!(
        h.send(h.post(&check_path, &csrf)).await.status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    let scan_args = std::fs::read_to_string(base.join("fanbox-scan-args")).unwrap();
    assert!(
        scan_args.contains("--date-after\n1756789862\n"),
        "{scan_args}"
    );
    assert_eq!(
        std::fs::read_to_string(base.join("fanbox-downloads")).unwrap(),
        "501\n501\n"
    );
    h.state
        .db
        .with(|conn| {
            let id = posts::find_by_source_id(conn, "fanbox", "501")?.unwrap();
            let post = posts::get(conn, id)?;
            assert_eq!(post.state, PostState::New);
            assert_eq!(
                post.url.as_deref(),
                Some("https://paper-bird.fanbox.cc/posts/501")
            );
            assert_eq!(post.rating, Rating::Explicit);
            assert!(post.media[0].sensitive);
            assert_eq!(
                post.posted_at.unwrap().to_rfc3339(),
                "2025-09-02T05:11:03+00:00"
            );
            let author = post.author.unwrap();
            assert_eq!(author.handle.as_deref(), Some("paper-bird"));
            assert_eq!(author.name.as_deref(), Some("Paper Bird"));
            assert_eq!(author.url.as_deref(), Some("https://paper-bird.fanbox.cc"));
            assert_eq!(watches::pending_count(conn, watch_id)?, 0);
            Ok(())
        })
        .unwrap();
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
        StatusCode::OK
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

    assert_eq!(h.send(check(&h)).await.status(), StatusCode::ACCEPTED);
    let second = h.send(check(&h)).await;
    assert_eq!(Harness::message(&second), "A check is already active");
    assert!(!base.join("listing-args").exists());
    let watches = h.page("/watches").await;
    assert_eq!(watches["watches"][0]["name"], "Pixiv Art");
    assert_eq!(watches["watches"][0]["job"]["active"], true);
    assert!(watches["watches"][0]["last_run_at"].is_null());
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
    let first_check = &h.page("/watches").await["watches"][0];
    assert!(
        first_check["job"]["message"]
            .as_str()
            .unwrap()
            .contains("1 added · 0 skipped (already archived) · 1 failed · 1 pending")
    );
    assert!(first_check["last_run_at"].as_str().is_some());

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
    assert_eq!(Harness::message(&pause), "Watch paused");
    assert_eq!(h.send(check(&h)).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(h.page("/watches").await["watches"][0]["enabled"], false);

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
    assert_eq!(Harness::message(&resume), "Watch resumed");
    let worker = JobWorker::new(reopened);
    assert_eq!(h.send(check(&h)).await.status(), StatusCode::ACCEPTED);
    assert_eq!(
        h.page("/watches").await["watches"][0]["last_run_at"],
        first_check["last_run_at"]
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_ne!(
        h.page("/watches").await["watches"][0]["last_run_at"],
        first_check["last_run_at"]
    );
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
        StatusCode::OK
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

fn reorder_form(csrf: &str, version: &str, order: &[i64]) -> String {
    let ids = order
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("csrf={csrf}&action=reorder&gallery_version={version}&media_ids={ids}")
}

#[tokio::test]
async fn gallery_reorder_keeps_duplicate_identity_and_drives_publishing_and_exports() {
    use nujijushi::db::{media_merges as gallery, posts, queue};
    use nujijushi::media::compress::Compression;
    use nujijushi::tg::publish::{PublishOutcome, publish_item};

    let h = harness();
    let channel = h.add_channel().await;
    let [a, b, c, new] = [30, 60, 90, 120].map(|shade| {
        encode_image(
            image::RgbImage::from_pixel(8, 8, image::Rgb([shade, 0, 0])),
            image::ImageFormat::Png,
        )
    });
    let id = archive_test_images(
        &h.state,
        "local",
        "ordered",
        &[(&a, "png"), (&b, "png"), (&a, "png"), (&c, "png")],
    );
    let load = || h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let version = || {
        h.state
            .db
            .with(|conn| gallery::current_revision(conn, id))
            .unwrap()
    };
    let original = load();
    let [a_id, b_id, second_a, c_id] = std::array::from_fn(|index| original.media[index].id);
    let initial_version = version();
    h.state
        .db
        .with(|conn| gallery::set_hidden(conn, id, second_a, &initial_version, true))
        .unwrap();
    let revision = version();
    let order = [second_a, c_id, b_id, a_id];
    let endpoint = format!("/posts/{id}/media");
    let csrf = &h.ws.csrf;
    let form = reorder_form(csrf, &revision, &order);
    for rejected in [
        reorder_form("bad", &revision, &order),
        reorder_form(csrf, &revision, &[a_id, a_id, b_id, c_id]),
        reorder_form(csrf, &revision, &[a_id, b_id, c_id]),
        reorder_form(csrf, &revision, &[a_id, b_id, c_id, -1]),
        format!("csrf={csrf}&action=reorder&gallery_version={revision}&media_ids=artwork"),
    ] {
        assert_eq!(
            h.send(h.post(&endpoint, &rejected)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let job = h
        .state
        .db
        .with(|conn| {
            Ok(gallery::enqueue_compression(
                conn,
                id,
                None,
                &revision,
                Compression::new(85, false)?,
            )?
            .unwrap())
        })
        .unwrap();
    let active = h.send(h.post(&endpoint, &form)).await;
    assert_eq!(active.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(active).await.contains("gallery edit to finish"));
    let queue_id = h
        .state
        .db
        .with(|conn| {
            job_db::set_state(conn, job, "failed", None)?;
            Ok(queue::enqueue_new(conn, id, channel.id, 0)?.unwrap())
        })
        .unwrap();
    assert!(
        nujijushi::queue::lease::acquire_for_gallery(
            &h.state,
            queue_id,
            channel.tg_id.clone(),
            id,
            revision.clone()
        )
        .await
        .unwrap()
        .is_some()
    );
    let sending = h.send(h.post(&endpoint, &form)).await;
    assert_eq!(sending.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(sending).await.contains("current send"));
    h.state
        .db
        .with(|conn| queue::release(conn, queue_id))
        .unwrap();
    assert_eq!(version(), revision);

    assert_eq!(
        h.send(h.post(&endpoint, &form)).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        load()
            .media
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        order
    );
    assert_eq!(
        h.page(&format!("/posts/{id}")).await["media"][0]["hidden"],
        true
    );
    assert_eq!(
        h.send(h.post(&endpoint, &form)).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert!(
        nujijushi::queue::lease::acquire_for_gallery(
            &h.state,
            queue_id,
            channel.tg_id.clone(),
            id,
            revision
        )
        .await
        .unwrap()
        .is_none()
    );

    let export = tempfile::tempdir().unwrap();
    let share = export.path().join("share");
    let mirror = export.path().join("mirror");
    let mut mirrored = h.state.clone();
    Arc::make_mut(&mut mirrored.cfg).storage.local = Some(nujijushi::config::LocalSinkConfig {
        path: mirror.clone(),
    });
    assert_eq!(
        nujijushi::storage::share(&h.state, &share).await.unwrap(),
        (1, 0)
    );
    assert_eq!(
        nujijushi::storage::enqueue(&mirrored, &[id]).await.unwrap(),
        1
    );
    assert_eq!(nujijushi::storage::drain(&mirrored, 10).await.unwrap(), 1);
    let outgoing = h
        .state
        .db
        .with(|conn| gallery::outgoing_post(conn, id))
        .unwrap();
    let planned = nujijushi::storage::plan(&h.state, &outgoing).unwrap();
    for root in [&share, &mirror] {
        assert!(!root.join(&planned.dir).join("00.png").exists());
        for (position, bytes) in [&c, &b, &a].into_iter().enumerate() {
            assert_eq!(
                &std::fs::read(
                    root.join(&planned.dir)
                        .join(format!("{:02}.png", position + 1))
                )
                .unwrap(),
                bytes
            );
        }
    }
    assert!(matches!(
        publish_item(&h.state, queue_id, false).await.unwrap(),
        PublishOutcome::Published { .. }
    ));
    let sent = h.mock.sends();
    let sent_paths: Vec<_> = sent
        .iter()
        .flat_map(|send| send.media.iter().map(|item| &item.path))
        .collect();
    assert_eq!(
        sent_paths,
        planned
            .files
            .iter()
            .map(|file| &file.path)
            .collect::<Vec<_>>()
    );

    assert_eq!(
        archive_test_images(
            &h.state,
            "local",
            "ordered",
            &[
                (&b, "png"),
                (&a, "png"),
                (&new, "png"),
                (&a, "png"),
                (&c, "png")
            ]
        ),
        id
    );
    let reimported = load();
    assert_eq!(
        reimported.media[..4]
            .iter()
            .map(|item| item.blob_hash.as_str())
            .collect::<Vec<_>>(),
        [2, 3, 1, 0].map(|index| original.media[index].blob_hash.as_str())
    );
    assert_eq!(
        reimported.media[4].blob_hash,
        blake3::hash(&new).to_hex().as_str()
    );
    assert_eq!(
        h.page(&format!("/posts/{id}")).await["media"][0]["hidden"],
        true
    );
}

#[tokio::test]
async fn gallery_reorder_carries_merge_and_compression_and_undo_expands_at_the_new_position() {
    use nujijushi::db::{media_merges as gallery, posts};
    use nujijushi::jobs::{self, worker::JobWorker};
    use nujijushi::media::compress::Compression;

    let h = harness();
    let panels: Vec<_> = (1..=4)
        .map(|shade| {
            encode_image(
                image::RgbImage::from_pixel(8, 8, image::Rgb([shade, 20, 30])),
                image::ImageFormat::Png,
            )
        })
        .collect();
    let files = panels
        .iter()
        .map(|bytes| (bytes.as_slice(), "png"))
        .collect::<Vec<_>>();
    let id = archive_test_images(&h.state, "local", "ordered-merge", &files);
    let load = || h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let version = || {
        h.state
            .db
            .with(|conn| gallery::current_revision(conn, id))
            .unwrap()
    };
    let original = load();
    let reorder = |order: &[i64]| {
        let revision = version();
        h.state
            .db
            .with(|conn| gallery::reorder(conn, id, order, &revision))
            .unwrap()
    };
    reorder(&[3, 0, 2, 1].map(|index| original.media[index].id));
    let selected = vec![original.media[2].id, original.media[3].id];
    let worker = JobWorker::new(h.state.clone());
    let job = jobs::create_merge_media_job(&h.state, id, selected, version(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(h.job_state(job).await, "done");
    let merged = load();
    assert_eq!(merged.media[0].width, Some(16));
    assert_eq!(merged.media[1].blob_hash, original.media[0].blob_hash);
    let job = jobs::create_compress_media_job(
        &h.state,
        id,
        Some(merged.media[0].id),
        version(),
        Compression::new(75, false).unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(h.job_state(job).await, "done");
    let compressed = load();
    reorder(&[1, 2, 0].map(|index| compressed.media[index].id));
    let moved = load();
    assert_eq!(moved.media[2].blob_hash, compressed.media[0].blob_hash);
    let revision = version();
    let merge = h
        .state
        .db
        .with(|conn| {
            assert_eq!(gallery::compressed_positions(conn, id)?, [2]);
            let merges = gallery::undo_ids(conn, id)?;
            assert_eq!(merges[0].0, 2);
            gallery::undo_compression(conn, id, moved.media[2].id, &revision)?;
            Ok(merges[0].1)
        })
        .unwrap();
    assert_eq!(load().media[2].blob_hash, merged.media[0].blob_hash);
    let revision = version();
    h.state
        .db
        .with(|conn| gallery::undo(conn, id, merge, &revision))
        .unwrap();
    assert_eq!(
        load()
            .media
            .iter()
            .map(|item| item.blob_hash.as_str())
            .collect::<Vec<_>>(),
        [0, 1, 3, 2].map(|index| original.media[index].blob_hash.as_str())
    );
}

#[tokio::test]
async fn batch_compression_preserves_the_gallery_on_encode_failure() {
    use nujijushi::{
        db::{media_merges as gallery, posts},
        jobs::worker::JobWorker,
    };
    let h = harness();
    let left = encode_image(
        image::RgbImage::from_pixel(30, 20, image::Rgb([80, 90, 100])),
        image::ImageFormat::Png,
    );
    let id = archive_test_images(
        &h.state,
        "local",
        "broken-gallery",
        &[(&left, "png"), (b"broken", "png")],
    );
    let original = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let version = gallery::gallery_version(&original.media);
    let form = format!(
        "csrf={}&action=compress_all&gallery_version={version}&output=fit",
        h.ws.csrf
    );
    let response = h.send(h.post(&format!("/posts/{id}/media"), &form)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let job = Harness::json(&response)["job_ids"][0].as_i64().unwrap();
    assert_eq!(JobWorker::new(h.state.clone()).drain().await.unwrap(), 1);
    let status = h.page(&format!("/jobs/{job}/status")).await;
    assert_eq!(status["state"], "failed");
    assert!(status["message"].as_str().unwrap().contains("Image 2"));
    assert_eq!(
        h.state.db.with(|conn| posts::get(conn, id)).unwrap().media,
        original.media
    );
}

#[tokio::test]
async fn sent_cleanup_keeps_batch_edits_hides_and_saved_tags_and_rechecks_the_preview() {
    use nujijushi::db::{media_merges as gallery, posts, queue, tags};
    use nujijushi::jobs::worker::JobWorker;
    let h = harness();
    let panel = |color| {
        encode_image(
            image::RgbImage::from_pixel(24, 16, image::Rgb(color)),
            image::ImageFormat::Png,
        )
    };
    let shared = panel([70, 0, 0]);
    let right = panel([0, 70, 0]);
    let hidden = panel([0, 0, 70]);
    let input = [
        (shared.as_slice(), "png"),
        (&right, "png"),
        (&hidden, "png"),
        (b"video", "mp4"),
    ];
    let id = archive_test_images(&h.state, "local", "sent", &input);
    archive_test_images(&h.state, "local", "shared", &[(&shared, "png")]);
    let original = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let version = |conn: &rusqlite::Connection| gallery::current_revision(conn, id);
    let old_version = h.state.db.with(|conn| version(conn)).unwrap();
    let merge = nujijushi::jobs::create_merge_media_job(
        &h.state,
        id,
        original.media[..2].iter().map(|item| item.id).collect(),
        old_version,
        None,
    )
    .await
    .unwrap();
    assert!(merge.is_some());
    let worker = JobWorker::new(h.state.clone());
    worker.drain().await.unwrap();
    h.state
        .db
        .with(|conn| {
            let hidden_id = posts::get(conn, id)?.media[1].id;
            gallery::set_hidden(conn, id, hidden_id, &version(conn)?, true)
        })
        .unwrap();
    let gallery_version = h.state.db.with(|conn| version(conn)).unwrap();
    let action = format!("/posts/{id}/media");
    let form = format!(
        "csrf={}&action=compress_all&gallery_version={gallery_version}&output=quality&quality=78",
        h.ws.csrf
    );
    assert_eq!(
        h.send(h.post(&action, &form)).await.status(),
        StatusCode::ACCEPTED
    );
    worker.drain().await.unwrap();
    let edited = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let outgoing = h
        .state
        .db
        .with(|conn| gallery::outgoing_post(conn, id))
        .unwrap()
        .media;
    assert_eq!(
        edited
            .media
            .iter()
            .map(|item| item.ext.as_str())
            .collect::<Vec<_>>(),
        ["jpg", "jpg", "mp4"]
    );
    assert_eq!(
        outgoing.iter().map(|item| item.ordinal).collect::<Vec<_>>(),
        [0, 2]
    );
    let gallery_version = h.state.db.with(|conn| version(conn)).unwrap();
    h.state
        .db
        .with(|conn| gallery::undo_compression(conn, id, edited.media[1].id, &gallery_version))
        .unwrap();
    let gallery_version = h.state.db.with(|conn| version(conn)).unwrap();
    let form = format!(
        "csrf={}&action=compress_all&gallery_version={gallery_version}",
        h.ws.csrf
    );
    assert_eq!(
        h.send(h.post(&action, &form)).await.status(),
        StatusCode::ACCEPTED
    );
    worker.drain().await.unwrap();
    let edited = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let outgoing = h
        .state
        .db
        .with(|conn| gallery::outgoing_post(conn, id))
        .unwrap()
        .media;

    let channel = h.add_channel().await;
    let sent = chrono::Utc::now() - chrono::Duration::days(31);
    let queue_id = h.state.db.with(|conn| {
        conn.execute("UPDATE posts SET imported_at = ?1 WHERE id = ?2", [(sent - chrono::Duration::days(1)).to_rfc3339(), id.to_string()])?;
        let queue_id = queue::enqueue_new(conn, id, channel.id, 0)?.unwrap();
        queue::mark_sent(conn, queue_id, "123", sent)?;
        tags::add_manual(conn, id, &["saved_tag".into()])?;
        let (job_id, _) = nujijushi::db::tag_suggestions::enqueue(conn, id, "suggest_tags")?;
        job_db::set_state(conn, job_id, "done", None)?;
        conn.execute("INSERT INTO post_tag_suggestions (job_id, kind, tag, evidence) VALUES (?1, 'character', 'unsaved', 'test')", [job_id])?;
        Ok(queue_id)
    }).unwrap();

    let path = "/storage/cleanup";
    assert_eq!(
        h.page("/storage/cleanup?older_than_days=32").await["posts"],
        0
    );
    let preview = h.page(path).await;
    assert_eq!(preview["posts"], 1);
    assert_eq!(preview["suggestions"], 1);
    assert!(preview["estimated_bytes"].as_u64().unwrap() > 0);
    let form = |preview: &serde_json::Value, csrf: &str| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", csrf)
            .append_pair(
                "cutoff",
                preview["confirmation"]["cutoff"].as_str().unwrap(),
            )
            .append_pair(
                "revision",
                preview["confirmation"]["revision"].as_str().unwrap(),
            )
            .finish()
    };
    assert_eq!(
        h.send(h.post(path, &form(&preview, "invalid")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );

    h.state
        .db
        .with(|conn| {
            conn.execute(
                "UPDATE queue SET state = 'unknown' WHERE id = ?1",
                [queue_id],
            )?;
            Ok(())
        })
        .unwrap();
    let response = h.send(h.post(path, &form(&preview, &h.ws.csrf))).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stale = Harness::json(&response)["job_ids"][0].as_i64().unwrap();
    worker.drain().await.unwrap();
    let status = h.page(&format!("/jobs/{stale}/status")).await;
    assert_eq!(status["state"], "failed");
    assert!(
        status["message"]
            .as_str()
            .unwrap()
            .contains("Preview cleanup again")
    );
    assert_eq!(h.page(path).await["posts"], 0);
    h.state
        .db
        .with(|conn| queue::mark_sent(conn, queue_id, "123", sent))
        .unwrap();

    let preview = h.page(path).await;
    let response = h.send(h.post(path, &form(&preview, &h.ws.csrf))).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let job = Harness::json(&response)["job_ids"][0].as_i64().unwrap();
    worker.drain().await.unwrap();
    assert_eq!(h.job_state(job).await, "done");
    h.state
        .db
        .with(|conn| {
            assert_eq!(posts::get(conn, id)?.media, edited.media);
            assert_eq!(gallery::outgoing_post(conn, id)?.media, outgoing);
            assert_eq!(posts::get(conn, id)?.tags, ["saved_tag"]);
            assert_eq!(queue::get(conn, queue_id)?.state, QueueState::Sent);
            assert!(gallery::undo_ids(conn, id)?.is_empty());
            assert!(gallery::compressed_positions(conn, id)?.is_empty());
            assert!(nujijushi::db::tag_suggestions::latest_job(conn, id)?.is_none());
            Ok(())
        })
        .unwrap();
    assert!(
        h.state
            .blobs
            .path_for(&original.media[0].blob_hash, "png")
            .is_file()
    );
    assert!(
        !h.state
            .blobs
            .path_for(&original.media[1].blob_hash, "png")
            .exists()
    );
    assert_eq!(h.page(path).await["posts"], 0);
    assert_eq!(archive_test_images(&h.state, "local", "sent", &input), id);
    let restored = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    assert_eq!(
        restored
            .media
            .iter()
            .map(|item| &item.blob_hash)
            .collect::<Vec<_>>(),
        original
            .media
            .iter()
            .map(|item| &item.blob_hash)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn duplicate_review_finds_exact_same_source_copies_before_indexing() {
    use nujijushi::db::duplicates;

    let h = harness();
    let png = encode_image(artwork(), image::ImageFormat::Png);
    let mut gif = Vec::new();
    image::codecs::gif::GifEncoder::new(&mut gif)
        .encode_frames([
            image::Frame::new(image::RgbaImage::new(10, 10)),
            image::Frame::new(image::RgbaImage::from_pixel(
                10,
                10,
                image::Rgba([0, 0, 0, 255]),
            )),
        ])
        .unwrap();
    let left = archive_test_images(
        &h.state,
        "local",
        "duplicate-one",
        &[(&png, "png"), (&png, "png"), (&gif, "gif")],
    );
    let right = archive_test_images(
        &h.state,
        "local",
        "duplicate-two",
        &[(&png, "png"), (&gif, "gif")],
    );

    let page = h.page("/duplicates").await;
    assert_eq!(page["pending"], 3);
    assert_eq!(page["failed"], 0);
    assert_eq!(page["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(page["pairs"][0]["left"]["id"], left);
    assert_eq!(page["pairs"][0]["right"]["id"], right);
    let evidence = page["pairs"][0]["matches"].as_array().unwrap();
    assert_eq!(evidence.len(), 3);
    assert!(evidence.iter().all(|item| item["exact"] == true));
    assert_eq!(evidence[2]["left"]["position"], 3);
    assert_eq!(evidence[2]["right"]["position"], 2);
    h.state
        .db
        .with(|conn| {
            assert!(duplicates::evidence(conn, left, left)?.is_empty());
            assert!(duplicates::dismiss(conn, left, right)?);
            assert!(duplicates::pairs(conn, 0)?.is_empty());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn duplicate_index_retries_missing_blobs_and_reports_progress_across_restarts() {
    use nujijushi::db::{duplicates, posts};

    let h = harness();
    let original = artwork();
    let bytes = encode_image(original.clone(), image::ImageFormat::Png);
    let resized =
        image::imageops::resize(&original, 270, 216, image::imageops::FilterType::Triangle);
    let jpeg = encode_image(resized, image::ImageFormat::Jpeg);
    let left = archive_test_images(&h.state, "pixiv", "unreadable", &[(&bytes, "png")]);
    let exact = archive_test_images(&h.state, "pixiv", "unreadable-copy", &[(&bytes, "png")]);
    let near = archive_test_images(&h.state, "twitter", "similar", &[(&jpeg, "jpg")]);
    let hash = h
        .state
        .db
        .with(|conn| Ok(posts::get(conn, left)?.media[0].blob_hash.clone()))
        .unwrap();
    let blob = h.state.blobs.path_for(&hash, "png");
    let backup = h._dir.path().join("missing.png");
    std::fs::rename(&blob, &backup).unwrap();

    nujijushi::duplicates::index_batch(&h.state).await.unwrap();
    let failed = h.page("/duplicates").await;
    assert_eq!(failed["pending"], 0);
    assert_eq!(failed["failed"], 2);
    assert_eq!(failed["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(failed["pairs"][0]["right"]["id"], exact);
    let reopened = AppState::open((*h.state.cfg).clone(), None).unwrap();
    let attempts = || {
        reopened
            .db
            .with(|conn| {
                Ok(conn.query_row(
                    "SELECT attempts FROM duplicate_fingerprint_failures",
                    [],
                    |row| row.get::<_, i64>(0),
                )?)
            })
            .unwrap()
    };
    assert_eq!(attempts(), 1);
    nujijushi::duplicates::index_batch(&reopened).await.unwrap();
    assert_eq!(attempts(), 1);
    reopened
        .db
        .with(|conn| {
            conn.execute(
                "UPDATE duplicate_fingerprint_failures SET retry_at = unixepoch()",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    nujijushi::duplicates::index_batch(&reopened).await.unwrap();
    assert_eq!(attempts(), 2);

    let retry_path = "/duplicates/retry";
    let wrong = h.send(h.post(retry_path, "csrf=bad")).await;
    assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);
    let retry = format!("csrf={}", h.ws.csrf);
    assert_eq!(
        h.send(h.post(retry_path, &retry)).await.status(),
        StatusCode::OK
    );
    assert_eq!(h.page("/duplicates").await["pending"], 2);
    std::fs::rename(&backup, &blob).unwrap();
    nujijushi::duplicates::index_batch(&reopened).await.unwrap();
    let recovered = h.page("/duplicates").await;
    assert_eq!(recovered["pending"], 0);
    assert_eq!(recovered["failed"], 0);
    reopened
        .db
        .with(|conn| {
            for (first, second) in [(left, exact), (left, near), (exact, near)] {
                let evidence = duplicates::evidence(conn, first, second)?;
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].exact, second == exact);
            }
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn duplicate_index_works_through_older_images_first() {
    use nujijushi::db::{duplicates, posts};

    let h = harness();
    let bytes = encode_image(artwork(), image::ImageFormat::Png);
    let oldest = archive_test_images(&h.state, "local", "oldest", &[(&bytes, "png")]);
    let repeated = [(&bytes[..], "png"); 16];
    let newest = archive_test_images(&h.state, "local", "newest", &repeated);
    nujijushi::duplicates::index_batch(&h.state).await.unwrap();
    h.state
        .db
        .with(|conn| {
            let waiting = duplicates::pending(conn, 20)?;
            let newest = posts::get(conn, newest)?;
            assert_eq!(waiting.len(), 1);
            assert_ne!(waiting[0].post_id, oldest);
            assert_eq!(waiting[0].id, newest.media[15].id);
            Ok(())
        })
        .unwrap();
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
    assert_eq!(h.page("/duplicates").await["indexing"], true);
    nujijushi::duplicates::index_batch(&h.state).await.unwrap();
    let pairs = h.state.db.with(|conn| duplicates::pairs(conn, 0)).unwrap();
    assert_eq!(pairs.first(), Some(&(left, exact)));
    assert!(pairs.contains(&(left, right)));
    assert_eq!(pairs.len(), 3, "{pairs:?}");
    let page = h.page("/duplicates").await;
    let pair = page["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pair| pair["left"]["source"] == "twitter" || pair["right"]["source"] == "twitter")
        .unwrap();
    let matches = page["pairs"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|pair| pair["matches"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert!(matches.iter().any(|item| item["exact"] == true));
    assert!(matches.iter().any(|item| item["exact"] == false));
    let twitter = [&pair["left"], &pair["right"]]
        .into_iter()
        .find(|post| post["source"] == "twitter")
        .unwrap();
    assert_eq!(twitter["url"], "https://example.test/twitter/102");

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
        StatusCode::OK
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
async fn automatic_watched_scan_does_not_delay_a_later_publish_job() {
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
    let page = h.page("/watches").await;
    let watch = &page["watches"][0];
    assert_eq!(watch["job"]["state"], "running");
    assert!(watch["last_run_at"].as_str().is_some());
    let job_id = watch["job"]["id"].as_i64().unwrap();
    assert_eq!(h.mock.send_count(), 1);
    std::fs::write(base.join("watch-release"), "").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(4), async {
        while h.job_state(job_id).await != "done" {
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

#[tokio::test]
async fn post_detail_renders_with_tag_suggestions_enabled_before_the_first_job() {
    let mut h = harness();
    Arc::make_mut(&mut h.state.cfg).tag_suggestions.enabled = true;
    h.ws = WebState::new(h.state.clone()).unwrap();
    let post_id = h.post_only("suggestions-not-started").await;
    for query in ["", "?section=suggestions"] {
        let response = h
            .send(h.get(&format!("/api/v1/posts/{post_id}{query}")))
            .await;
        let status = response.status();
        let body = body_text(response).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let view: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(view["can_suggest_tags"], true);
        assert!(view["tag_review"].is_null());
    }
}

fn foidfinder_reply(
    strong: &[(&str, f32)],
    guesses: &[(&str, f32)],
    series: &[(&str, f32)],
) -> axum::Json<serde_json::Value> {
    let entries = |hits: &[(&str, f32)]| {
        hits.iter()
            .map(|(tag, score)| serde_json::json!({"tag": tag, "score": score}))
            .collect::<Vec<_>>()
    };
    axum::Json(serde_json::json!({
        "tags": { "character": entries(strong), "copyright": entries(series) },
        "candidates": { "character": entries(guesses) }
    }))
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
                    b"still-one" => foidfinder_reply(
                        &[
                            ("pikachu", 0.96),
                            ("hidden_source", 0.39),
                            ("unknown_character", 0.38),
                            ("pikachu_alias", 0.36),
                            ("original", 0.35),
                        ],
                        &[("weak_character", 0.18), ("other_guess", 0.2)],
                        &[("model_series", 0.55)],
                    )
                    .into_response(),
                    b"still-two" => foidfinder_reply(
                        &[("pikachu", 0.97)],
                        &[("unknown_character", 0.26), ("weak_character", 0.24)],
                        &[("model_series", 0.77)],
                    )
                    .into_response(),
                    b"animated" => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
                    _ => panic!("unexpected image request"),
                }
            }
        }
    };
    let server = tokio::spawn(async move {
        axum::serve(companion, Router::new().route("/api/tag", post(handler)))
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
    assert_eq!(h.send(anonymous).await.status(), StatusCode::UNAUTHORIZED);
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
    let mut statuses = [first.status(), second.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::ACCEPTED]);
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
    let detail = h.page(&format!("/posts/{post_id}")).await;
    let suggestions = detail["tag_review"]["suggestions"].as_array().unwrap();
    let positions = suggestions
        .iter()
        .flat_map(|suggestion| suggestion["images"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert!(positions.iter().any(|position| **position == 1));
    assert!(positions.iter().any(|position| **position == 3));
    assert!(!positions.iter().any(|position| **position == 2));
    assert!(suggestions.iter().any(|suggestion| {
        suggestion["evidence"]
            .as_str()
            .unwrap()
            .contains("Low confidence")
    }));
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
            "pokemon",
            "model_series"
        ]
    );
    assert_eq!(review.suggestions[0].images, [1, 3]);
    assert_eq!(review.suggestions[0].character_tag_id, Some(3861));
    assert_eq!(review.suggestions[1].images, [1]);
    assert_eq!(review.suggestions[2].images, [3]);
    assert_eq!(review.suggestions[5].images, [1, 3]);
    assert!(review.suggestions[5].evidence.contains("FoidFinder"));
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
    assert_eq!(saved.status(), StatusCode::OK);
    let discarded = h
        .send(h.post(
            &path,
            &resolve(review.suggestions[4].id, "discard", "pokemon"),
        ))
        .await;
    assert_eq!(discarded.status(), StatusCode::OK);
    h.state
        .db
        .with(|conn| {
            assert_eq!(
                tags::for_post(conn, post_id)?,
                vec!["original", "keep_manual", "fixed_character"]
            );
            assert_eq!(tags::removals(conn, post_id)?[0].name, "hidden_source");
            let review = tag_suggestions::review(conn, &posts::get(conn, post_id)?)?.unwrap();
            assert_eq!(review.suggestions.len(), 4);
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
    let discard_all = format!("csrf={}&job_id={}&action=discard_all", h.ws.csrf, job.id);
    assert_eq!(
        h.send(h.post(&path, &discard_all)).await.status(),
        StatusCode::BAD_REQUEST
    );
    h.state
        .db
        .with(|conn| {
            let mut updated = NormalizedPost::new("local", "tagged01");
            updated.tags = vec!["hidden_source".into(), "original".into()];
            updated.raw_json = Some(r#"{"tag_string_copyright":"source_series"}"#.into());
            posts::upsert(conn, &updated, &[])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        h.page(&format!("/posts/{post_id}")).await["tag_review"]["suggestions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        h.send(h.post(&path, &discard_all)).await.status(),
        StatusCode::OK
    );
    let detail = h.page(&format!("/posts/{post_id}")).await;
    assert_eq!(detail["tag_review"]["suggestions"], serde_json::json!([]));
    h.state
        .db
        .with(|conn| {
            let mut remaining = tags::for_post(conn, post_id)?;
            remaining.sort();
            assert_eq!(remaining, ["fixed_character", "keep_manual", "original"]);
            assert_eq!(tags::removals(conn, post_id)?[0].name, "hidden_source");
            Ok(())
        })
        .unwrap();
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
        "/api/tag",
        post(|| async {
            foidfinder_reply(
                &[],
                &[
                    ("pikachu", 0.25),
                    ("second_guess", 0.2),
                    ("third_guess", 0.1),
                ],
                &[],
            )
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
                foidfinder_reply(&[], &[], &[])
            }
        }
    };
    let server = tokio::spawn(async move {
        let router = axum::Router::new().route("/api/tag", axum::routing::post(handler));
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
    assert_eq!(h.page(&format!("/posts/{id}")).await["can_merge"], true);
    assert_eq!(
        h.send(h.post(&path, "csrf=wrong&action=merge"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.send(h.post(&path, &form(&originals.media, &[0])))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let first_merge = form(&originals.media, &[1, 0]);
    let mut anonymous = h.post(&path, &first_merge);
    anonymous.headers_mut().remove(header::COOKIE);
    assert_eq!(h.send(anonymous).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        h.send(h.post(&path, &first_merge)).await.status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        h.send(h.post(&path, &first_merge)).await.status(),
        StatusCode::OK
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
    let done = h.page(&format!("/posts/{id}")).await;
    assert!(done["media"][0]["undo_id"].is_number());
    assert!(
        done["merge_job"]["message"]
            .as_str()
            .unwrap()
            .contains("Merged 2 images")
    );
    assert_eq!(done["merge_job"]["progress"], 2);
    assert_eq!(done["merge_job"]["total"], 2);

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
        StatusCode::ACCEPTED
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
        StatusCode::ACCEPTED
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
        StatusCode::ACCEPTED
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
    assert_eq!(h.send(h.post(&path, &undo)).await.status(), StatusCode::OK);
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
        .is_some()
    );
    assert_eq!(worker.drain().await.unwrap(), 1);
    let merged = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    h.state
        .db
        .with(|conn| {
            let version = media_merges::edit_version(conn, id, &merged.media)?;
            media_merges::set_hidden(conn, id, merged.media[1].id, &version, true)
        })
        .unwrap();
    imported.files.swap(0, 1);
    imported.files.swap(2, 3);
    let refreshed = jobs::import_posts(&h.state, vec![imported.clone()], None, None)
        .await
        .unwrap();
    assert_eq!(refreshed.post_ids, [id]);
    assert_eq!(refreshed.gallery_edits_reset, 0);
    h.state
        .db
        .with(|conn| {
            let post = posts::get(conn, id)?;
            assert_eq!(post.media.len(), 4);
            assert_eq!(post.media[0].blob_hash, merged.media[0].blob_hash);
            assert_eq!(media_merges::hidden_positions(conn, id, &post.media)?, [2]);
            Ok(())
        })
        .unwrap();

    imported.files.remove(0);
    let refreshed = jobs::import_posts(&h.state, vec![imported], None, None)
        .await
        .unwrap();
    assert_eq!(refreshed.gallery_edits_reset, 1);
    assert!(refreshed.summary().contains("source gallery changed"));
    let refreshed = h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    assert_eq!(refreshed.media.len(), 4);
    assert_eq!(refreshed.media[0].blob_hash, originals.media[0].blob_hash);
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
    assert!(enqueue(original.clone()).await.unwrap().is_some());
    assert_eq!(worker.drain().await.unwrap(), 1);
    assert_eq!(latest_job().state, "failed");
    assert!(latest_job().message.unwrap().contains("same height"));
    assert_eq!(get().media, original.media);
    assert!(enqueue(original).await.unwrap().is_some());
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
    let page = h.page(&post_url).await;
    assert_eq!(page["media"][0]["can_compress"], true);
    assert_eq!(page["media"][0]["over_photo_limit"], true);
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
    let exact = form.replace("output=fit", "output=quality");
    assert_eq!(
        h.send(h.post(&action_url, &format!("{exact}&quality=101")))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.send(h.post(&action_url, &format!("{form}&quality=101")))
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let job = h
        .state
        .db
        .with(|conn| media_merges::latest_job(conn, post_id))
        .unwrap()
        .unwrap();
    assert!(job.payload.unwrap().ends_with(":85:true"));
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
    assert_eq!(h.page(&post_url).await["media"][0]["compressed"], true);

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
        StatusCode::OK
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
        StatusCode::ACCEPTED
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
    assert!(h.page(&post_url).await["media"][0]["undo_id"].is_number());
}

#[tokio::test]
async fn setup_explains_credentials_and_accepts_web_only_publishing() {
    let mut h = harness();
    let cfg = Arc::make_mut(&mut h.state.cfg);
    let token = cfg.bot_token.take();
    cfg.bot_enabled = false;
    cfg.access.admins.clear();
    h.ws = WebState::new(h.state.clone()).unwrap();
    assert_eq!(
        h.page("/").await["setup"]["next"]["href"],
        "#telegram-setup"
    );

    Arc::make_mut(&mut h.state.cfg).bot_token = token;
    h.ws = WebState::new(h.state.clone()).unwrap();
    assert_eq!(
        h.page("/").await["setup"]["next"]["href"],
        "/channels#add-channel"
    );

    (*h.mock)
        .clone()
        .with_chat(nujijushi::tg::botapi::ChatInfo {
            id: -1001234567890,
            title: Some("Test channel".into()),
            username: Some("test_channel".into()),
            is_channel: true,
            is_private: false,
        });
    let response = h
        .send(h.post(
            "/channels",
            &format!(
                "csrf={}&title=Art&alias=art&tg_id=%40test_channel",
                h.ws.csrf
            ),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let channel = h
        .state
        .db
        .with(|conn| nujijushi::db::channels::get(conn, 1))
        .unwrap();
    assert!(!channel.enabled);
}

#[tokio::test]
async fn setup_holds_the_first_post_until_preview_and_survives_an_empty_queue() {
    use nujijushi::db::queue;
    use nujijushi::tg::publish::{PublishOutcome, publish_channel, publish_item};

    let h = harness();
    let channel = h.add_channel().await;
    let post_id = h.post_only("1").await;
    h.state
        .db
        .set_setting(
            nujijushi::queue::scheduler::TICK_KEY,
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();

    assert_eq!(h.page("/posts?setup=true").await["setup"], true);
    let preview_path = format!("/channels/{}/preview", channel.id);
    let form = format!(
        "csrf={}&channel={}&post_id={post_id}",
        h.ws.csrf, channel.id
    );
    let response = h.send(h.post("/setup/queue", &form)).await;
    assert_eq!(Harness::json(&response)["location"], preview_path);
    let item = h
        .state
        .db
        .with(|conn| queue::list(conn, channel.id, &[QueueState::Held], 1))
        .unwrap()
        .pop()
        .expect("the queue holds a review item");
    assert!(!matches!(
        publish_channel(&h.state, &channel, false, false)
            .await
            .unwrap(),
        PublishOutcome::Published { .. }
    ));
    assert!(matches!(
        publish_item(&h.state, item.id, false).await.unwrap(),
        PublishOutcome::NotDue(_)
    ));
    assert_eq!(h.mock.send_count(), 0);

    assert_eq!(h.page("/").await["setup"]["next"]["action"], "Open preview");
    let preview = h.page(&preview_path).await;
    assert_eq!(preview["held"]["post_id"], post_id);
    assert_eq!(preview["enable_item"]["post_id"], post_id);
    assert!(
        preview["held"]["caption"]
            .as_str()
            .unwrap()
            .contains("#original")
    );
    assert_eq!(h.mock.send_count(), 0);

    let confirm = |csrf| {
        format!(
            "csrf={csrf}&queue_id={}&post_id={post_id}&held=true&target=%40test_channel",
            item.id
        )
    };
    let invalid = h.send(h.post(&preview_path, &confirm("wrong"))).await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    h.state
        .db
        .with(|conn| nujijushi::db::posts::set_state(conn, post_id, PostState::Rejected))
        .unwrap();
    let blocked = h.send(h.post(&preview_path, &confirm(&h.ws.csrf))).await;
    assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        h.state
            .db
            .with(|conn| queue::get(conn, item.id))
            .unwrap()
            .state,
        QueueState::Held
    );
    h.state
        .db
        .with(|conn| nujijushi::db::posts::set_state(conn, post_id, PostState::Approved))
        .unwrap();

    let response = h.send(h.post(&preview_path, &confirm(&h.ws.csrf))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let duplicate = h.send(h.post(&preview_path, &confirm(&h.ws.csrf))).await;
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    assert_eq!(h.mock.send_count(), 0, "the web request never sends");
    assert!(matches!(
        publish_channel(&h.state, &channel, false, false)
            .await
            .unwrap(),
        PublishOutcome::Published { .. }
    ));
    assert_eq!(h.mock.send_count(), 1);
    h.state
        .db
        .with(|conn| queue::delete(conn, item.id))
        .unwrap();
    h.state
        .db
        .set_setting(nujijushi::queue::scheduler::TICK_KEY, "stale")
        .unwrap();
    assert!(h.page("/").await["setup"].is_null());
    assert_eq!(h.page("/").await["scheduler"]["stale"], true);
}

#[tokio::test]
async fn setup_links_to_preview_when_only_stopped_channels_have_queued_posts() {
    let h = harness();
    let channel = h.add_channel().await;
    let (post_id, queue_id) = h.queue_a_post("1", &channel).await;
    h.state
        .db
        .with(|conn| {
            nujijushi::db::channels::set_enabled(conn, channel.id, false)?;
            nujijushi::db::channels::set_paused(conn, channel.id, true)
        })
        .unwrap();
    h.state
        .db
        .set_setting(
            nujijushi::queue::scheduler::TICK_KEY,
            &chrono::Utc::now().to_rfc3339(),
        )
        .unwrap();

    assert_eq!(h.page("/").await["setup"]["next"]["action"], "Open preview");
    let path = format!("/channels/{}/preview", channel.id);
    let preview = h.page(&path).await;
    assert_eq!(preview["stopped"], true);
    assert_eq!(preview["enable_item"]["post_id"], post_id);
    assert!(
        preview["items"][0]["caption"]
            .as_str()
            .unwrap()
            .contains("#original")
    );
    let form = format!(
        "csrf={}&queue_id={queue_id}&post_id={post_id}&held=false&target=%40test_channel",
        h.ws.csrf
    );
    assert_eq!(h.send(h.post(&path, &form)).await.status(), StatusCode::OK);
    let stored = h
        .state
        .db
        .with(|conn| nujijushi::db::channels::get(conn, channel.id))
        .unwrap();
    assert!(stored.enabled && !stored.paused);
    assert_eq!(h.mock.send_count(), 0);
    assert!(h.page("/").await["setup"].is_null());
}

#[tokio::test]
async fn posts_per_page_is_bounded_and_preserved_while_paging() {
    let h = harness();
    for n in 1..=51 {
        h.post_only(&n.to_string()).await;
    }
    let defaults = h.page("/posts?per_page=10000").await;
    assert_eq!(ids(&defaults["cards"]).len(), 24);
    let first = h
        .page("/posts?per_page=48&source=pixiv&search=Artwork&setup=true")
        .await;
    assert_eq!(ids(&first["cards"]).len(), 48);
    assert_eq!(first["page_count"], 2);
    let next = first["next_page"].as_str().unwrap();
    assert!(next.contains("search=Artwork&source=pixiv&setup=true&per_page=48&page=2"));
    let second = h.page("/posts?page=999&per_page=48").await;
    assert_eq!(ids(&second["cards"]).len(), 3);
    assert_eq!(second["page_number"], 2);
    assert!(second["next_page"].is_null());
    assert_eq!(ids(&h.page("/posts?per_page=96").await["cards"]).len(), 51);
}

#[tokio::test]
async fn post_page_deletion_requires_confirmation_and_waits_for_an_active_send() {
    use nujijushi::db::{posts, queue};
    use nujijushi::queue::lease;

    let h = harness();
    let channel = h.add_channel().await;
    let (id, queue_id) = h.queue_a_post("65", &channel).await;
    let other = h.post_only("66").await;
    let path = format!("/posts/{id}");
    let confirmation = format!("{path}/delete");
    let view = h.page(&confirmation).await;
    assert_eq!(view["id"], id);
    assert_eq!(view["sending"], false);
    assert_eq!(h.send(h.get(&path)).await.status(), StatusCode::OK);

    let imported = h
        .state
        .db
        .with(|conn| posts::get(conn, id))
        .unwrap()
        .imported_at;
    let form = |csrf: &str, stamp: chrono::DateTime<chrono::Utc>| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", csrf)
            .append_pair("imported_at", &stamp.to_rfc3339())
            .finish()
    };
    let valid = form(&h.ws.csrf, imported);
    let wrong_csrf = form("wrong", imported);
    assert_eq!(
        h.send(h.post(&confirmation, &wrong_csrf)).await.status(),
        StatusCode::BAD_REQUEST
    );
    let stale = form(&h.ws.csrf, imported - chrono::Duration::seconds(1));
    assert_eq!(
        h.send(h.post(&confirmation, &stale)).await.status(),
        StatusCode::BAD_REQUEST
    );

    assert!(
        lease::acquire(&h.state, queue_id, channel.tg_id)
            .await
            .unwrap()
            .is_some()
    );
    let blocked = h.send(h.post(&confirmation, &valid)).await;
    assert!(Harness::message(&blocked).contains("Wait for the current send"));
    assert_eq!(h.page(&confirmation).await["sending"], true);
    assert_eq!(
        h.state
            .db
            .with(|conn| queue::get(conn, queue_id))
            .unwrap()
            .state,
        QueueState::Sending
    );
    lease::release(&h.state, queue_id).await.unwrap();

    let response = h.send(h.post(&confirmation, &valid)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(Harness::json(&response)["location"], "/posts");
    assert_eq!(h.send(h.get(&path)).await.status(), StatusCode::NOT_FOUND);
    assert!(h.state.db.with(|conn| queue::get(conn, queue_id)).is_err());
    assert!(h.state.db.with(|conn| posts::get(conn, other)).is_ok());
}

#[tokio::test]
async fn gapped_merge_visibility_and_outgoing_copies_use_the_same_gallery() {
    use nujijushi::db::{media_merges as gallery, posts, queue};
    use nujijushi::jobs::{self, worker::JobWorker};
    use nujijushi::tg::publish::{PublishOutcome, publish_item};

    let h = harness();
    let channel = h.add_channel().await;
    let panels: Vec<_> = (1..=4_u8)
        .map(|n| {
            encode_image(
                image::RgbImage::from_pixel(8, 8, image::Rgb([n, 50, 30])),
                image::ImageFormat::Png,
            )
        })
        .collect();
    let files: Vec<_> = panels
        .iter()
        .map(|bytes| (bytes.as_slice(), "png"))
        .collect();
    let id = archive_test_images(&h.state, "local", "gapped", &files);
    let load = || h.state.db.with(|conn| posts::get(conn, id)).unwrap();
    let version = || {
        h.state
            .db
            .with(|conn| gallery::edit_version(conn, id, &posts::get(conn, id)?.media))
            .unwrap()
    };
    let original = load();
    let ids = [0, 1, 3].map(|index| original.media[index].id);
    let path = format!("/api/v1/posts/{id}/media");
    let mut form = format!(
        "csrf={}&action=merge&gallery_version={}",
        h.ws.csrf,
        version()
    );
    for media in ids {
        form.push_str(&format!("&sel_{media}=on"));
    }
    let response = h.send(h.post(&path, &form)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    let job = body["job_ids"][0].as_i64().unwrap();
    assert_eq!(JobWorker::new(h.state.clone()).drain().await.unwrap(), 1);
    let status: serde_json::Value = serde_json::from_str(
        &body_text(h.send(h.get(&format!("/api/v1/jobs/{job}/status"))).await).await,
    )
    .unwrap();
    assert_eq!(status["state"], "done");
    let merged = load();
    assert_eq!(merged.media[0].width, Some(24));
    assert_eq!(merged.media[1].blob_hash, original.media[2].blob_hash);
    let merge_id = h
        .state
        .db
        .with(|conn| Ok(gallery::undo_ids(conn, id)?[0].1))
        .unwrap();
    let previous_version = version();
    h.state
        .db
        .with(|conn| gallery::undo(conn, id, merge_id, &previous_version))
        .unwrap();
    assert_eq!(
        load()
            .media
            .iter()
            .map(|item| &item.blob_hash)
            .collect::<Vec<_>>(),
        original
            .media
            .iter()
            .map(|item| &item.blob_hash)
            .collect::<Vec<_>>()
    );

    let queue_id = h
        .state
        .db
        .with(|conn| Ok(queue::enqueue_new(conn, id, channel.id, 0)?.unwrap()))
        .unwrap();
    let before = load();
    let publish_revision = version();
    let set_hidden = |media_id, hidden| {
        let edit = version();
        h.state
            .db
            .with(|conn| gallery::set_hidden(conn, id, media_id, &edit, hidden))
            .unwrap()
    };
    set_hidden(before.media[2].id, true);
    assert!(
        nujijushi::queue::lease::acquire_for_gallery(
            &h.state,
            queue_id,
            channel.tg_id.clone(),
            before.id,
            publish_revision
        )
        .await
        .unwrap()
        .is_none()
    );
    let trimmed = h
        .state
        .db
        .with(|conn| gallery::outgoing_post(conn, id))
        .unwrap();
    assert_eq!(
        trimmed
            .media
            .iter()
            .map(|item| item.ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 3]
    );
    let mirror = nujijushi::storage::plan(&h.state, &trimmed).unwrap();
    assert!(
        !mirror
            .files
            .iter()
            .any(|file| file.relative.ends_with("/02.png"))
    );
    let parent = tempfile::tempdir().unwrap();
    let exported = parent.path().join("share");
    assert_eq!(
        nujijushi::storage::share(&h.state, &exported)
            .await
            .unwrap(),
        (1, 0)
    );
    assert!(!exported.join(&mirror.dir).join("02.png").exists());
    assert!(exported.join(mirror.meta_name()).exists());
    assert!(
        matches!(publish_item(&h.state, queue_id, true).await.unwrap(), PublishOutcome::DryRun(plan) if plan.media_count() == 3)
    );
    let mut mirrored = h.state.clone();
    let root = parent.path().join("mirror");
    std::sync::Arc::make_mut(&mut mirrored.cfg).storage.local =
        Some(nujijushi::config::LocalSinkConfig { path: root.clone() });
    assert_eq!(
        nujijushi::storage::enqueue(&mirrored, &[id]).await.unwrap(),
        1
    );
    assert_eq!(nujijushi::storage::drain(&mirrored, 10).await.unwrap(), 1);
    assert!(!root.join(&mirror.dir).join("02.png").exists());
    assert!(root.join(mirror.meta_name()).exists());
    assert!(matches!(
        publish_item(&h.state, queue_id, false).await.unwrap(),
        PublishOutcome::Published { .. }
    ));
    let sent = h.mock.sends();
    assert_eq!(sent.iter().map(|batch| batch.media.len()).sum::<usize>(), 3);
    let hidden_path = h.state.blobs.path_for(&before.media[2].blob_hash, "png");
    assert!(
        sent.iter()
            .flat_map(|batch| &batch.media)
            .all(|media| media.path != hidden_path)
    );

    let rejected = jobs::create_merge_media_job(
        &h.state,
        id,
        before.media[..3].iter().map(|m| m.id).collect(),
        version(),
        None,
    )
    .await;
    assert!(
        rejected
            .unwrap_err()
            .to_string()
            .contains("Restore hidden images")
    );
    set_hidden(before.media[2].id, false);
    let restored = parent.path().join("restored");
    assert_eq!(
        nujijushi::storage::share(&h.state, &restored)
            .await
            .unwrap(),
        (1, 0)
    );
    assert!(restored.join(&mirror.dir).join("02.png").exists());
    assert_eq!(
        h.state
            .db
            .with(|conn| gallery::outgoing_post(conn, id))
            .unwrap()
            .media
            .len(),
        4
    );
}

#[tokio::test]
async fn web_channels_require_a_verified_broadcast_destination_and_posting_rights() {
    use nujijushi::db::channels;
    use nujijushi::tg::{botapi::ChatInfo, transport::AdminStatus};

    let mut h = harness();
    let public = ChatInfo {
        id: -1001234567810,
        title: Some("Public".into()),
        username: Some("mock_public".into()),
        is_channel: true,
        is_private: false,
    };
    let private = ChatInfo {
        id: -1001234567820,
        title: Some("Private channel".into()),
        username: None,
        ..public.clone()
    };
    let group = ChatInfo {
        id: -1001234567830,
        username: Some("mock_group".into()),
        is_channel: false,
        ..public.clone()
    };
    (*h.mock)
        .clone()
        .with_chat(public)
        .with_chat(private)
        .with_chat(group);
    let form = |csrf: &str, alias: &str, target: &str| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", csrf)
            .append_pair("title", alias)
            .append_pair("alias", alias)
            .append_pair("tg_id", target)
            .append_pair("tz", "Pacific/Chatham")
            .finish()
    };
    let created = |alias, target| form(&h.ws.csrf, alias, target);
    for (alias, target) in [("public", "@mock_public"), ("private", "-1001234567820")] {
        let response = h
            .send(h.post("/api/v1/channels", &created(alias, target)))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_text(response).await.contains("verified and created"));
    }
    let list = || h.state.db.with(|conn| channels::list(conn)).unwrap();
    assert_eq!(list().len(), 2);
    assert!(list().iter().all(|channel| channel.tz == "Pacific/Chatham"));

    for (target, reason) in [
        ("@missing", "could not find this channel"),
        ("@mock_group", "broadcast channel"),
    ] {
        let response = h
            .send(h.post("/api/v1/channels", &created("bad", target)))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains(reason));
    }

    (*h.mock).clone().with_admin(AdminStatus {
        title: None,
        kind: "administrator".into(),
        is_admin: true,
        can_post: false,
    });
    let response = h
        .send(h.post("/api/v1/channels", &created("denied", "@mock_public")))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(response)
            .await
            .contains("permission to post messages")
    );
    assert_eq!(list().len(), 2);
    let public_channel = list()
        .into_iter()
        .find(|channel| channel.tg_id == "@mock_public")
        .unwrap();
    let edit = format!("/api/v1/channels/{}/edit", public_channel.id);
    let rejected = h
        .send(h.post(&edit, &created("modified", "-1001234567820")))
        .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        h.state
            .db
            .with(|conn| channels::get(conn, public_channel.id))
            .unwrap(),
        public_channel
    );

    let saved = h
        .send(h.post(&edit, &created("renamed", "@mock_public")))
        .await;
    assert_eq!(saved.status(), StatusCode::OK);

    let timeout_transport = Arc::new(
        (*h.mock)
            .clone()
            .with_chat_delay(std::time::Duration::from_secs(60)),
    );
    h.state.transport = Some(timeout_transport);
    h.ws = WebState::new(h.state.clone()).unwrap();
    let response = h
        .send(h.post(
            "/api/v1/channels",
            &form(&h.ws.csrf, "slow", "@mock_public"),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(response)
            .await
            .contains("did not respond in time")
    );
    assert_eq!(
        h.state.db.with(|conn| channels::list(conn)).unwrap().len(),
        2
    );
}

#[tokio::test]
async fn channel_template_names_are_validated_before_saving() {
    use nujijushi::db::channels;

    let h = harness();
    let form = |csrf: &str, target: &str, template: &str| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("csrf", csrf)
            .append_pair("title", "Template test")
            .append_pair("alias", "template-test")
            .append_pair("tg_id", target)
            .append_pair("template_name", template)
            .finish()
    };

    let response = h
        .send(h.post(
            "/api/v1/channels",
            &form(&h.ws.csrf, "@template_test", "does-not-exist"),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(response)
            .await
            .contains("unknown caption template"),
        "the API must name the bad template"
    );
    assert!(
        h.state
            .db
            .with(|conn| channels::list(conn))
            .unwrap()
            .is_empty(),
        "nothing is saved when the template is invalid"
    );

    let stored = h.add_channel().await;
    let edit = format!("/api/v1/channels/{}/edit", stored.id);
    let response = h
        .send(h.post(&edit, &form(&h.ws.csrf, &stored.tg_id, "does-not-exist")))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        h.state
            .db
            .with(|conn| channels::get(conn, stored.id))
            .unwrap()
            .template_name,
        "default",
        "the stored channel is untouched"
    );
}

#[tokio::test]
async fn a_stale_sending_row_is_resolvable_from_the_queue_page() {
    use nujijushi::db::queue;

    let h = harness();
    let channel = h.add_channel().await;
    let (_post_id, queue_id) = h.queue_a_post("stale", &channel).await;
    h.state
        .db
        .call(move |conn| {
            conn.execute(
                "UPDATE queue SET state = 'sending', lease_token = 'dead', lease_at = ?1
                 WHERE id = ?2",
                rusqlite::params![
                    (chrono::Utc::now() - chrono::Duration::hours(1))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    queue_id
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let path = format!("/api/v1/channels/{}/queue", channel.id);
    let response = h.send(h.get(&path)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let page: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    let row = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"].as_i64() == Some(queue_id))
        .expect("queue row");
    assert_eq!(row["stale_sending"], true);
    assert_eq!(row["state_label"], "Interrupted send");

    let form = format!(
        "csrf={}&id={queue_id}&channel={}&action=delivered",
        h.ws.csrf, channel.id
    );
    let response = h.send(h.post("/queue/action", &form)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let stored = h
        .state
        .db
        .call(move |conn| queue::get(conn, queue_id))
        .await
        .unwrap();
    assert_eq!(stored.state, QueueState::Sent);
}

#[tokio::test]
async fn queue_status_tracks_worker_and_scheduled_deliveries_for_its_channel() {
    use nujijushi::db::{jobs, queue};
    use nujijushi::jobs::worker::JobWorker;

    let h = harness();
    let channel = h.add_channel().await;
    let (post_id, queue_id) = h.queue_a_post("live", &channel).await;
    let status_path = format!("/api/v1/channels/{}/queue/status", channel.id);
    let status = || async {
        let response = h.send(h.get(&status_path)).await;
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_str::<serde_json::Value>(&body_text(response).await).unwrap()
    };
    let first = status().await;
    assert_eq!(first["revision"], status().await["revision"]);
    let publish_url = format!("/channels/{}/queue", channel.id);
    let form = format!("csrf={}&queue_id={queue_id}&back={publish_url}", h.ws.csrf);
    let response = h.send(h.post("/api/v1/publish", &form)).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let pending = status().await;
    assert_ne!(first["revision"], pending["revision"]);
    assert_eq!(pending["jobs"][0]["state"], "pending");
    assert_eq!(pending["jobs"][0]["queue_id"], queue_id);
    let job_id = pending["jobs"][0]["id"].as_i64().unwrap();
    h.state
        .db
        .with(|conn| jobs::set_state(conn, job_id, "running", None))
        .unwrap();
    assert_eq!(status().await["jobs"][0]["state"], "running");
    h.state
        .db
        .with(|conn| jobs::set_state(conn, job_id, "pending", None))
        .unwrap();
    assert_eq!(JobWorker::new(h.state.clone()).drain().await.unwrap(), 1);
    let sent = status().await;
    assert_eq!(sent["jobs"][0]["state"], "done");
    assert_eq!(sent["latest_attempt"]["state"], "Sent");
    assert_eq!(sent["latest_attempt"]["post_id"], post_id);

    let (next_post, next_queue) = h.queue_a_post("scheduled", &channel).await;
    let fresh = h
        .state
        .db
        .with(|conn| {
            nujijushi::db::channels::set_next_due(conn, channel.id, None)?;
            nujijushi::db::channels::get(conn, channel.id)
        })
        .unwrap();
    let outcome = nujijushi::tg::publish::publish_channel(&h.state, &fresh, false, true)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        nujijushi::tg::publish::PublishOutcome::Published { .. }
    ));
    let updated = status().await;
    assert_eq!(updated["latest_attempt"]["post_id"], next_post);
    assert_eq!(updated["jobs"].as_array().unwrap().len(), 1);
    assert_ne!(updated["revision"], sent["revision"]);
    let distinct = h
        .state
        .db
        .with(|conn| queue::get(conn, next_queue))
        .unwrap();
    assert_eq!(distinct.state, QueueState::Sent);
}

#[tokio::test]
async fn tag_autocomplete_ranks_visible_prefix_matches_and_skips_existing_tags() {
    use nujijushi::db::{posts, tags};

    let h = harness();
    let first = h.post_only("autocomplete-first").await;
    let second = h.post_only("autocomplete-second").await;
    h.state
        .db
        .with(|conn| {
            let mut post = NormalizedPost::new("pixiv", "autocomplete-first");
            post.tags = vec!["night_sky".into(), "night_snow".into()];
            posts::upsert(conn, &post, &[])?;
            tags::add_manual(
                conn,
                first,
                &["night_sky".into(), "nightxs".into(), "night_sea".into()],
            )?;
            tags::add_manual(conn, second, &["night_sky".into(), "night_stars".into()])?;
            let snow = tags::provenance(conn, first)?
                .into_iter()
                .find(|tag| tag.name == "night_snow")
                .unwrap();
            tags::suppress_source(conn, first, snow.id)?;
            Ok(())
        })
        .unwrap();

    let endpoint = "/api/v1/tags/autocomplete";
    let unauthenticated = Request::get(endpoint).body(Body::empty()).unwrap();
    assert_eq!(
        h.send(unauthenticated).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(h.page(endpoint).await, serde_json::json!([]));

    let query = format!("{endpoint}?q=%23NiGhT+S");
    assert_eq!(
        h.page(&query).await,
        serde_json::json!([
            {"name": "night_sky", "count": 2},
            {"name": "night_sea", "count": 1},
            {"name": "night_stars", "count": 1}
        ])
    );
    assert_eq!(
        h.page(&format!("{query}&post_id={first}")).await,
        serde_json::json!([{"name": "night_stars", "count": 1}])
    );
}

#[tokio::test]
async fn posts_cursor_filter_and_web_default_follow_archive_state() {
    use nujijushi::db::{channels, posts, queue, tags};
    let h = harness();
    let channel = h.add_channel().await;
    let first = h.post_only("first").await;
    let second = h.post_only("second").await;
    let third = h.post_only("third").await;
    let fourth = h.post_only("fourth").await;
    let private = h.post_only("private").await;
    let history = h.post_only("history").await;
    h.state
        .db
        .with(|conn| {
            // Equal times exercise the ID tiebreaker on both sides of a cursor.
            conn.execute(
                "UPDATE posts SET imported_at = '2025-01-01T00:00:00+00:00'",
                [],
            )?;
            posts::set_state(conn, fourth, PostState::Archived)?;
            posts::set_state(conn, second, PostState::Approved)?;
            let job = queue::enqueue_new(conn, private, channel.id, 0)?.unwrap();
            queue::mark_failed(conn, job, "assigned but failed")?;
            queue::log_send(
                conn,
                queue::SendRecord {
                    channel_id: channel.id,
                    queue_id: job,
                    post_id: history,
                    ok: true,
                    caption: None,
                    message_ids: None,
                    error: None,
                    destination: None,
                },
            )?;
            tags::add_manual(
                conn,
                second,
                &[
                    "rare".into(),
                    "landscape".into(),
                    "water".into(),
                    "fourth_tag".into(),
                ],
            )?;
            Ok(())
        })
        .unwrap();

    let page = h.page("/posts?sent=not_queued").await;
    let shown = ids(&page["cards"]);
    for included in [first, second, third] {
        assert!(shown.contains(&included));
    }
    for excluded in [fourth, private, history] {
        assert!(!shown.contains(&excluded));
    }
    let tagged = h.page("/posts?tag=rare&sent=not_queued").await;
    assert_eq!(tagged["top_tags"][0]["active"], true);
    assert_eq!(tagged["top_tags"][0]["tag"], "rare");
    assert_eq!(tagged["top_tags"][0]["count"], 1);

    let cursor = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("cursor", &format!("2025-01-01T00:00:00+00:00|{third}"))
        .append_pair("sent", "not_queued")
        .finish();
    let batch = h.page(&format!("/api/v1/posts/cards?{cursor}")).await;
    let shown = ids(&batch["cards"]);
    assert!(shown.contains(&first) && shown.contains(&second) && !shown.contains(&third));
    let tags = batch["cards"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|card| card["tags"].as_array().unwrap())
        .map(|tag| tag["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(tags.contains(&"rare") && tags.contains(&"landscape") && !tags.contains(&"fourth_tag"));

    let form = format!("csrf={}&channel={}", h.ws.csrf, channel.id);
    let response = h.send(h.post("/api/v1/settings", &form)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let reopened = AppState::open((*h.state.cfg).clone(), h.state.transport.clone()).unwrap();
    assert_eq!(
        reopened
            .db
            .with(|conn| { nujijushi::db::web_queue_default(conn, &channels::list(conn)?) })
            .unwrap(),
        Some(channel.id)
    );
    for path in ["/posts".into(), format!("/posts/{second}")] {
        assert_eq!(h.page(&path).await["default_channel"], channel.id);
    }

    h.state
        .db
        .with(|conn| channels::delete(conn, channel.id))
        .unwrap();
    assert!(h.page("/settings").await["default_channel"].is_null());
}

#[tokio::test]
async fn admin_login_uses_a_separate_challenge_and_bootstraps_the_session() {
    assert_admin_login("").await;
}

#[tokio::test]
async fn the_prefixed_admin_serves_metrics_and_scopes_auth_and_redirects() {
    let prefix = "/tools/archive";
    assert_admin_login(prefix).await;

    let h = harness_at(prefix);
    for (path, expected) in [
        ("/metrics", StatusCode::NOT_FOUND),
        ("/tools/archives/metrics", StatusCode::NOT_FOUND),
        ("/tools/archive/metrics", StatusCode::OK),
    ] {
        let response = h
            .send(Request::get(path).body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), expected, "{path}");
    }
    let protected = h
        .send(
            Request::get(format!("{prefix}/api/status?sample=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(protected.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        protected.headers()[header::LOCATION],
        "/tools/archive/login?next=%2Fapi%2Fstatus%3Fsample%3D1"
    );
}

async fn assert_admin_login(prefix: &str) {
    let h = harness_at(prefix);
    let request = |path, form: Option<String>, cookie: Option<&str>| {
        let mut builder = Request::builder()
            .uri(format!("{prefix}{path}"))
            .method(if form.is_some() { "POST" } else { "GET" });
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        if form.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        }
        builder.body(Body::from(form.unwrap_or_default())).unwrap()
    };

    let challenge = h.send(request("/api/v1/login", None, None)).await;
    assert_eq!(challenge.headers()[header::CACHE_CONTROL], "no-store");
    let csrf = Harness::json(&challenge)["csrf"].as_str().unwrap();
    assert_ne!(csrf, h.ws.csrf);
    let cookie = challenge.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly; SameSite=Strict"));
    assert!(cookie.contains(&format!("Path={prefix}/api/v1/login;")));
    let cookie = cookie.split(';').next().unwrap();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("csrf", csrf)
        .append_pair("token", UI_TOKEN)
        .append_pair("next", "/posts?nsfw=no")
        .finish();
    let denied = h
        .send(request("/api/v1/login", Some(form.clone()), None))
        .await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let login = h
        .send(request("/api/v1/login", Some(form), Some(cookie)))
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    assert_eq!(Harness::json(&login)["location"], "/posts?nsfw=no");
    let session_path = if prefix.is_empty() { "/" } else { prefix };
    let cookies: Vec<_> = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert!(cookies[0].contains(&format!("Path={session_path};")));
    assert!(cookies[1].contains(&format!("Path={prefix}/api/v1/login;")));
    assert!(cookies[1].ends_with("Max-Age=0"));
    let session_cookie = cookies[0].split(';').next().unwrap();
    let session = h
        .send(request("/api/v1/session", None, Some(session_cookie)))
        .await;
    assert_eq!(Harness::json(&session)["csrf"], h.ws.csrf);
    let logout = h
        .send(request(
            "/api/v1/logout",
            Some(format!("csrf={}", h.ws.csrf)),
            Some(session_cookie),
        ))
        .await;
    assert_eq!(logout.status(), StatusCode::OK);
    let cookie = logout.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains(&format!("Path={session_path};")));
    assert!(cookie.ends_with("Max-Age=0"));
    let expired = h.send(request("/api/v1/session", None, None)).await;
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(expired.headers()[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn thumbnails_are_cached_privately_and_revalidate_the_webp() {
    let h = harness();
    let bytes = encode_image(artwork(), image::ImageFormat::Png);
    let blob = h.state.blobs.ingest_bytes(&bytes, "png").unwrap();
    let path = format!("/thumb/{}", blob.hash);
    let request = |condition: Option<&str>, authenticated| {
        let mut builder = Request::get(&path);
        if authenticated {
            builder = builder.header(header::COOKIE, format!("{}={}", auth::COOKIE, h.ws.session));
        }
        if let Some(condition) = condition {
            builder = builder.header(header::IF_NONE_MATCH, condition);
        }
        builder.body(Body::empty()).unwrap()
    };

    let missing = h.send(request(None, true)).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing.headers()[header::CACHE_CONTROL], "no-store");
    let file = h
        .state
        .blobs
        .write_thumb(&blob.hash, &bytes, 64)
        .unwrap()
        .unwrap();
    let first = h.send(request(None, true)).await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()[header::CONTENT_TYPE], "image/webp");
    assert_eq!(
        first.headers()[header::CACHE_CONTROL],
        "private, max-age=86400"
    );
    assert_eq!(first.headers()[header::VARY], "Cookie");
    let tag = first.headers()[header::ETAG].to_str().unwrap().to_owned();

    let denied = h.send(request(Some(&tag), false)).await;
    assert_eq!(denied.status(), StatusCode::SEE_OTHER);
    assert_eq!(denied.headers()[header::CACHE_CONTROL], "no-store");
    assert!(!denied.headers().contains_key(header::ETAG));

    let weak_list = format!("\"other,tag\", W/{tag}");
    for condition in [&tag, &weak_list, "*"] {
        let cached = h.send(request(Some(condition), true)).await;
        assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            cached.headers()[header::CACHE_CONTROL],
            "private, max-age=86400"
        );
        assert_eq!(cached.headers()[header::VARY], "Cookie");
        assert_eq!(cached.headers()[header::ETAG], tag);
        assert!(body_text(cached).await.is_empty());
    }

    // Regenerating a rendition can change its bytes without changing the source hash.
    std::fs::remove_file(file).unwrap();
    h.state
        .blobs
        .write_thumb(&blob.hash, &bytes, 32)
        .unwrap()
        .unwrap();
    let regenerated = h.send(request(Some(&tag), true)).await;
    assert_eq!(regenerated.status(), StatusCode::OK);
    assert_ne!(regenerated.headers()[header::ETAG], tag);
}
