//! Telegram sink: a private channel used as an image backup drive.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{MirrorPost, Progress, Sink};
use crate::error::{Error, Result};
use crate::media::plan_messages;
use crate::render::truncate::{CAPTION_LIMIT, fit_caption};
use crate::tg::{MediaKind, OutgoingMedia, SendOptions, TgError, Transport};

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

/// Progress of a chunked backup. `ids` has one message id per completed group;
/// `plan` changes when the outgoing files do, in which case a retry starts over.
#[derive(Debug, Default, Deserialize, Serialize)]
struct Checkpoint {
    plan: String,
    groups: usize,
    ids: Vec<String>,
}

fn plan_key(item: &MirrorPost) -> String {
    let mut hasher = blake3::Hasher::new();
    for file in &item.files {
        hasher.update(file.relative.as_bytes());
        hasher.update(b"\0");
        hasher.update(&file.bytes.to_le_bytes());
        hasher.update(if file.is_video { b"1" } else { b"0" });
    }
    hasher.finalize().to_hex().to_string()
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
        let mut ignore = |_: &str| {};
        self.put_resumable(item, None, &mut ignore).await
    }

    async fn put_resumable(
        &self,
        item: &MirrorPost,
        resume_from: Option<&str>,
        progress: Progress<'_>,
    ) -> Result<String> {
        let flags: Vec<bool> = item.files.iter().map(|file| file.is_video).collect();
        let groups = plan_messages(&flags);
        let plan = plan_key(item);
        let mut checkpoint = resume_from
            .and_then(|raw| serde_json::from_str::<Checkpoint>(raw).ok())
            .filter(|checkpoint| {
                checkpoint.plan == plan
                    && checkpoint.groups <= groups.len()
                    && checkpoint.groups == checkpoint.ids.len()
            })
            .unwrap_or_default();
        if checkpoint.plan.is_empty() {
            checkpoint.plan = plan;
        }

        let caption = self.caption(item);
        let mut ids = checkpoint.ids;
        for (index, group) in groups.iter().enumerate() {
            if index < checkpoint.groups {
                continue;
            }
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
            let batch = match self
                .transport
                .send_batch(&self.chat_id, media, SendOptions::default())
                .await
            {
                Ok(batch) => batch,
                // Nothing went out, or the first attempt is unconfirmed: the
                // normal failed/blocked handling is right for those.
                Err(err) if ids.is_empty() && !matches!(err, TgError::Uncertain(_)) => {
                    return Err(Error::Telegram(err.to_string()));
                }
                Err(err) => {
                    return Err(Error::Partial(format!(
                        "sent {} group(s) before failing (ids: {}): {err}. \
                         Verify the backup channel before retrying.",
                        checkpoint.groups,
                        ids.join(",")
                    )));
                }
            };
            ids.extend(batch.message_ids.iter().map(|id| id.to_string()));
            checkpoint.groups = index + 1;
            checkpoint.ids = ids.clone();
            progress(&serde_json::to_string(&checkpoint)?);
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

    #[tokio::test]
    async fn a_mid_album_failure_is_partial_and_not_blindly_retried() {
        let dir = tempfile::tempdir().unwrap();
        // One photo then one video: two messages. The second one fails.
        let mock = Arc::new(MockTransport::with_script(vec![
            None,
            Some(TgError::Permanent("file is too big".into())),
        ]));
        let sink = TelegramSink::new(mock.clone(), "-100123".into());
        let item = sample_mirror(&dir);

        let error = sink.put(&item).await.unwrap_err();
        assert!(matches!(error, Error::Partial(_)), "got {error:?}");
        assert!(error.to_string().contains("Verify the backup channel"));
        assert_eq!(mock.send_count(), 1, "the first group went out");
    }

    #[tokio::test]
    async fn a_retry_resumes_after_the_last_confirmed_group() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockTransport::with_script(vec![
            None,
            Some(TgError::Permanent("file is too big".into())),
        ]));
        let sink = TelegramSink::new(mock.clone(), "-100123".into());
        let item = sample_mirror(&dir);

        let mut saved = String::new();
        {
            let mut progress = |value: &str| saved = value.to_string();
            let error = sink
                .put_resumable(&item, None, &mut progress)
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Partial(_)), "got {error:?}");
        }
        assert_eq!(mock.send_count(), 1, "the first group went out");
        assert!(saved.contains("\"groups\":1"), "checkpoint: {saved}");

        mock.append_script(vec![None]);
        let resume = saved.clone();
        let mut progress = |value: &str| saved = value.to_string();
        let external = sink
            .put_resumable(&item, Some(&resume), &mut progress)
            .await
            .unwrap();
        assert_eq!(mock.send_count(), 2, "only the failed group is resent");
        assert_eq!(
            external.split(',').count(),
            2,
            "both group ids are reported"
        );
    }
}
