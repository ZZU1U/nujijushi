//! In-memory transport for tests: records every call, scripts every failure.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::botapi::{BotUpdate, ChatInfo, CopiedMessage, UpdateKind, WebhookInfo};
use super::transport::{
    AdminStatus, BotCommands, OutgoingMedia, SendOptions, SentBatch, TgError, TgResult, Transport,
};

const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 2, 8, 2, 0,
    0, 0, 253, 212, 154, 115, 0, 0, 0, 16, 73, 68, 65, 84, 120, 156, 99, 56, 97, 99, 3, 68, 12, 16,
    10, 0, 37, 62, 5, 1, 116, 153, 148, 96, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedSend {
    pub chat: String,
    pub media: Vec<OutgoingMedia>,
    pub opts: SendOptions,
}

#[derive(Clone)]
pub struct MockTransport {
    inner: Arc<Mutex<MockState>>,
}

struct MockState {
    sends: Vec<RecordedSend>,
    /// Scripted outcomes: `Some(err)` fails that call, `None` succeeds.
    script: Option<VecDeque<Option<TgError>>>,
    failures: VecDeque<TgError>,
    next_id: i32,
    admin: AdminStatus,
    texts: Vec<(String, String)>,
    text_failure: Option<TgError>,
    updates: VecDeque<BotUpdate>,
    chats: HashMap<i64, ChatInfo>,
    chat_delay: std::time::Duration,
    copies: HashMap<(String, i32), CopiedMessage>,
    copy_errors: HashMap<(String, i32), TgError>,
    files: HashMap<String, Vec<u8>>,
    downloads: Vec<(String, PathBuf)>,
    deleted: Vec<(String, i32)>,
    webhook: WebhookInfo,
    command_menus: HashMap<Option<i64>, BotCommands>,
    webhook_secret: Option<String>,
    webhook_calls: u32,
    webhook_deletes: u32,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockState {
                sends: Vec::new(),
                script: None,
                failures: VecDeque::new(),
                next_id: 1,
                admin: AdminStatus {
                    title: Some("mock channel".into()),
                    kind: "channel".into(),
                    is_admin: true,
                    can_post: true,
                },
                texts: Vec::new(),
                text_failure: None,
                updates: VecDeque::new(),
                chats: HashMap::new(),
                chat_delay: std::time::Duration::ZERO,
                copies: HashMap::new(),
                copy_errors: HashMap::new(),
                files: HashMap::new(),
                downloads: Vec::new(),
                deleted: Vec::new(),
                webhook: WebhookInfo::default(),
                command_menus: HashMap::new(),
                webhook_secret: None,
                webhook_calls: 0,
                webhook_deletes: 0,
            })),
        }
    }

    pub fn with_failures(failures: Vec<TgError>) -> Self {
        let mock = Self::new();
        mock.inner.lock().unwrap().failures = failures.into();
        mock
    }

    pub fn with_text_failure(self, failure: TgError) -> Self {
        self.inner.lock().unwrap().text_failure = Some(failure);
        self
    }

    /// Precise per-call outcomes (used to simulate a failure mid-album).
    pub fn with_script(outcomes: Vec<Option<TgError>>) -> Self {
        let mock = Self::new();
        mock.inner.lock().unwrap().script = Some(outcomes.into());
        mock
    }

    pub fn with_admin(self, admin: AdminStatus) -> Self {
        self.inner.lock().unwrap().admin = admin;
        self
    }

    pub fn with_webhook(self, url: &str) -> Self {
        self.inner.lock().unwrap().webhook = WebhookInfo {
            url: url.to_string(),
            ..Default::default()
        };
        self
    }

    /// Register a chat so `get_chat` can resolve it.
    pub fn with_chat(self, chat: ChatInfo) -> Self {
        self.inner.lock().unwrap().chats.insert(chat.id, chat);
        self
    }

    pub fn with_chat_delay(self, delay: std::time::Duration) -> Self {
        self.inner.lock().unwrap().chat_delay = delay;
        self
    }

    /// Queue an update to be returned by the next `get_updates`.
    pub fn push_update(&self, id: u32, kind: UpdateKind) {
        self.inner
            .lock()
            .unwrap()
            .updates
            .push_back(BotUpdate { id, kind });
    }

    /// Make `copy_message(to, from, id)` return this message.
    pub fn script_copy(&self, from: &str, message_id: i32, copied: CopiedMessage) {
        self.inner
            .lock()
            .unwrap()
            .copies
            .insert((from.to_string(), message_id), copied);
    }

    pub fn fail_copy(&self, from: &str, message_id: i32, error: TgError) {
        self.inner
            .lock()
            .unwrap()
            .copy_errors
            .insert((from.to_string(), message_id), error);
    }

    /// Bytes served for a `file_id` (a tiny PNG by default).
    pub fn with_file(&self, file_id: &str, bytes: Vec<u8>) {
        self.inner
            .lock()
            .unwrap()
            .files
            .insert(file_id.to_string(), bytes);
    }

    pub fn sends(&self) -> Vec<RecordedSend> {
        self.inner.lock().unwrap().sends.clone()
    }

    pub fn send_count(&self) -> usize {
        self.inner.lock().unwrap().sends.len()
    }

    pub fn texts(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().texts.clone()
    }

    pub fn last_text(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .texts
            .last()
            .map(|(_, text)| text.clone())
    }

    pub fn deleted(&self) -> Vec<(String, i32)> {
        self.inner.lock().unwrap().deleted.clone()
    }

    pub fn downloads(&self) -> Vec<(String, PathBuf)> {
        self.inner.lock().unwrap().downloads.clone()
    }

    pub fn command_menu(&self, chat_id: Option<i64>) -> Option<BotCommands> {
        self.inner
            .lock()
            .unwrap()
            .command_menus
            .get(&chat_id)
            .cloned()
    }

    /// How many times `set_webhook` was called, and with which secret.
    pub fn webhook_registrations(&self) -> (u32, Option<String>) {
        let state = self.inner.lock().unwrap();
        (state.webhook_calls, state.webhook_secret.clone())
    }

    pub fn webhook_deletes(&self) -> u32 {
        self.inner.lock().unwrap().webhook_deletes
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Transport for MockTransport {
    fn label(&self) -> String {
        "mock".to_string()
    }

    async fn me(&self) -> TgResult<String> {
        Ok("@mock_bot".to_string())
    }

    async fn check_chat(&self, _chat: &str) -> TgResult<AdminStatus> {
        Ok(self.inner.lock().unwrap().admin.clone())
    }

    async fn send_batch(
        &self,
        chat: &str,
        media: Vec<OutgoingMedia>,
        opts: SendOptions,
    ) -> TgResult<SentBatch> {
        let mut state = self.inner.lock().unwrap();
        if let Some(err) = state.failures.pop_front() {
            return Err(err);
        }
        if let Some(script) = state.script.as_mut()
            && let Some(outcome) = script.pop_front()
            && let Some(err) = outcome
        {
            return Err(err);
        }
        let count = media.len().max(1);
        let ids: Vec<i32> = (0..count)
            .map(|_| {
                let id = state.next_id;
                state.next_id += 1;
                id
            })
            .collect();
        state.sends.push(RecordedSend {
            chat: chat.to_string(),
            media,
            opts,
        });
        let matched = state.chats.values().find(|info| {
            chat.parse::<i64>().ok() == Some(info.id)
                || info
                    .username
                    .as_deref()
                    .is_some_and(|name| chat.trim_start_matches('@').eq_ignore_ascii_case(name))
        });
        Ok(SentBatch {
            message_ids: ids,
            chat_id: matched.map(|info| info.id).or_else(|| chat.parse().ok()),
            chat_username: matched.and_then(|info| info.username.clone()),
        })
    }

    async fn send_text(&self, chat: &str, html: &str) -> TgResult<()> {
        let mut state = self.inner.lock().unwrap();
        if let Some(err) = state.text_failure.take() {
            return Err(err);
        }
        state.texts.push((chat.to_string(), html.to_string()));
        Ok(())
    }

    async fn get_updates(&self, _offset: i32, _timeout: u32) -> TgResult<Vec<BotUpdate>> {
        let mut state = self.inner.lock().unwrap();
        let mut out = Vec::new();
        while let Some(update) = state.updates.pop_front() {
            out.push(update);
            if out.len() >= 100 {
                break;
            }
        }
        Ok(out)
    }

    async fn get_chat(&self, chat: &str) -> TgResult<ChatInfo> {
        let delay = self.inner.lock().unwrap().chat_delay;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let state = self.inner.lock().unwrap();
        let needle = chat.trim();
        if let Ok(id) = needle.parse::<i64>()
            && let Some(info) = state.chats.get(&id)
        {
            return Ok(info.clone());
        }
        let bare = needle.trim_start_matches('@');
        for info in state.chats.values() {
            if info.username.as_deref() == Some(bare)
                || info.title.as_deref() == Some(needle)
                || info.id.to_string() == needle
            {
                return Ok(info.clone());
            }
        }
        Err(TgError::Permanent(format!(
            "Bad Request: chat not found ({needle})"
        )))
    }

    async fn copy_message(&self, to: &str, from: &str, message_id: i32) -> TgResult<CopiedMessage> {
        let state = self.inner.lock().unwrap();
        if let Some(err) = state
            .copy_errors
            .get(&(from.to_string(), message_id))
            .cloned()
        {
            return Err(err);
        }
        match state.copies.get(&(from.to_string(), message_id)).cloned() {
            Some(mut copied) => {
                copied.chat_id = to.trim().parse::<i64>().unwrap_or(copied.chat_id);
                Ok(copied)
            }
            None => Err(TgError::Permanent(format!(
                "Bad Request: message to copy not found ({from}/{message_id})"
            ))),
        }
    }

    async fn download_file(&self, file_id: &str, dest: &Path) -> TgResult<u64> {
        let mut state = self.inner.lock().unwrap();
        let bytes = state
            .files
            .get(file_id)
            .cloned()
            .unwrap_or_else(|| TINY_PNG.to_vec());
        if let Some(parent) = dest.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            return Err(TgError::Transient(format!("mkdir failed: {err}")));
        }
        if let Err(err) = std::fs::write(dest, &bytes) {
            return Err(TgError::Transient(format!("write failed: {err}")));
        }
        state
            .downloads
            .push((file_id.to_string(), dest.to_path_buf()));
        Ok(bytes.len() as u64)
    }

    async fn delete_message(&self, chat: &str, message_id: i32) -> TgResult<()> {
        self.inner
            .lock()
            .unwrap()
            .deleted
            .push((chat.to_string(), message_id));
        Ok(())
    }

    async fn set_commands(&self, chat_id: Option<i64>, commands: BotCommands) -> TgResult<()> {
        self.inner
            .lock()
            .unwrap()
            .command_menus
            .insert(chat_id, commands);
        Ok(())
    }

    async fn webhook_info(&self) -> TgResult<WebhookInfo> {
        Ok(self.inner.lock().unwrap().webhook.clone())
    }

    async fn set_webhook(&self, url: &str, secret: &str) -> TgResult<()> {
        let mut state = self.inner.lock().unwrap();
        state.webhook = WebhookInfo {
            url: url.to_string(),
            ..Default::default()
        };
        state.webhook_secret = Some(secret.to_string());
        state.webhook_calls += 1;
        Ok(())
    }

    async fn delete_webhook(&self) -> TgResult<()> {
        let mut state = self.inner.lock().unwrap();
        state.webhook = WebhookInfo::default();
        state.webhook_deletes += 1;
        Ok(())
    }

    /// The mock has no JSON wire format: it hands out the next queued update.
    fn parse_update(&self, _body: &[u8]) -> TgResult<BotUpdate> {
        self.inner
            .lock()
            .unwrap()
            .updates
            .pop_front()
            .ok_or_else(|| TgError::Permanent("no update queued in the mock".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: i64, username: &str) -> ChatInfo {
        ChatInfo {
            id,
            title: Some("Night Art".into()),
            username: Some(username.into()),
            is_channel: true,
            is_private: false,
        }
    }

    #[tokio::test]
    async fn chats_resolve_by_id_and_username() {
        let mock = MockTransport::new().with_chat(chat(-100123, "night_art"));
        let by_id = mock.get_chat("-100123").await.unwrap();
        assert_eq!(by_id.id, -100123);
        let by_name = mock.get_chat("@night_art").await.unwrap();
        assert_eq!(by_name.id, -100123);
        assert!(mock.get_chat("@nope").await.is_err());
    }

    #[tokio::test]
    async fn copies_are_scripted_and_failures_are_reported() {
        let mock = MockTransport::new();
        mock.script_copy(
            "-100123",
            77,
            CopiedMessage {
                chat_id: 42,
                message_id: 900,
                media_group_id: Some("g1".into()),
                caption: Some("album".to_string()),
                files: vec![],
            },
        );
        let copied = mock.copy_message("42", "-100123", 77).await.unwrap();
        assert_eq!(copied.media_group_id.as_deref(), Some("g1"));
        assert!(mock.copy_message("42", "-100123", 78).await.is_err());
    }

    #[tokio::test]
    async fn downloads_write_the_registered_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mock = MockTransport::new();
        mock.with_file("f1", b"hello".to_vec());
        let dest = dir.path().join("nested/file.bin");
        let written = mock.download_file("f1", &dest).await.unwrap();
        assert_eq!(written, 5);
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        assert_eq!(mock.downloads().len(), 1);
    }

    #[tokio::test]
    async fn texts_and_updates_are_recorded() {
        let mock = MockTransport::new();
        mock.send_text("42", "<b>hi</b>").await.unwrap();
        assert_eq!(mock.last_text().as_deref(), Some("<b>hi</b>"));
        mock.push_update(1, UpdateKind::Other);
        assert_eq!(mock.get_updates(0, 1).await.unwrap().len(), 1);
        assert_eq!(mock.get_updates(0, 1).await.unwrap().len(), 0);
    }
}
