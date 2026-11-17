//! Telegram command interface: access control, routing and handlers.

pub mod access;
pub mod commands;
pub mod handlers;
pub mod reply;

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;
use crate::tg::Transport;

/// Persisted `getUpdates` offset (survives restarts).
pub const OFFSET_KEY: &str = "bot.offset";
/// Path Telegram POSTs to when `[bot] mode = "webhook"`.
pub const WEBHOOK_PATH: &str = "/tg/webhook";
/// Generated webhook secret, persisted when the config does not carry one.
pub const SECRET_KEY: &str = "bot.webhook_secret";

pub struct Bot<'a> {
    pub state: &'a AppState,
    pub transport: Arc<dyn Transport>,
    pub access: access::Access,
    pub username: Option<String>,
}

impl<'a> Bot<'a> {
    pub async fn new(state: &'a AppState) -> Result<Self> {
        let transport = state.require_transport()?;
        let me = transport.me().await.unwrap_or_else(|_| "@bot".to_string());
        let username = Some(me.trim_start_matches('@').to_string());
        let access = access::Access::new(state.cfg.access.clone());
        Ok(Self {
            state,
            transport,
            access,
            username,
        })
    }

    /// Start the interface in the configured mode.
    pub async fn run(&self, once: bool) -> Result<()> {
        if let Err(err) = self.transport.set_commands().await {
            tracing::warn!("cannot register the command menu: {err}");
        }
        match self.state.cfg.bot_mode {
            crate::config::BotMode::Polling => self.run_polling(once).await,
            crate::config::BotMode::Webhook => self.run_webhook(once).await,
        }
    }

    /// Register the endpoint; the web server delivers the updates from then on.
    async fn run_webhook(&self, once: bool) -> Result<()> {
        let url = webhook_target(&self.state.cfg)?;
        let secret = webhook_secret(self.state)?;
        self.transport
            .set_webhook(&url, &secret)
            .await
            .map_err(|err| crate::error::Error::Telegram(err.to_string()))?;
        tracing::info!(%url, "webhook registered");
        if once {
            println!("webhook registered: {url}");
        }
        Ok(())
    }

    /// Handle one webhook delivery (called by the admin server).
    pub async fn handle_webhook_body(&self, body: &[u8]) -> Result<()> {
        let update = self
            .transport
            .parse_update(body)
            .map_err(|err| crate::error::Error::Telegram(err.to_string()))?;
        // one line per delivery: "no reaction" must be distinguishable from
        // "Telegram never called us" when a webhook silently fails
        tracing::info!(update = update.id, "webhook update received");
        self.handle(update).await
    }

