//! Bot API transport (teloxide). All network-specific quirks live here.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types as tt;
use teloxide::types::{
    AllowedUpdate, BotCommand, ChatId, ChatMemberKind, FileId, InputFile, InputMedia,
    InputMediaPhoto, InputMediaVideo, Message, MessageId, ParseMode, Recipient, UserId,
};
use teloxide::{Bot, RequestError};

use super::botapi::{
    BotFile, BotMessage, BotUpdate, BotUser, ChatInfo, CopiedMessage, FileKind, MemberChange,
    UpdateKind as BotUpdateKind, WebhookInfo,
};
use super::transport::{
    AdminStatus, BotCommands, MediaKind, OutgoingMedia, SendOptions, SentBatch, TgError, TgResult,
    Transport,
};
use crate::error::Error;

const BOT_UPDATES: [AllowedUpdate; 5] = [
    AllowedUpdate::Message,
    AllowedUpdate::EditedMessage,
    AllowedUpdate::ChannelPost,
    AllowedUpdate::EditedChannelPost,
    AllowedUpdate::MyChatMember,
];

pub struct BotTransport {
    bot: Bot,
    identity: tokio::sync::OnceCell<(String, UserId)>,
    command_menus: tokio::sync::Mutex<HashMap<Option<i64>, BotCommands>>,
}

impl BotTransport {
    /// `client` carries the proxy configuration, so every Bot API call goes
    /// through it.
    pub fn new(token: &str, client: reqwest::Client) -> crate::error::Result<Self> {
        let token = token.trim();
        if token.is_empty() {
            return Err(Error::config("empty bot token"));
        }
        Ok(Self {
            bot: Bot::with_client(token, client),
            identity: tokio::sync::OnceCell::new(),
            command_menus: tokio::sync::Mutex::default(),
        })
    }

    async fn identity(&self) -> TgResult<(String, UserId)> {
        self.identity
            .get_or_try_init(|| async {
                let me = self.bot.get_me().await.map_err(map_error)?;
                let name = me
                    .user
                    .username
                    .clone()
                    .unwrap_or_else(|| me.user.id.to_string());
                Ok::<(String, UserId), TgError>((format!("@{name}"), me.user.id))
            })
            .await
            .cloned()
    }

    async fn send_one(
        &self,
        recip: Recipient,
        item: OutgoingMedia,
        opts: SendOptions,
    ) -> TgResult<Message> {
        match item.kind {
            MediaKind::Photo => {
                let mut req = self.bot.send_photo(recip, InputFile::file(&item.path));
                if let Some(caption) = item.caption.clone() {
                    req = req.caption(caption).parse_mode(ParseMode::Html);
                }
                req.has_spoiler(item.spoiler)
                    .disable_notification(opts.silent)
                    .protect_content(opts.protect)
                    .await
                    .map_err(map_send_error)
            }
            MediaKind::Video => {
                let mut req = self.bot.send_video(recip, InputFile::file(&item.path));
                if let Some(thumb) = item.thumb.clone() {
                    req = req.thumbnail(InputFile::file(thumb));
                }
                if let Some(caption) = item.caption.clone() {
                    req = req.caption(caption).parse_mode(ParseMode::Html);
                }
                req.has_spoiler(item.spoiler)
                    .supports_streaming(true)
                    .disable_notification(opts.silent)
                    .protect_content(opts.protect)
                    .await
                    .map_err(map_send_error)
            }
        }
    }
}

fn to_input_media(item: OutgoingMedia) -> InputMedia {
    match item.kind {
        MediaKind::Photo => {
            let mut photo = InputMediaPhoto::new(InputFile::file(&item.path));
            photo.caption = item.caption;
            photo.parse_mode = Some(ParseMode::Html);
            photo.has_spoiler = item.spoiler;
            InputMedia::Photo(photo)
        }
        MediaKind::Video => {
            let mut video = InputMediaVideo::new(InputFile::file(&item.path));
            video.caption = item.caption;
            video.parse_mode = Some(ParseMode::Html);
            video.has_spoiler = item.spoiler;
            video.supports_streaming = Some(true);
            video.thumbnail = item.thumb.as_ref().map(InputFile::file);
            InputMedia::Video(video)
        }
    }
}

fn recipient(chat: &str) -> TgResult<Recipient> {
    let trimmed = chat.trim();
    if trimmed.is_empty() {
        return Err(TgError::Permanent("empty chat id".to_string()));
    }
    if let Ok(id) = trimmed.parse::<i64>() {
        return Ok(Recipient::Id(ChatId(id)));
    }
    let username = trimmed.trim_start_matches('@');
    if username.is_empty() || username.contains(char::is_whitespace) {
        return Err(TgError::Permanent(format!("invalid chat id: {chat}")));
    }
    Ok(Recipient::ChannelUsername(format!("@{username}")))
}

