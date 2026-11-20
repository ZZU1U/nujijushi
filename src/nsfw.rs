//! NSFW gate: rating + manual override + tag rules + per-channel policy.

use crate::db::tags::TagRules;
use crate::model::{Channel, NsfwPolicy, Post};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Publish normally.
    Send,
    /// Publish with `has_spoiler` (and an `#nsfw` tag).
    SendSpoilered,
    /// Never publish this post to this channel.
    Reject(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    pub nsfw: bool,
    pub reason: String,
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        !matches!(self.verdict, Verdict::Reject(_))
    }

    pub fn spoiler(&self) -> bool {
        self.verdict == Verdict::SendSpoilered
    }
}

/// Decide whether `post` may go to `channel`, and whether it needs a spoiler.
pub fn evaluate(post: &Post, channel: &Channel, rules: &TagRules) -> Decision {
    // 1. absolute blocks: rules and artists are respected regardless of channel
    if let Some(tag) = rules.blocked_hit(&post.tags) {
        return Decision {
            verdict: Verdict::Reject(format!("blocked tag: {tag}")),
            nsfw: true,
            reason: format!("blocked tag: {tag}"),
        };
    }
    if post.author.as_ref().map(|a| a.blocked).unwrap_or(false) {
        return Decision {
            verdict: Verdict::Reject("artist is blocked".to_string()),
            nsfw: post.rating.is_nsfw(),
            reason: "artist is blocked".to_string(),
        };
    }

    // 2. channel-specific tag gates
    let blocked: Vec<String> = channel
        .blocked_tags_list()
        .iter()
        .map(|t| crate::db::tags::normalize(t).unwrap_or_else(|| t.to_lowercase()))
        .collect();
    if let Some(hit) = post.tags.iter().find(|t| blocked.iter().any(|b| b == *t)) {
        return Decision {
            verdict: Verdict::Reject(format!("channel blocks tag: {hit}")),
            nsfw: true,
            reason: format!("channel blocks tag: {hit}"),
        };
    }
    let required: Vec<String> = channel
        .required_tags_list()
        .iter()
        .map(|t| crate::db::tags::normalize(t).unwrap_or_else(|| t.to_lowercase()))
        .collect();
    if !required.is_empty() {
        let missing: Vec<&String> = required
            .iter()
            .filter(|req| !post.tags.iter().any(|t| t == *req))
            .collect();
        if !missing.is_empty() {
            let names = missing
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Decision {
                verdict: Verdict::Reject(format!("missing required tag(s): {names}")),
                nsfw: post.rating.is_nsfw(),
                reason: format!("missing required tag(s): {names}"),
            };
        }
    }

    // 3. media gate
    if post.media.is_empty() {
        return Decision {
            verdict: Verdict::Reject("No outgoing media".into()),
            nsfw: post.rating.is_nsfw(),
            reason: "No outgoing media".into(),
        };
    }
    if (post.media.len() as i64) < channel.min_media {
        return Decision {
            verdict: Verdict::Reject(format!(
                "needs at least {} media item(s), post has {}",
                channel.min_media,
                post.media.len()
            )),
            nsfw: post.rating.is_nsfw(),
            reason: "not enough media".to_string(),
        };
    }

    // 4. content rating
    let nsfw = post.is_nsfw(rules.hits_nsfw(&post.tags));
    if !nsfw {
        return Decision {
            verdict: Verdict::Send,
            nsfw: false,
            reason: "sfw".to_string(),
        };
    }
    match channel.nsfw_policy {
        NsfwPolicy::Allow => Decision {
            verdict: Verdict::Send,
            nsfw: true,
            reason: "nsfw allowed".to_string(),
        },
        NsfwPolicy::Spoil => Decision {
            verdict: Verdict::SendSpoilered,
            nsfw: true,
            reason: "nsfw spoiler".to_string(),
        },
        NsfwPolicy::Reject => Decision {
            verdict: Verdict::Reject("channel rejects nsfw".to_string()),
            nsfw: true,
            reason: "channel rejects nsfw".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Author, MediaItem, PostState, Rating};
    use chrono::Utc;

    fn post_with(tags: &[&str], rating: Rating) -> Post {
        Post {
            id: 1,
            source: "pixiv".into(),
            source_id: "1".into(),
            url: Some("https://example.com/1".into()),
            author: Some(Author {
                id: 1,
                source: "pixiv".into(),
                source_id: "a".into(),
                name: Some("artist".into()),
                handle: None,
                url: None,
                blocked: false,
            }),
            title: None,
            description: None,
            rating,
            posted_at: None,
            imported_at: Utc::now(),
            nsfw_override: None,
            state: PostState::Approved,
            notes: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            media: vec![MediaItem {
                id: 1,
                post_id: 1,
                ordinal: 0,
                blob_hash: "h".into(),
                ext: "png".into(),
                mime: None,
                width: None,
                height: None,
                bytes: 1,
                is_video: false,
                poster_hash: None,
                sensitive: false,
                meta_json: None,
            }],
        }
    }

    fn channel(policy: NsfwPolicy) -> Channel {
        let mut ch = Channel::new("test", "-100123");
        ch.nsfw_policy = policy;
        ch
    }

    fn rules(block: &[&str], nsfw: &[&str]) -> TagRules {
        TagRules {
            block: block.iter().map(|s| s.to_string()).collect(),
            nsfw: nsfw.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn sfw_passes_everywhere() {
        let p = post_with(&["landscape"], Rating::Sfw);
        for policy in [NsfwPolicy::Allow, NsfwPolicy::Spoil, NsfwPolicy::Reject] {
            let d = evaluate(&p, &channel(policy), &rules(&[], &[]));
            assert_eq!(d.verdict, Verdict::Send);
            assert!(!d.spoiler());
        }
    }

    #[test]
    fn nsfw_policy_matrix() {
        let p = post_with(&["nude"], Rating::Explicit);
        assert_eq!(
            evaluate(&p, &channel(NsfwPolicy::Allow), &rules(&[], &[])).verdict,
            Verdict::Send
        );
        // queued posts get spoilers when the policy says so
        assert_eq!(
            evaluate(&p, &channel(NsfwPolicy::Spoil), &rules(&[], &[])).verdict,
            Verdict::SendSpoilered
        );
        let rejected = evaluate(&p, &channel(NsfwPolicy::Reject), &rules(&[], &[]));
        assert!(!rejected.is_allowed());
        assert!(rejected.nsfw);
    }

    #[test]
    fn blocked_tags_win_over_channel_policy() {
        let p = post_with(&["guro"], Rating::Sfw);
        let decision = evaluate(&p, &channel(NsfwPolicy::Allow), &rules(&["guro"], &[]));
        assert!(!decision.is_allowed());
        assert!(matches!(decision.verdict, Verdict::Reject(_)));
    }

    #[test]
    fn nsfw_tag_rule_marks_sfw_rated_post() {
        let p = post_with(&["lewd"], Rating::Sfw);
        let decision = evaluate(&p, &channel(NsfwPolicy::Spoil), &rules(&[], &["lewd"]));
        assert!(decision.nsfw);
        assert!(decision.spoiler());
    }

    #[test]
    fn manual_override_beats_rating() {
        let mut p = post_with(&[], Rating::Explicit);
        p.nsfw_override = Some(false);
        assert_eq!(
            evaluate(&p, &channel(NsfwPolicy::Reject), &rules(&[], &[])).verdict,
            Verdict::Send
        );
        p.nsfw_override = Some(true);
        let p2 = post_with(&[], Rating::Sfw);
        let mut p2 = p2;
        p2.nsfw_override = Some(true);
        assert!(evaluate(&p2, &channel(NsfwPolicy::Spoil), &rules(&[], &[])).spoiler());
    }

    #[test]
    fn channel_tag_gates() {
        let p = post_with(&["png", "landscape"], Rating::Sfw);
        let mut ch = channel(NsfwPolicy::Allow);
        ch.blocked_tags = "png".into();
        assert!(!evaluate(&p, &ch, &rules(&[], &[])).is_allowed());

        let mut ch = channel(NsfwPolicy::Allow);
        ch.required_tags = "landscape".into();
        assert!(evaluate(&p, &ch, &rules(&[], &[])).is_allowed());

        let mut ch = channel(NsfwPolicy::Allow);
        ch.required_tags = "portrait".into();
        assert!(!evaluate(&p, &ch, &rules(&[], &[])).is_allowed());
    }

    #[test]
    fn blocked_artist_is_skipped() {
        let mut p = post_with(&["x"], Rating::Sfw);
        if let Some(author) = p.author.as_mut() {
            author.blocked = true;
        }
        assert!(!evaluate(&p, &channel(NsfwPolicy::Allow), &rules(&[], &[])).is_allowed());
    }

    #[test]
    fn min_media_gate() {
        let mut p = post_with(&[], Rating::Sfw);
        p.media.clear();
        let mut ch = channel(NsfwPolicy::Allow);
        ch.min_media = 1;
        assert!(!evaluate(&p, &ch, &rules(&[], &[])).is_allowed());
    }

    #[test]
    fn one_sensitive_item_makes_the_post_nsfw() {
        // twitter flags individual images; the post-level rating can still be sfw
        let mut post = post_with(&[], Rating::Sfw);
        post.media[0].sensitive = true;
        assert!(post.is_nsfw(false), "a sensitive item is enough");

        let spoil = channel(NsfwPolicy::Spoil);
        assert_eq!(
            evaluate(&post, &spoil, &rules(&[], &[])).verdict,
            Verdict::SendSpoilered
        );
        let reject = channel(NsfwPolicy::Reject);
        assert!(!evaluate(&post, &reject, &rules(&[], &[])).is_allowed());

        // the manual override still wins over a per-media flag
        post.nsfw_override = Some(false);
        assert_eq!(
            evaluate(&post, &spoil, &rules(&[], &[])).verdict,
            Verdict::Send
        );
    }
}
