//! Build the template context for a post and channel.

use std::collections::BTreeMap;

use chrono::Utc;
use minijinja::Value;

use crate::model::{Attribution, Channel, Post, TagStyle};
use crate::render::truncate::CAPTION_LIMIT;

pub use crate::render::truncate::RenderedCaption;

#[derive(Debug, Clone)]
pub struct CaptionContext {
    pub value: Value,
    pub limit: usize,
    pub nsfw: bool,
    pub spoiler: bool,
    pub tags: Vec<String>,
}

fn escape(text: &str) -> String {
    htmlescape::encode_minimal(text)
}

/// Render the tag block for a channel's tag style.
pub fn tag_block(tags: &[String], style: TagStyle) -> String {
    match style {
        TagStyle::Hashtags => super::filters::render_hashtags(tags),
        TagStyle::Plain => tags.join(", "),
        TagStyle::None => String::new(),
    }
}

/// Effective tag list for a caption: capped, plus `#nsfw` and the source tag.
pub fn caption_tags(channel: &Channel, post: &Post, spoiler: bool) -> Vec<String> {
    let mut tags: Vec<String> = post.tags.clone();
    let cap = channel.max_tags.max(0) as usize;
    if cap > 0 && tags.len() > cap {
        tags.truncate(cap);
    }
    if channel.tag_style == TagStyle::None {
        return Vec::new();
    }
    if spoiler && !tags.iter().any(|t| t == "nsfw") {
        tags.push("nsfw".to_string());
    }
    if channel.include_source_tag {
        let source = post.source.to_lowercase();
        if !source.is_empty() && !tags.contains(&source) {
            tags.push(source);
        }
    }
    tags
}