fn map_user(user: &tt::User) -> BotUser {
    BotUser {
        id: user.id.0 as i64,
        username: user.username.clone(),
        first_name: Some(user.first_name.clone()),
        is_bot: user.is_bot,
    }
}

fn extract_files(message: &tt::Message) -> Vec<BotFile> {
    let mut files = Vec::new();
    if let Some(largest) = message.photo().and_then(|sizes| sizes.last()) {
        files.push(BotFile {
            file_id: largest.file.id.0.clone(),
            size: largest.file.size as u64,
            kind: FileKind::Photo,
            mime: Some("image/jpeg".to_string()),
        });
    }
    if let Some(video) = message.video() {
        files.push(BotFile {
            file_id: video.file.id.0.clone(),
            size: video.file.size as u64,
            kind: FileKind::Video,
            mime: video.mime_type.as_ref().map(|m| m.to_string()),
        });
    }
    if let Some(animation) = message.animation() {
        files.push(BotFile {
            file_id: animation.file.id.0.clone(),
            size: animation.file.size as u64,
            kind: FileKind::Animation,
            mime: animation.mime_type.as_ref().map(|m| m.to_string()),
        });
    }
    if let Some(document) = message.document() {
        files.push(BotFile {
            file_id: document.file.id.0.clone(),
            size: document.file.size as u64,
            kind: FileKind::Document,
            mime: document.mime_type.as_ref().map(|m| m.to_string()),
        });
    }
    if let Some(audio) = message.audio() {
        files.push(BotFile {
            file_id: audio.file.id.0.clone(),
            size: audio.file.size as u64,
            kind: FileKind::Audio,
            mime: audio.mime_type.as_ref().map(|m| m.to_string()),
        });
    }
    files
}

fn map_message(message: &tt::Message) -> BotMessage {
    let (forward_chat, forward_message) = match message.forward_origin() {
        Some(tt::MessageOrigin::Channel {
            chat, message_id, ..
        }) => (Some(chat), Some(message_id.0)),
        Some(tt::MessageOrigin::Chat { sender_chat, .. }) => (Some(sender_chat), None),
        _ => (None, None),
    };
    BotMessage {
        chat_id: message.chat.id.0,
        chat_is_private: message.chat.is_private(),
        message_id: message.id.0,
        from: message.from.as_ref().map(map_user),
        text: message.text().map(str::to_string),
        caption: message.caption().map(str::to_string),
        files: extract_files(message),
        media_group_id: message.media_group_id().map(|group| group.0.clone()),
        forward_from_chat_id: forward_chat.map(|chat| chat.id.0),
        forward_from_chat_title: forward_chat.and_then(|chat| chat.title().map(str::to_string)),
        forward_from_chat_username: forward_chat
            .and_then(|chat| chat.username().map(str::to_string)),
        forward_from_message_id: forward_message,
    }
}

fn map_member_change(updated: &tt::ChatMemberUpdated) -> MemberChange {
    MemberChange {
        chat_id: updated.chat.id.0,
        title: updated.chat.title().unwrap_or_default().to_string(),
        username: updated.chat.username().map(str::to_string),
        is_channel: updated.chat.is_channel(),
        became_admin: matches!(
            updated.new_chat_member.kind,
            ChatMemberKind::Administrator(_) | ChatMemberKind::Owner(_)
        ),
        by_user: Some(map_user(&updated.from)),
    }
}

fn map_update(update: tt::Update) -> BotUpdate {
    let id = update.id.0;
    let kind = match update.kind {
        tt::UpdateKind::Message(message)
        | tt::UpdateKind::ChannelPost(message)
        | tt::UpdateKind::EditedMessage(message)
        | tt::UpdateKind::EditedChannelPost(message) => {
            BotUpdateKind::Message(map_message(&message))
        }
        tt::UpdateKind::MyChatMember(updated) => {
            BotUpdateKind::MemberChange(map_member_change(&updated))
        }
        _ => BotUpdateKind::Other,
    };
    BotUpdate { id, kind }
}

