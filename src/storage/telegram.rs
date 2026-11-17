//! Telegram sink: a private channel used as an image backup drive.

use std::sync::Arc;

use async_trait::async_trait;

use super::{MirrorPost, Sink};
use crate::error::{Error, Result};
use crate::media::plan_messages;
use crate::render::truncate::{CAPTION_LIMIT, fit_caption};
use crate::tg::{MediaKind, OutgoingMedia, SendOptions, Transport};

pub struct TelegramSink {
    transport: Arc<dyn Transport>,
    chat_id: String,
}

impl TelegramSink {
    pub fn new(transport: Arc<dyn Transport>, chat_id: String) -> Self {
        Self { transport, chat_id }
    }

    fn caption(&self, item: &MirrorPost) -> String {
        let post = &item.post;
        let artist = post
            .author
            .as_ref()
            .map(|author| author.display())
            .unwrap_or_else(|| "unknown artist".to_string());
        let mut text = format!(
            "<b>{}</b>\n{} / {}",
            htmlescape::encode_minimal(&artist),
            htmlescape::encode_minimal(&post.source),
            htmlescape::encode_minimal(&post.source_id)
        );
        if let Some(url) = &post.url {
            text.push('\n');
            text.push_str(&htmlescape::encode_minimal(url));
        }
        text.push_str(&format!(
            "\n<code>{}</code>",
            htmlescape::encode_minimal(&item.dir)
        ));
        fit_caption(text, CAPTION_LIMIT).text
    }
}

#[async_trait]
impl Sink for TelegramSink {
    fn name(&self) -> &'static str {
        "telegram"
    }

    fn describe(&self) -> String {
        format!("send albums to chat {}", self.chat_id)
    }

    async fn put(&self, item: &MirrorPost) -> Result<String> {
        let flags: Vec<bool> = item.files.iter().map(|file| file.is_video).collect();
        let groups = plan_messages(&flags);
        let caption = self.caption(item);
        let mut ids: Vec<String> = Vec::new();

        for (index, group) in groups.iter().enumerate() {
            let media: Vec<OutgoingMedia> = group
                .indices
                .iter()
                .filter_map(|position| item.files.get(*position))
                .map(|file| OutgoingMedia {
                    path: file.path.clone(),
                    kind: if file.is_video {
                        MediaKind::Video
                    } else {
                        MediaKind::Photo
                    },
                    caption: (group.caption && index == 0).then(|| caption.clone()),
                    spoiler: false,
                    thumb: None,
                })
                .collect();
            if media.is_empty() {
                continue;
            }
            let batch = self
                .transport
                .send_batch(&self.chat_id, media, SendOptions::default())
                .await
                .map_err(|err| Error::Telegram(err.to_string()))?;
            ids.extend(batch.message_ids.iter().map(|id| id.to_string()));
        }
        if ids.is_empty() {
            return Err(Error::other("nothing was sent to the backup channel"));
        }
        Ok(ids.join(","))
    }

    async fn check(&self) -> Result<String> {
        let chat = self
            .transport
            .get_chat(&self.chat_id)
            .await
            .map_err(|err| Error::Telegram(err.to_string()))?;
        if !chat.is_channel {
            return Err(Error::other(format!("{} is not a channel", self.chat_id)));
        }
        Ok(format!("{} ({})", chat.label(), self.chat_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests_support::*;
    use crate::tg::ChatInfo;
    use crate::tg::mock::MockTransport;

    #[tokio::test]
    async fn sends_one_album_with_a_caption() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockTransport::new());
        let sink = TelegramSink::new(mock.clone(), "-100123".into());
        let item = sample_mirror(&dir);

        let external = sink.put(&item).await.unwrap();
        assert!(!external.is_empty(), "message ids are recorded");

        let sends = mock.sends();
        // Telegram cannot mix photos and videos in one album: 1 photo + 1 video
        assert_eq!(sends.len(), 2, "one message per media kind");
        assert!(sends.iter().all(|send| send.chat == "-100123"));
        let caption = sends[0].media[0].caption.clone().unwrap();
        assert!(caption.contains("Nightjar"), "caption: {caption}");
        assert!(caption.contains("pixiv / 123456"));
        assert!(
            sends[1].media[0].caption.is_none(),
            "the caption is only on the first message"
        );
    }

    #[tokio::test]
    async fn check_validates_the_chat() {
        let mock = Arc::new(MockTransport::new().with_chat(ChatInfo {
            id: -100123,
            title: Some("Art backup".into()),
            username: None,
            is_channel: true,
            is_private: false,
        }));
        let sink = TelegramSink::new(mock, "-100123".into());
        assert!(sink.check().await.unwrap().contains("Art backup"));
    }

    #[tokio::test]
    async fn check_fails_for_an_unknown_chat() {
        let sink = TelegramSink::new(Arc::new(MockTransport::new()), "-100999".into());
        assert!(sink.check().await.is_err());
    }
}
