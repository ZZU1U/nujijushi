//! The Telegram webhook endpoint: secret handling, auth bypass, dispatch.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use nujijushi::config::Config;
use nujijushi::state::AppState;
use nujijushi::tg::mock::MockTransport;
use nujijushi::tg::{BotMessage, BotUser, Transport, UpdateKind};
use nujijushi::web::{WebState, routes};

const SECRET_HEADER: &str = "x-telegram-bot-api-secret-token";
const UI_TOKEN: &str = "test-ui-token";

fn webhook_config() -> String {
    format!(
        r#"
[telegram]
bot_token = "1:test"
[web]
token = "{UI_TOKEN}"
[bot]
mode = "webhook"
webhook_url = "https://bot.example.com"
[access]
admins = [1]
public = true
allow_import = true
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
    std::fs::write(dir.path().join("config.toml"), webhook_config()).unwrap();
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

fn whoami_update() -> UpdateKind {
    UpdateKind::Message(BotMessage {
        chat_id: 99,
        chat_is_private: true,
        message_id: 1,
        from: Some(BotUser {
            id: 99,
            username: Some("someone".into()),
            first_name: None,
            is_bot: false,
        }),
        text: Some("/whoami".into()),
        caption: None,
        files: Vec::new(),
        media_group_id: None,
        forward_from_chat_id: None,
        forward_from_chat_title: None,
        forward_from_chat_username: None,
        forward_from_message_id: None,
    })
}

impl Harness {
    fn secret(&self) -> String {
        self.ws.webhook_secret.clone().expect("webhook secret")
    }

    async fn post(&self, secret: Option<&str>, body: &str) -> StatusCode {
        let mut request = Request::post(nujijushi::bot::WEBHOOK_PATH);
        if let Some(secret) = secret {
            request = request.header(SECRET_HEADER, secret);
        }
        routes::router(self.ws.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap()
            .status()
    }
}

#[tokio::test]
async fn a_valid_delivery_is_handled_without_the_ui_cookie() {
    let harness = harness();
    harness.mock.push_update(1, whoami_update());

    let status = harness.post(Some(&harness.secret()), "{}").await;
    assert_eq!(status, StatusCode::OK);
    let reply = harness.mock.last_text().unwrap_or_default();
    assert!(reply.contains("99"), "the handler answered, got {reply:?}");
}

#[tokio::test]
async fn a_wrong_or_missing_secret_is_refused() {
    let harness = harness();
    harness.mock.push_update(1, whoami_update());

    assert_eq!(
        harness.post(Some("not-the-secret"), "{}").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(harness.post(None, "{}").await, StatusCode::FORBIDDEN);
    assert!(
        harness.mock.texts().is_empty(),
        "nothing was dispatched for a rejected delivery"
    );
}

#[tokio::test]
async fn a_malformed_update_gets_a_400() {
    let harness = harness();
    // the mock has no queued update, so parsing fails
    let status = harness.post(Some(&harness.secret()), "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_endpoint_only_exists_in_webhook_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!("[telegram]\nbot_token = \"1:test\"\n[web]\ntoken = \"{UI_TOKEN}\"\n[bot]\nmode = \"polling\"\n"),
    )
    .unwrap();
    let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    let transport: Arc<dyn Transport> = Arc::new(MockTransport::new());
    let state = AppState::open(cfg, Some(transport)).unwrap();
    let ws = WebState::new(state).unwrap();
    assert!(ws.webhook_secret.is_none());

    let status = routes::router(ws)
        .oneshot(
            Request::post(nujijushi::bot::WEBHOOK_PATH)
                .header(SECRET_HEADER, "anything")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_ui_still_requires_its_cookie() {
    let harness = harness();
    let status = routes::router(harness.ws.clone())
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::SEE_OTHER, "redirect to /login");

    // and the webhook path does not leak the UI
    let status = routes::router(harness.ws.clone())
        .oneshot(Request::get("/posts").body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn webhook_mode_registers_the_endpoint_once() {
    let harness = harness();
    let bot = nujijushi::bot::Bot::new(&harness.state).await.unwrap();
    bot.run(true).await.unwrap();
    bot.run(true).await.unwrap();

    let (calls, secret) = harness.mock.webhook_registrations();
    assert_eq!(
        calls, 2,
        "registration is idempotent but does run per start"
    );
    assert_eq!(secret.as_deref(), Some(harness.secret().as_str()));
    let info = harness.mock.webhook_info().await.unwrap();
    assert_eq!(info.url, "https://bot.example.com/tg/webhook");
    assert_eq!(harness.mock.command_menu(None), Some(Vec::new()));
}

#[tokio::test]
async fn polling_mode_removes_a_stale_webhook() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "[telegram]\nbot_token = \"1:test\"\n[web]\ntoken = \"{UI_TOKEN}\"\n[bot]\nmode = \"polling\"\n"
        ),
    )
    .unwrap();
    let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
    let mock = Arc::new(MockTransport::new().with_webhook("https://someone-else.example/hook"));
    let transport: Arc<dyn Transport> = mock.clone();
    let state = AppState::open(cfg, Some(transport)).unwrap();

    let bot = nujijushi::bot::Bot::new(&state).await.unwrap();
    bot.run(true).await.unwrap();

    assert_eq!(
        mock.webhook_deletes(),
        1,
        "a leftover webhook would block polling"
    );
    assert!(mock.webhook_info().await.unwrap().url.is_empty());
}

#[tokio::test]
async fn metrics_are_scrapable_without_the_ui_cookie() {
    let harness = harness();
    let response = routes::router(harness.ws.clone())
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("text/plain"), "{content_type}");

    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let document = String::from_utf8_lossy(&body);
    assert!(document.contains("nujijushi_info{version="), "{document}");
    assert!(document.contains("nujijushi_posts_total"), "{document}");
}

#[tokio::test]
async fn static_assets_bypass_the_cookie_gate() {
    let harness = harness();
    for (path, kind) in [
        ("/static/app.js", "text/javascript"),
        ("/static/app.css", "text/css"),
    ] {
        let response = routes::router(harness.ws.clone())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(content_type.starts_with(kind), "{path}: {content_type}");
    }
}