fn map_error(err: RequestError) -> TgError {
    match &err {
        RequestError::RetryAfter(secs) => {
            TgError::classify(&format!("retry after {}", secs.seconds()))
        }
        RequestError::Network(_) | RequestError::Io(_) | RequestError::InvalidJson { .. } => {
            TgError::Transient(err.to_string())
        }
        RequestError::MigrateToChatId(_) => TgError::Permanent(err.to_string()),
        RequestError::Api(_) => TgError::classify(&err.to_string()),
    }
}

/// A send that fails at the network/IO level may already have been accepted by
/// Telegram. Retrying it could duplicate the message, so it is *uncertain*, not
/// transient. Read-only calls keep the retryable `map_error` classification.
fn map_send_error(err: RequestError) -> TgError {
    match &err {
        RequestError::Network(_) | RequestError::Io(_) | RequestError::InvalidJson { .. } => {
            TgError::Uncertain(err.to_string())
        }
        _ => map_error(err),
    }
}

#[async_trait]
impl Transport for BotTransport {
    fn label(&self) -> String {
        "telegram bot api".to_string()
    }

    async fn me(&self) -> TgResult<String> {
        Ok(self.identity().await?.0)
    }

    async fn check_chat(&self, chat: &str) -> TgResult<AdminStatus> {
        let (_, user_id) = self.identity().await?;
        let recip = recipient(chat)?;
        let member = self
            .bot
            .get_chat_member(recip, user_id)
            .await
            .map_err(map_error)?;
        let status = match &member.kind {
            ChatMemberKind::Owner(_) => AdminStatus {
                title: None,
                kind: "creator".into(),
                is_admin: true,
                can_post: true,
            },
            ChatMemberKind::Administrator(admin) => AdminStatus {
                title: admin.custom_title.clone(),
                kind: "administrator".into(),
                is_admin: true,
                can_post: admin.can_post_messages,
            },
            ChatMemberKind::Member(_) => AdminStatus {
                title: None,
                kind: "member".into(),
                is_admin: false,
                can_post: false,
            },
            ChatMemberKind::Restricted(restricted) => AdminStatus {
                title: None,
                kind: "restricted".into(),
                is_admin: false,
                can_post: restricted.can_send_messages,
            },
            ChatMemberKind::Left => AdminStatus {
                title: None,
                kind: "left".into(),
                is_admin: false,
                can_post: false,
            },
            ChatMemberKind::Banned(_) => AdminStatus {
                title: None,
                kind: "banned".into(),
                is_admin: false,
                can_post: false,
            },
        };
        Ok(status)
    }

    async fn send_batch(
        &self,
        chat: &str,
        media: Vec<OutgoingMedia>,
        opts: SendOptions,
    ) -> TgResult<SentBatch> {
        if media.is_empty() {
            return Err(TgError::Permanent("nothing to send".to_string()));
        }
        let recip = recipient(chat)?;
        if media.len() == 1 {
            let item = media.into_iter().next().expect("checked non-empty");
            let message = self.send_one(recip, item, opts).await?;
            return Ok(SentBatch {
                message_ids: vec![message.id.0],
                chat_id: Some(message.chat.id.0),
                chat_username: message.chat.username().map(str::to_string),
            });
        }
        let inputs: Vec<InputMedia> = media.into_iter().map(to_input_media).collect();
        let messages = self
            .bot
            .send_media_group(recip, inputs)
            .disable_notification(opts.silent)
            .protect_content(opts.protect)
            .await
            .map_err(map_send_error)?;
        let chat = messages.first().map(|message| &message.chat);
        Ok(SentBatch {
            chat_id: chat.map(|chat| chat.id.0),
            chat_username: chat.and_then(|chat| chat.username().map(str::to_string)),
            message_ids: messages.into_iter().map(|m| m.id.0).collect(),
        })
    }

    async fn send_text(&self, chat: &str, html: &str) -> TgResult<()> {
        let recip = recipient(chat)?;
        self.bot
            .send_message(recip, crate::render::truncate::shorten_html(html, 4096))
            .parse_mode(ParseMode::Html)
            .link_preview_options(tt::LinkPreviewOptions {
                is_disabled: true,
                url: None,
                prefer_small_media: false,
                prefer_large_media: false,
                show_above_text: false,
            })
            .await
            .map_err(map_error)?;
        Ok(())
    }

    async fn get_updates(&self, offset: i32, timeout: u32) -> TgResult<Vec<BotUpdate>> {
        let updates = self
            .bot
            .get_updates()
            .offset(offset)
            .timeout(timeout.clamp(1, 120))
            .limit(100)
            .allowed_updates(BOT_UPDATES)
            .await
            .map_err(map_error)?;
        Ok(updates.into_iter().map(map_update).collect())
    }