    async fn run_polling(&self, once: bool) -> Result<()> {
        // a registered webhook swallows every update we would poll for
        if let Ok(info) = self.transport.webhook_info().await
            && info.is_set()
        {
            tracing::warn!(
                url = %info.url,
                "a webhook was registered while [bot] mode = polling: deleting it"
            );
            if let Err(err) = self.transport.delete_webhook().await {
                tracing::error!("cannot delete the webhook, polling will fail: {err}");
            }
        }
        let mut offset = self.stored_offset().await;
        tracing::info!(offset, "bot polling started");

        loop {
            let updates = self
                .transport
                .get_updates(offset, self.state.cfg.bot_poll_timeout_secs)
                .await;
            match updates {
                Ok(updates) if updates.is_empty() => {
                    if once {
                        return Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                }
                Ok(updates) => {
                    for update in updates {
                        offset = update.id as i32 + 1;
                        if let Err(err) = self.handle(update).await {
                            tracing::warn!("bot update failed: {err}");
                        }
                    }
                    self.save_offset(offset).await;
                    if once {
                        return Ok(());
                    }
                }
                Err(err) => {
                    if once {
                        return Err(crate::error::Error::Telegram(err.to_string()));
                    }
                    // 409 = another getUpdates holder (a second `nujijushi run`)
                    tracing::warn!("polling failed: {err}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn stored_offset(&self) -> i32 {
        self.state
            .db
            .setting(OFFSET_KEY)
            .ok()
            .flatten()
            .and_then(|raw| raw.parse::<i32>().ok())
            .unwrap_or(0)
    }

    async fn save_offset(&self, offset: i32) {
        if let Err(err) = self.state.db.set_setting(OFFSET_KEY, &offset.to_string()) {
            tracing::warn!("cannot persist the bot offset: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tg::mock::MockTransport;

    async fn state_with(mock: MockTransport) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        let transport: Arc<dyn Transport> = Arc::new(mock);
        // keep the temp dir alive for the whole test process
        std::mem::forget(dir);
        AppState::open(cfg, Some(transport)).unwrap()
    }

    #[tokio::test]
    async fn once_returns_when_no_updates_are_queued() {
        let mock = MockTransport::new();
        let state = state_with(mock).await;
        let bot = Bot::new(&state).await.unwrap();
        bot.run(true).await.unwrap();
    }
}

/// The URL Telegram should POST to, derived from `[bot] webhook_url`.
pub fn webhook_target(cfg: &crate::config::Config) -> Result<String> {
    let base = cfg.webhook_url.as_deref().ok_or_else(|| {
        crate::error::Error::config(
            "[bot] webhook_url is required for webhook mode (the public HTTPS base URL)",
        )
    })?;
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with(WEBHOOK_PATH) {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("{trimmed}{WEBHOOK_PATH}"))
    }
}

/// Config secret, else the one generated on a previous run, else a fresh one.
pub fn webhook_secret(state: &AppState) -> Result<String> {
    if let Some(secret) = state.cfg.webhook_secret.clone() {
        return Ok(secret);
    }
    if let Some(stored) = state.db.setting(SECRET_KEY)? {
        return Ok(stored);
    }
    let secret = crate::config::random_token();
    state.db.set_setting(SECRET_KEY, &secret)?;
    tracing::info!("generated a webhook secret and stored it in the database");
    Ok(secret)
}

#[cfg(test)]
mod webhook_tests {
    use super::*;
    use crate::config::{BotMode, Config};

    fn cfg(mode: BotMode, url: Option<&str>) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        std::mem::forget(dir);
        cfg.bot_mode = mode;
        cfg.webhook_url = url.map(|url| url.to_string());
        cfg
    }

    #[test]
    fn target_is_derived_from_the_base_url() {
        assert_eq!(
            webhook_target(&cfg(BotMode::Webhook, Some("https://bot.example.com"))).unwrap(),
            "https://bot.example.com/tg/webhook"
        );
        assert_eq!(
            webhook_target(&cfg(BotMode::Webhook, Some("https://bot.example.com/"))).unwrap(),
            "https://bot.example.com/tg/webhook"
        );
        assert_eq!(
            webhook_target(&cfg(BotMode::Webhook, Some("https://x.dev/tg/webhook"))).unwrap(),
            "https://x.dev/tg/webhook"
        );
        let error = webhook_target(&cfg(BotMode::Webhook, None)).unwrap_err();
        assert!(error.to_string().contains("webhook_url"));
    }

    #[test]
    fn secret_prefers_config_then_storage() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        cfg.webhook_secret = Some("from-config".into());
        let db = crate::db::Db::open(dir.path().join("t.sqlite3")).unwrap();
        let state = AppState {
            cfg: std::sync::Arc::new(cfg),
            db: db.clone(),
            blobs: std::sync::Arc::new(
                crate::blob::BlobStore::new(dir.path().join("b"), dir.path().join("t")).unwrap(),
            ),
            renderer: std::sync::Arc::new(crate::render::Renderer::new().unwrap()),
            transport: None,
        };
        assert_eq!(webhook_secret(&state).unwrap(), "from-config");

        // without a configured secret one is generated and remembered
        let mut generated_cfg = (*state.cfg).clone();
        generated_cfg.webhook_secret = None;
        let state2 = AppState {
            cfg: std::sync::Arc::new(generated_cfg),
            ..state.clone()
        };
        let first = webhook_secret(&state2).unwrap();
        assert!(first.len() >= 16);
        assert_eq!(webhook_secret(&state2).unwrap(), first);
        assert_eq!(
            db.setting(SECRET_KEY).unwrap().as_deref(),
            Some(first.as_str())
        );
    }
}
