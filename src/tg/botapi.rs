//! Telegram-neutral shapes for the bot interface.
//!
//! The bot logic never touches teloxide types directly: `teloxide_impl` maps
//! updates into these structs and `mock` fakes them, so every access rule and
//! command handler is testable without a network.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotUser {
    pub id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub is_bot: bool,
}

impl BotUser {
    pub fn label(&self) -> String {
        match (&self.username, &self.first_name) {
            (Some(name), _) => format!("@{name}"),
            (None, Some(name)) => name.clone(),
            _ => self.id.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Photo,
    Video,
    Animation,
    Document,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotFile {
    pub file_id: String,
    /// Bytes as reported by Telegram (0 when unknown).
    pub size: u64,
    pub kind: FileKind,
    pub mime: Option<String>,
}

impl BotFile {
    pub fn extension(&self) -> &'static str {
        match self.kind {
            FileKind::Photo => "jpg",
            FileKind::Video => "mp4",
            FileKind::Animation => "mp4",
            FileKind::Audio => "mp3",
            FileKind::Document => self
                .mime
                .as_deref()
                .and_then(ext_from_mime)
                .unwrap_or("bin"),
        }
    }

    pub fn is_video(&self) -> bool {
        matches!(self.kind, FileKind::Video | FileKind::Animation)
    }
}

fn ext_from_mime(mime: &str) -> Option<&'static str> {
    Some(match mime.split(';').next()?.trim() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotMessage {
    pub chat_id: i64,
    pub chat_is_private: bool,
    pub message_id: i32,
    pub from: Option<BotUser>,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub files: Vec<BotFile>,
    pub media_group_id: Option<String>,
    /// Original channel when this message is a forward.
    pub forward_from_chat_id: Option<i64>,
    pub forward_from_chat_title: Option<String>,
}

impl BotMessage {
    /// Text the user typed, or the caption of a media message.
    pub fn body(&self) -> Option<&str> {
        self.text.as_deref().or(self.caption.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInfo {
    pub id: i64,
    pub title: Option<String>,
    pub username: Option<String>,
    pub is_channel: bool,
    pub is_private: bool,
}

impl ChatInfo {
    pub fn label(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.username.clone().map(|u| format!("@{u}")))
            .unwrap_or_else(|| self.id.to_string())
    }
}

/// The bot's own membership changed in a chat (`my_chat_member`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberChange {
    pub chat_id: i64,
    pub title: String,
    pub username: Option<String>,
    pub is_channel: bool,
    pub became_admin: bool,
    pub by_user: Option<BotUser>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateKind {
    Message(BotMessage),
    MemberChange(MemberChange),
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotUpdate {
    pub id: u32,
    pub kind: UpdateKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedMessage {
    pub chat_id: i64,
    pub message_id: i32,
    pub media_group_id: Option<String>,
    pub caption: Option<String>,
    pub files: Vec<BotFile>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebhookInfo {
    pub url: String,
    /// Updates Telegram still has queued for delivery.
    pub pending_update_count: u32,
    /// Human readable reason of the most recent failed delivery.
    pub last_error_message: Option<String>,
    /// Unix seconds of that failure.
    pub last_error_date: Option<i64>,
}

impl WebhookInfo {
    pub fn is_set(&self) -> bool {
        !self.url.trim().is_empty()
    }

    /// Why Telegram cannot deliver, in one line (`None` when delivery is fine).
    pub fn delivery_problem(&self, now: i64) -> Option<String> {
        let message = self.last_error_message.as_deref()?.trim();
        if message.is_empty() {
            return None;
        }
        Some(match self.last_error_date {
            Some(ts) => format!("{message} ({} ago)", human_age(now.saturating_sub(ts))),
            None => message.to_string(),
        })
    }
}

/// Coarse age label: "a moment", "7 min", "3 h", "2 d".
pub fn human_age(secs: i64) -> String {
    match secs.max(0) {
        0..=59 => "a moment".to_string(),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// A file handed to the importer after downloading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedFile {
    pub path: PathBuf,
    pub is_video: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_follow_the_file_kind() {
        let photo = BotFile {
            file_id: "a".into(),
            size: 1,
            kind: FileKind::Photo,
            mime: None,
        };
        assert_eq!(photo.extension(), "jpg");
        assert!(!photo.is_video());

        let video = BotFile {
            kind: FileKind::Video,
            mime: None,
            ..photo.clone()
        };
        assert_eq!(video.extension(), "mp4");
        assert!(video.is_video());

        let doc = BotFile {
            kind: FileKind::Document,
            mime: Some("image/png".into()),
            ..photo.clone()
        };
        assert_eq!(doc.extension(), "png");

        let unknown = BotFile {
            kind: FileKind::Document,
            mime: Some("application/zip".into()),
            ..photo.clone()
        };
        assert_eq!(unknown.extension(), "bin");
    }

    #[test]
    fn message_body_prefers_text() {
        let message = BotMessage {
            chat_id: 1,
            chat_is_private: true,
            message_id: 2,
            from: None,
            text: Some("hello".into()),
            caption: Some("caption".into()),
            files: vec![],
            media_group_id: None,
            forward_from_chat_id: None,
            forward_from_chat_title: None,
        };
        assert_eq!(message.body(), Some("hello"));
    }

    #[test]
    fn webhook_detection() {
        let set = WebhookInfo {
            url: "https://x/hook".into(),
            ..Default::default()
        };
        assert!(set.is_set());
        assert!(!WebhookInfo::default().is_set());
    }

    #[test]
    fn webhook_reports_the_last_delivery_failure() {
        let healthy = WebhookInfo {
            url: "https://x/hook".into(),
            pending_update_count: 0,
            ..Default::default()
        };
        assert_eq!(healthy.delivery_problem(1_000), None);

        let broken = WebhookInfo {
            url: "https://x/hook".into(),
            pending_update_count: 4,
            last_error_message: Some("Wrong response from the webhook: 502 Bad Gateway".into()),
            last_error_date: Some(1_000 - 180),
        };
        assert_eq!(
            broken.delivery_problem(1_000).as_deref(),
            Some("Wrong response from the webhook: 502 Bad Gateway (3 min ago)")
        );

        // a blank error is no error, and a missing date keeps the bare message
        let blank = WebhookInfo {
            last_error_message: Some("   ".into()),
            last_error_date: Some(1),
            ..Default::default()
        };
        assert_eq!(blank.delivery_problem(1_000), None);
        let undated = WebhookInfo {
            last_error_message: Some("timeout".into()),
            ..Default::default()
        };
        assert_eq!(undated.delivery_problem(1_000).as_deref(), Some("timeout"));
    }

    #[test]
    fn human_age_is_coarse() {
        assert_eq!(human_age(0), "a moment");
        assert_eq!(human_age(59), "a moment");
        assert_eq!(human_age(60), "1 min");
        assert_eq!(human_age(3_600), "1 h");
        assert_eq!(human_age(90_000), "1 d");
        assert_eq!(human_age(-5), "a moment");
    }

    #[test]
    fn user_label() {
        let user = BotUser {
            id: 5,
            username: Some("someone".into()),
            first_name: None,
            is_bot: false,
        };
        assert_eq!(user.label(), "@someone");
    }
}
