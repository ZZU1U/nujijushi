//! Embedded React admin and HTTP endpoints over the same services as the CLI.

mod admin;
pub mod auth;
mod curation;
mod data;
pub mod forms;
mod media_merges;
mod queue_status;
pub mod routes;
mod setup;
mod tag_suggestions;

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

pub struct WebState {
    pub app: AppState,
    /// Expected session cookie value (derived from the ui token).
    pub session: String,
    /// CSRF token embedded in every form.
    pub csrf: String,
    /// Mark session cookies `Secure` (set when a TLS proxy serves the UI).
    pub secure_cookies: bool,
    /// Expected `X-Telegram-Bot-Api-Secret-Token`; `None` unless webhook mode is on.
    pub webhook_secret: Option<String>,
}

impl WebState {
    pub fn new(app: AppState) -> Result<Arc<Self>> {
        let token = app
            .cfg
            .ui_token
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| {
                Error::config("no UI token: set [web] token in config.toml or NUJIJUSHI_UI_TOKEN")
            })?;
        let session = auth::derive_at(token, "session", &app.cfg.web_base_path);
        let csrf = auth::derive_at(token, "csrf", &app.cfg.web_base_path);
        let webhook_secret = if app.cfg.bot_mode == crate::config::BotMode::Webhook {
            Some(crate::bot::webhook_secret(&app)?)
        } else {
            None
        };
        let secure_cookies = app.cfg.secure_cookies;
        Ok(Arc::new(Self {
            app,
            session,
            csrf,
            secure_cookies,
            webhook_secret,
        }))
    }

    pub(super) fn base_path(&self) -> &str {
        &self.app.cfg.web_base_path
    }

    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_path())
    }
}

pub async fn serve(state: AppState, bind: Option<String>) -> Result<()> {
    let ws = WebState::new(state)?;
    let addr = bind.unwrap_or_else(|| ws.app.cfg.bind.clone());
    let socket: std::net::SocketAddr = addr
        .parse()
        .map_err(|_| Error::invalid(format!("invalid bind address: {addr}")))?;
    if !socket.ip().is_loopback() {
        tracing::warn!(
            "admin UI binds to {} - you are exposing it beyond this machine",
            socket.ip()
        );
    }
    let listener = tokio::net::TcpListener::bind(socket)
        .await
        .map_err(|e| Error::other(format!("cannot bind {socket}: {e}")))?;
    if admin::assets_available() {
        println!("admin UI   http://{socket}{}", ws.url("/"));
    } else {
        tracing::info!("admin UI assets missing; HTTP service listening on {socket}");
    }
    if ws.webhook_secret.is_some() {
        let app = ws.app.clone();
        tokio::spawn(async move { crate::bot::menus::initialize(&app).await });
        match crate::bot::webhook_target(&ws.app.cfg) {
            Ok(url) => println!("webhook    {url}  (Telegram posts updates here)"),
            Err(err) => println!("webhook    misconfigured: {err}"),
        }
    }
    if admin::assets_available() {
        println!(
            "log in with the ui token from {}",
            ws.app.cfg.home.join("config.toml").display()
        );
    }

    // the UI can queue imports, so it also runs the import worker
    let worker = crate::jobs::worker::JobWorker::new(ws.app.clone());
    tokio::spawn(async move {
        if let Err(err) = worker.run(std::future::pending::<()>()).await {
            tracing::error!("import worker stopped: {err}");
        }
    });
    axum::serve(listener, routes::router(ws))
        .await
        .map_err(|e| Error::other(format!("web server stopped: {e}")))?;
    Ok(())
}