pub fn build_context(
    channel: &Channel,
    post: &Post,
    nsfw: bool,
    spoiler: bool,
    limit: usize,
) -> CaptionContext {
    let tags = caption_tags(channel, post, spoiler);
    let hashtags = tag_block(&tags, channel.tag_style);

    let source_label = crate::source::source_label(&post.source);
    let source_url = post.url.clone();
    let (source_link, attribution_inline, attribution_footer) =
        match (&source_url, channel.attribution) {
            (Some(url), Attribution::Link) => (
                Value::from_safe_string(format!(
                    "<a href=\"{}\">{}</a>",
                    escape(url),
                    escape(source_label)
                )),
                true,
                false,
            ),
            (Some(url), Attribution::Footer) => (
                Value::from_safe_string(format!(
                    "via <a href=\"{}\">{}</a>",
                    escape(url),
                    escape(source_label)
                )),
                false,
                true,
            ),
            (Some(_), Attribution::None) | (None, _) => (Value::from(""), false, false),
        };

    // A handle goes into the caption as plain text. In Telegram `@name` is a
    // *mention*: it pings whoever owns that username, and a twitter/pixiv handle
    // is not a Telegram username at all, so the `@` was either a ping or a dead
    // link. `author.mention` keeps the `@` form for a template that wants one.
    let given_name = post.author.as_ref().and_then(|a| a.name.clone());
    let plain_handle = post
        .author
        .as_ref()
        .and_then(|a| a.handle.as_deref())
        .map(|h| h.trim().trim_start_matches('@').to_string())
        .filter(|h| !h.is_empty());
    let mention = plain_handle
        .as_ref()
        .map(|h| format!("@{h}"))
        .unwrap_or_default();
    // a handle identical to the name is dropped from the "(...)" suffix, but it
    // still serves as the label when there is no display name at all
    let handle = plain_handle
        .clone()
        .filter(|h| {
            given_name
                .as_deref()
                .is_none_or(|name| !name.trim().eq_ignore_ascii_case(h))
        })
        .unwrap_or_default();
    let author_label = given_name
        .clone()
        .or(plain_handle)
        .unwrap_or_else(|| "unknown artist".to_string());
    let author_url = post
        .author
        .as_ref()
        .and_then(|a| a.url.clone())
        .unwrap_or_default();

    let mut post_map = BTreeMap::new();
    post_map.insert("id".to_string(), Value::from(post.id));
    post_map.insert("source".to_string(), Value::from(post.source.clone()));
    post_map.insert("source_id".to_string(), Value::from(post.source_id.clone()));
    post_map.insert(
        "url".to_string(),
        Value::from(post.url.clone().unwrap_or_default()),
    );
    post_map.insert(
        "title".to_string(),
        Value::from(post.title.clone().unwrap_or_default()),
    );
    post_map.insert(
        "description".to_string(),
        Value::from(post.description.clone().unwrap_or_default()),
    );
    post_map.insert("rating".to_string(), Value::from(post.rating.as_str()));
    post_map.insert("media_count".to_string(), Value::from(post.media.len()));
    post_map.insert(
        "date".to_string(),
        Value::from(
            post.posted_at
                .unwrap_or_else(Utc::now)
                .format("%Y-%m-%d")
                .to_string(),
        ),
    );

    let mut author_map = BTreeMap::new();
    // `name` keeps its old meaning (name, else handle, else a placeholder) so
    // custom templates keep working; `label` is the same rule under a clearer
    // name, and `given` is the real display name (may be empty).
    author_map.insert("name".to_string(), Value::from(author_label.clone()));
    author_map.insert("label".to_string(), Value::from(author_label));
    author_map.insert(
        "given".to_string(),
        Value::from(given_name.unwrap_or_default()),
    );
    author_map.insert("handle".to_string(), Value::from(handle));
    author_map.insert("url".to_string(), Value::from(author_url));
    // `@name` is a Telegram *mention* (a ping), so the caption gets the bare
    // handle; a template that really wants to ping someone asks for this one.
    author_map.insert("mention".to_string(), Value::from(mention));
    let mut channel_map = BTreeMap::new();
    channel_map.insert("id".to_string(), Value::from(channel.id));
    channel_map.insert("title".to_string(), Value::from(channel.title.clone()));
    channel_map.insert("tg_id".to_string(), Value::from(channel.tg_id.clone()));
    let mut root = BTreeMap::new();
    root.insert("post".to_string(), Value::from(post_map));
    root.insert("author".to_string(), Value::from(author_map));
    root.insert("channel".to_string(), Value::from(channel_map));
    root.insert("tags".to_string(), Value::from(tags.clone()));
    root.insert("hashtags".to_string(), Value::from(hashtags));
    root.insert("source_label".to_string(), Value::from(source_label));
    root.insert("source_link".to_string(), source_link);
    root.insert(
        "attribution_inline".to_string(),
        Value::from(attribution_inline),
    );
    root.insert(
        "attribution_footer".to_string(),
        Value::from(attribution_footer),
    );
    root.insert("nsfw".to_string(), Value::from(nsfw));
    root.insert("spoiler".to_string(), Value::from(spoiler));

    CaptionContext {
        value: Value::from(root),
        limit: limit.clamp(64, CAPTION_LIMIT),
        nsfw,
        spoiler,
        tags,
    }
}

/// Small synthetic context used to validate a template in the UI.
pub fn sample_context() -> CaptionContext {
    use crate::model::{Author, MediaItem, PostState, Rating};

    let channel = Channel::new("preview channel", "@preview");
    let post = Post {
        id: 1,
        source: "pixiv".into(),
        source_id: "123456".into(),
        url: Some("https://www.pixiv.net/artworks/123456".into()),
        author: Some(Author {
            id: 1,
            source: "pixiv".into(),
            source_id: "42".into(),
            name: Some("Sample Artist".into()),
            handle: Some("@sample".into()),
            url: Some("https://www.pixiv.net/users/42".into()),
            blocked: false,
        }),
        title: Some("Summer lights".into()),
        description: Some("A quiet street at dusk, hand painted. Thanks for looking!".to_string()),
        rating: Rating::Sfw,
        posted_at: Some(Utc::now()),
        imported_at: Utc::now(),
        nsfw_override: None,
        state: PostState::Approved,
        notes: None,
        tags: vec!["original".into(), "landscape".into(), "digital_art".into()],
        media: vec![MediaItem {
            id: 1,
            post_id: 1,
            ordinal: 0,
            blob_hash: "hash".into(),
            ext: "png".into(),
            mime: Some("image/png".into()),
            width: Some(1200),
            height: Some(800),
            bytes: 1000,
            is_video: false,
            poster_hash: None,
            sensitive: false,
            meta_json: None,
        }],
    };
    build_context(&channel, &post, false, false, CAPTION_LIMIT)
}

