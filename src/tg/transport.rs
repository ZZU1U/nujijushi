//! Transport abstraction so publishing can be tested without the network.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::botapi::{BotUpdate, ChatInfo, CopiedMessage, WebhookInfo};

/// Result of a transport call (kept separate from the crate error type).
pub type TgResult<T> = std::result::Result<T, TgError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutgoingMedia {
    /// Absolute path of the local blob file.
    pub path: PathBuf,
    pub kind: MediaKind,
    /// Only ever set on the first item of a batch (Telegram renders it as the album caption).
    pub caption: Option<String>,
    pub spoiler: bool,
    /// Video poster frame, sent as `thumbnail` so chats show a preview.
    pub thumb: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SendOptions {
    pub silent: bool,
    pub protect: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SentBatch {
    pub message_ids: Vec<i32>,
}

impl SentBatch {
    pub fn ids_csv(&self) -> String {
        self.message_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdminStatus {
    pub title: Option<String>,
    pub kind: String,
    pub is_admin: bool,
    pub can_post: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatTarget<'a> {
    /// Numeric id or `@username`.
    Raw(&'a str),
}

impl ChatTarget<'_> {
    pub fn as_str(&self) -> &str {
        match self {
            ChatTarget::Raw(s) => s,
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum TgError {
    #[error("rate limited (retry after {retry_after}s)")]
    RateLimited { retry_after: u64 },
    #[error("transient failure: {0}")]
    Transient(String),
    #[error("permanent failure: {0}")]
    Permanent(String),
}

impl TgError {
    /// Retrying is worth it for throttling and transient faults only.
    pub fn is_retryable(&self) -> bool {
        matches!(self, TgError::RateLimited { .. } | TgError::Transient(_))
    }

    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            TgError::RateLimited { retry_after } => Some(*retry_after),
            _ => None,
        }
    }

    /// Classify a raw API error message the way the Bot API reports it.
    pub fn classify(message: &str) -> TgError {
        let lowered = message.to_ascii_lowercase();
        if let Some(rest) = lowered.split("retry after").nth(1) {
            let secs = rest
                .trim()
                .trim_start_matches(|c: char| !c.is_ascii_digit())
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5);
            return TgError::RateLimited { retry_after: secs };
        }
        if lowered.contains("too many requests") {
            return TgError::RateLimited { retry_after: 5 };
        }
        // Chat-side misconfiguration: retrying will not help.
        let permanent_markers = [
            "chat not found",
            "chat_id is empty",
            "not enough rights",
            "have no rights",
            "bot is not a member",
            "chat_write_forbidden",
            "forbidden",
            "unauthorized",
            "wrong file identifier",
            "file is too big",
            "photo_invalid_dimensions",
            "webhook",
            "message is too long",
            "can't parse entities",
            "unsupported media",
            "chat is not a channel",
            "peer_id_invalid",
        ];
        if permanent_markers.iter().any(|m| lowered.contains(m)) {
            return TgError::Permanent(message.to_string());
        }
        TgError::Transient(message.to_string())
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    fn label(&self) -> String;

    /// Bot identity, used by `doctor`.
    async fn me(&self) -> TgResult<String>;

    /// Rights of the bot in a chat, used by `doctor`.
    async fn check_chat(&self, chat: &str) -> TgResult<AdminStatus>;

    /// 1 item = single message, 2..=10 items = album.
    async fn send_batch(
        &self,
        chat: &str,
        media: Vec<OutgoingMedia>,
        opts: SendOptions,
    ) -> TgResult<SentBatch>;

    // --- bot interface -------------------------------------------------

    /// Plain HTML text message (used for every bot reply).
    async fn send_text(&self, chat: &str, html: &str) -> TgResult<()>;

    /// Long-poll for updates (`timeout` in seconds).
    async fn get_updates(&self, offset: i32, timeout: u32) -> TgResult<Vec<BotUpdate>>;

    /// Metadata of a chat the bot can see; validates `chat` ids.
    async fn get_chat(&self, chat: &str) -> TgResult<ChatInfo>;

    /// Copy one channel post into `to` so its media can be downloaded.
    async fn copy_message(&self, to: &str, from: &str, message_id: i32) -> TgResult<CopiedMessage>;

    /// Download a file by id into `dest`, returning the number of bytes written.
    async fn download_file(&self, file_id: &str, dest: &Path) -> TgResult<u64>;

    async fn delete_message(&self, chat: &str, message_id: i32) -> TgResult<()>;

    /// Publish the command menu shown in Telegram clients.
    async fn set_commands(&self) -> TgResult<()>;

    /// A webhook blocks long polling; `doctor` reports it.
    async fn webhook_info(&self) -> TgResult<WebhookInfo>;

    /// Register the endpoint Telegram should POST updates to.
    async fn set_webhook(&self, url: &str, secret: &str) -> TgResult<()>;

    /// Remove any webhook (polling needs this, and webhook mode cleans up after a bad switch).
    async fn delete_webhook(&self) -> TgResult<()>;

    /// Parse a webhook JSON body. Polling hands us typed updates already.
    fn parse_update(&self, body: &[u8]) -> TgResult<BotUpdate>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rate_limits() {
        let err = TgError::classify("Too Many Requests: retry after 27");
        assert_eq!(err, TgError::RateLimited { retry_after: 27 });
        assert!(err.is_retryable());
        assert_eq!(err.retry_after_secs(), Some(27));
    }

    #[test]
    fn classifies_permanent_errors() {
        assert!(matches!(
            TgError::classify("Bad Request: not enough rights to send photos to the chat"),
            TgError::Permanent(_)
        ));
        assert!(matches!(
            TgError::classify("Bad Request: chat not found"),
            TgError::Permanent(_)
        ));
        assert!(!TgError::classify("Bad Request: chat not found").is_retryable());
    }

    #[test]
    fn network_blips_are_transient() {
        let err = TgError::classify("error sending request: connection reset");
        assert!(matches!(err, TgError::Transient(_)));
        assert!(err.is_retryable());
    }

    #[test]
    fn batch_ids() {
        let batch = SentBatch {
            message_ids: vec![1, 2, 3],
        };
        assert_eq!(batch.ids_csv(), "1,2,3");
    }
}