    async fn get_chat(&self, chat: &str) -> TgResult<ChatInfo> {
        let recip = recipient(chat)?;
        let info = self.bot.get_chat(recip).await.map_err(map_error)?;
        Ok(ChatInfo {
            id: info.id.0,
            title: info.title().map(str::to_string),
            username: info.username().map(str::to_string),
            is_channel: info.is_channel(),
            is_private: info.is_private(),
        })
    }

    async fn copy_message(&self, to: &str, from: &str, message_id: i32) -> TgResult<CopiedMessage> {
        let to = recipient(to)?;
        let from = recipient(from)?;
        // copyMessage only returns the new id, so forward instead: we need the
        // media file ids, and the copy is deleted right after downloading.
        let message = self
            .bot
            .forward_message(to, from, MessageId(message_id))
            .await
            .map_err(map_error)?;
        Ok(CopiedMessage {
            chat_id: message.chat.id.0,
            message_id: message.id.0,
            media_group_id: message.media_group_id().map(|group| group.0.clone()),
            caption: message.caption().map(str::to_string),
            files: extract_files(&message),
        })
    }

    async fn download_file(&self, file_id: &str, dest: &std::path::Path) -> TgResult<u64> {
        let file = self
            .bot
            .get_file(FileId(file_id.to_string()))
            .await
            .map_err(map_error)?;
        if let Some(parent) = dest.parent()
            && let Err(err) = tokio::fs::create_dir_all(parent).await
        {
            return Err(TgError::Transient(format!(
                "cannot create {}: {err}",
                parent.display()
            )));
        }
        let mut out = tokio::fs::File::create(dest)
            .await
            .map_err(|err| TgError::Transient(format!("cannot write {}: {err}", dest.display())))?;
        self.bot
            .download_file(&file.path, &mut out)
            .await
            .map_err(|err| TgError::Transient(format!("download failed: {err}")))?;
        Ok(tokio::fs::metadata(dest)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0))
    }

    async fn delete_message(&self, chat: &str, message_id: i32) -> TgResult<()> {
        let recip = recipient(chat)?;
        self.bot
            .delete_message(recip, MessageId(message_id))
            .await
            .map_err(map_error)?;
        Ok(())
    }

    async fn set_commands(&self, chat_id: Option<i64>, commands: BotCommands) -> TgResult<()> {
        let mut menus = self.command_menus.lock().await;
        if menus.get(&chat_id) == Some(&commands) {
            return Ok(());
        }
        let payload: Vec<BotCommand> = commands
            .iter()
            .map(|(command, description)| BotCommand::new(*command, *description))
            .collect();
        let scope = match chat_id {
            Some(id) => tt::BotCommandScope::Chat {
                chat_id: ChatId(id).into(),
            },
            None => tt::BotCommandScope::Default,
        };
        let request = self.bot.set_my_commands(payload).scope(scope);
        tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .map_err(|_| TgError::Transient("command menu update timed out".into()))?
            .map_err(map_error)?;
        menus.insert(chat_id, commands);
        Ok(())
    }

    async fn webhook_info(&self) -> TgResult<WebhookInfo> {
        let info = self.bot.get_webhook_info().await.map_err(map_error)?;
        Ok(WebhookInfo {
            url: info.url.map(|url| url.to_string()).unwrap_or_default(),
            pending_update_count: info.pending_update_count,
            last_error_message: info.last_error_message,
            last_error_date: info.last_error_date.map(|date| date.timestamp()),
        })
    }

    async fn set_webhook(&self, url: &str, secret: &str) -> TgResult<()> {
        let parsed = url
            .parse()
            .map_err(|_| TgError::Permanent(format!("invalid webhook url: {url}")))?;
        self.bot
            .set_webhook(parsed)
            .secret_token(secret.to_string())
            .allowed_updates(BOT_UPDATES)
            .await
            .map_err(map_error)?;
        Ok(())
    }

    async fn delete_webhook(&self) -> TgResult<()> {
        self.bot.delete_webhook().await.map_err(map_error)?;
        Ok(())
    }

    fn parse_update(&self, body: &[u8]) -> TgResult<BotUpdate> {
        let update: tt::Update = serde_json::from_slice(body)
            .map_err(|err| TgError::Permanent(format!("invalid update json: {err}")))?;
        Ok(map_update(update))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chat_targets() {
        assert_eq!(
            recipient("-1001234567890").unwrap(),
            Recipient::Id(ChatId(-1001234567890))
        );
        assert_eq!(
            recipient("@my_channel").unwrap(),
            Recipient::ChannelUsername("@my_channel".into())
        );
        assert_eq!(
            recipient("my_channel").unwrap(),
            Recipient::ChannelUsername("@my_channel".into())
        );
        assert!(recipient("  ").is_err());
        assert!(recipient("@bad name").is_err());
    }

    #[tokio::test]
    async fn sends_full_handle_and_subscribes_to_membership_updates() {
        use axum::{Json, Router, body::Bytes, extract::Path, routing::post};
        use serde_json::{Value, json};

        fn assert_updates(allowed: &Value) {
            let allowed = allowed.as_array().unwrap();
            assert!(allowed.contains(&json!("message")));
            assert!(allowed.contains(&json!("my_chat_member")));
        }

        async fn telegram_api(Path(method): Path<String>, body: Bytes) -> Json<Value> {
            if method.eq_ignore_ascii_case("setwebhook") {
                let body = std::str::from_utf8(&body).unwrap();
                let field = |name: &str| {
                    body.split_once(&format!("name=\"{name}\""))
                        .and_then(|(_, part)| part.split_once("\r\n\r\n"))
                        .and_then(|(_, value)| value.split_once("\r\n"))
                        .unwrap()
                        .0
                };
                assert_updates(&serde_json::from_str(field("allowed_updates")).unwrap());
                assert_eq!(field("secret_token"), "smoke-test-secret");
                return Json(json!({"ok": true, "result": true}));
            }
            let body: Value = serde_json::from_slice(&body).unwrap();
            let channel = json!({"id": -1001234567890i64, "type": "channel", "title": "Art", "username": "art_channel"});
            let result = match method.to_ascii_lowercase().as_str() {
                "setmycommands" => {
                    if body["scope"]["type"] == "default" {
                        assert_eq!(body["commands"], json!([]));
                    } else {
                        assert_eq!(body["scope"], json!({"type": "chat", "chat_id": 99}));
                        assert_eq!(
                            body["commands"],
                            json!([{"command": "import", "description": "archive a link"}])
                        );
                    }
                    json!(true)
                }
                "getchat" => {
                    assert_eq!(body["chat_id"], "@art_channel");
                    let mut full = channel;
                    full["max_reaction_count"] = json!(1);
                    full["accepted_gift_types"] = json!({
                        "unlimited_gifts": false, "limited_gifts": false,
                        "unique_gifts": false, "premium_subscription": false,
                    });
                    full
                }
                "getupdates" => {
                    assert_updates(&body["allowed_updates"]);
                    let bot = json!({"id": 1, "is_bot": true, "first_name": "Test Bot"});
                    json!([{
                            "update_id": 10,
                            "my_chat_member": {
                                "chat": channel,
                                "from": {"id": 2, "is_bot": false, "first_name": "Admin"},
                                "date": 1700000000,
                                "old_chat_member": {"user": bot, "status": "left"},
                                "new_chat_member": {
                                    "user": bot, "status": "administrator", "is_anonymous": false,
                                    "can_be_edited": false, "can_manage_chat": true, "can_change_info": false,
                                    "can_post_messages": true, "can_delete_messages": true, "can_manage_video_chats": false,
                                    "can_invite_users": false, "can_restrict_members": false, "can_promote_members": false
                                }
                            }
                    }])
                }
                other => panic!("unexpected Telegram method {other}"),
            };
            Json(json!({"ok": true, "result": result}))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let routes = Router::new().route("/bot1:test/{method}", post(telegram_api));
            axum::serve(listener, routes).await.unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let cfg = crate::config::Config::load_from_home(home.path().to_path_buf()).unwrap();
        let client = crate::http::client(&cfg).unwrap();
        let mut transport = BotTransport::new("1:test", client).unwrap();
        transport.bot = transport
            .bot
            .set_api_url(format!("http://{address}").parse().unwrap());

        let chat = transport.get_chat("@art_channel").await.unwrap();
        assert!(chat.is_channel);
        assert_eq!(chat.id, -1001234567890);

        transport.set_commands(None, Vec::new()).await.unwrap();
        transport
            .set_commands(Some(99), vec![("import", "archive a link")])
            .await
            .unwrap();

        let updates = transport.get_updates(0, 1).await.unwrap();
        let BotUpdateKind::MemberChange(change) = &updates[0].kind else {
            panic!("expected a membership update");
        };
        assert!(change.is_channel && change.became_admin);
        assert_eq!(change.chat_id, chat.id);
        transport
            .set_webhook("https://example.test/tg/webhook", "smoke-test-secret")
            .await
            .unwrap();
        server.abort();
    }
}
