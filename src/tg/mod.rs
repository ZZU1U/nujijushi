//! Telegram publishing: one transport trait, a Bot API implementation, a mock for tests.

pub mod audit;
pub mod botapi;
pub mod mock;
pub mod policy;
pub mod publish;
pub mod teloxide_impl;
pub mod transport;

pub use botapi::{
    BotFile, BotMessage, BotUpdate, BotUser, ChatInfo, CopiedMessage, DownloadedFile, FileKind,
    MemberChange, UpdateKind, WebhookInfo,
};
pub use transport::{
    AdminStatus, ChatTarget, MediaKind, OutgoingMedia, SendOptions, SentBatch, TgError, Transport,
};

use std::sync::Arc;

use crate::config::Config;
use crate::error::Result;

/// Build the real transport when a bot token is configured.
pub fn build_transport(cfg: &Config) -> Result<Option<Arc<dyn Transport>>> {
    match cfg.bot_token.as_deref() {
        Some(token) if !token.trim().is_empty() => {
            let client = crate::http::client(cfg)?;
            Ok(Some(Arc::new(teloxide_impl::BotTransport::new(
                token.trim(),
                client,
            )?)))
        }
        _ => Ok(None),
    }
}