impl CaptionContext {
    pub fn rendered(&self, caption: &RenderedCaption) -> String {
        caption.text.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Renderer;

    fn ctx(channel: &Channel) -> CaptionContext {
        build_context(channel, &sample_post(), false, false, CAPTION_LIMIT)
    }

    fn sample_post() -> Post {
        use crate::model::{Author, MediaItem, PostState, Rating};
        Post {
            id: 1,
            source: "pixiv".into(),
            source_id: "123456".into(),
            url: Some("https://www.pixiv.net/artworks/1".into()),
            author: Some(Author {
                id: 1,
                source: "pixiv".into(),
                source_id: "42".into(),
                name: Some("Sample Artist".into()),
                handle: Some("sample".into()),
                url: Some("https://www.pixiv.net/users/42".into()),
                blocked: false,
            }),
            title: Some("Summer lights".into()),
            description: Some("A quiet street at dusk.".into()),
            rating: Rating::Sfw,
            posted_at: None,
            imported_at: Utc::now(),
            nsfw_override: None,
            state: PostState::Approved,
            notes: None,
            tags: vec!["original".into(), "landscape".into()],
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

    #[test]
    fn default_caption_layout() {
        let renderer = Renderer::new().unwrap();
        let channel = Channel::new("ch", "@ch");
        let out = renderer.render(&channel, &ctx(&channel)).unwrap();
        let lines: Vec<&str> = out.text.lines().collect();
        assert_eq!(lines[0], "<b>Sample Artist</b> (sample)");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "A quiet street at dusk.");
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "#original #landscape");
        assert_eq!(lines[5], "");
        assert_eq!(
            lines[6],
            "via <a href=\"https://www.pixiv.net/artworks/1\">Pixiv</a>"
        );
        assert!(!out.truncated);
    }

    #[test]
    fn attribution_modes_change_the_layout() {
        let renderer = Renderer::new().unwrap();
        let mut channel = Channel::new("ch", "@ch");
        channel.attribution = Attribution::Link;
        let out = renderer.render(&channel, &ctx(&channel)).unwrap();
        assert!(
            out.text
                .starts_with("<b>Sample Artist</b> (sample) · <a href=")
        );

        channel.attribution = Attribution::None;
        let out = renderer.render(&channel, &ctx(&channel)).unwrap();
        assert!(!out.text.contains("<a href"));
        assert!(!out.text.contains("via "));
    }

    /// `@name` in a Telegram caption is a *mention*: it pings whoever owns that
    /// username, and a twitter/pixiv handle is not a Telegram username at all, so
    /// the caption carries the bare handle. `author.mention` is the opt-in form.
    #[test]
    fn a_handle_is_plain_text_and_the_mention_is_opt_in() {
        let renderer = Renderer::new().unwrap();
        let channel = Channel::new("ch", "@ch");
        let mut post = sample_post();
        if let Some(author) = post.author.as_mut() {
            author.name = None;
            // the telegram importer stores the handle *with* the @
            author.handle = Some("@nightjar".into());
        }
        let out = renderer
            .render(
                &channel,
                &build_context(&channel, &post, false, false, CAPTION_LIMIT),
            )
            .unwrap();
        assert_eq!(
            out.text.lines().next().unwrap_or_default(),
            "<b>nightjar</b>",
            "the handle is the label, without the @: {}",
            out.text
        );
        assert!(!out.text.contains('@'), "no mention: {}", out.text);

        let mut pingers = Channel::new("ch", "@ch");
        pingers.template = Some("{{ author.handle }} / {{ author.mention }}".into());
        let out = renderer
            .render(
                &pingers,
                &build_context(&pingers, &post, false, false, CAPTION_LIMIT),
            )
            .unwrap();
        assert_eq!(out.text, "nightjar / @nightjar", "a template can opt in");
    }

    #[test]
    fn tag_styles() {
        let renderer = Renderer::new().unwrap();
        let mut channel = Channel::new("ch", "@ch");
        channel.tag_style = TagStyle::Plain;
        let out = renderer.render(&channel, &ctx(&channel)).unwrap();
        assert!(out.text.contains("original, landscape"));
        assert!(!out.text.contains('#'));

        channel.tag_style = TagStyle::None;
        let out = renderer.render(&channel, &ctx(&channel)).unwrap();
        assert!(!out.text.contains("landscape"));
    }

    #[test]
    fn max_tags_caps_the_hashtags() {
        let mut channel = Channel::new("ch", "@ch");
        channel.max_tags = 1;
        let post = sample_post();
        let tags = caption_tags(&channel, &post, false);
        // the source tag is off by default now, so the cap leaves one tag
        assert_eq!(tags, vec!["original".to_string()]);
    }

    #[test]
    fn nsfw_tag_appears_when_spoilered() {
        let mut channel = Channel::new("ch", "@ch");
        channel.include_source_tag = false;
        let post = sample_post();
        let tags = caption_tags(&channel, &post, true);
        assert!(tags.contains(&"nsfw".to_string()));
    }

    #[test]
    fn inline_template_overrides_the_builtin() {
        let renderer = Renderer::new().unwrap();
        let mut channel = Channel::new("ch", "@ch");
        channel.template = Some("{{ author.name | upper }} / {{ post.source }}".into());
        let out = renderer.render(&channel, &ctx(&channel)).unwrap();
        assert_eq!(out.text, "SAMPLE ARTIST / pixiv");
    }

    #[test]
    fn long_descriptions_are_truncated_within_limit() {
        let renderer = Renderer::new().unwrap();
        let mut channel = Channel::new("ch", "@ch");
        let mut post = sample_post();
        post.description = Some("word ".repeat(900));
        let context = build_context(&channel, &post, false, false, CAPTION_LIMIT);
        let out = renderer.render(&channel, &context).unwrap();
        assert!(out.length <= 1024);
        assert!(!out.truncated, "word filter keeps it under the cap");
        // an inline template that ignores the filter must still be clipped
        channel.template = Some("{{ post.description }}".into());
        let context = build_context(&channel, &post, false, false, CAPTION_LIMIT);
        let out = renderer.render(&channel, &context).unwrap();
        assert!(out.truncated);
        assert!(out.length <= 1024);
    }

    #[test]
    fn description_is_html_escaped() {
        let renderer = Renderer::new().unwrap();
        let channel = Channel::new("ch", "@ch");
        let mut post = sample_post();
        post.description = Some("5 < 6 & <script>alert(1)</script>".into());
        let context = build_context(&channel, &post, false, false, CAPTION_LIMIT);
        let out = renderer.render(&channel, &context).unwrap();
        assert!(out.text.contains("5 &lt; 6 &amp;"));
        assert!(!out.text.contains("<script>"));
    }

    #[test]
    fn missing_author_and_description_still_render() {
        let renderer = Renderer::new().unwrap();
        let mut channel = Channel::new("ch", "@ch");
        channel.tag_style = TagStyle::Hashtags;
        let mut post = sample_post();
        post.author = None;
        post.description = None;
        post.title = None;
        let context = build_context(&channel, &post, false, false, CAPTION_LIMIT);
        let out = renderer.render(&channel, &context).unwrap();
        assert!(
            !out.text.contains("unknown artist"),
            "no placeholder author line: {}",
            out.text
        );
        assert!(out.text.contains("#original"));
        assert!(!out.text.starts_with('\n'));
    }
}
