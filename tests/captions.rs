//! Snapshot tests: the exact text sent to Telegram, per channel configuration.

use nujijushi::model::{Attribution, Channel, NsfwPolicy, Post, PostState, Rating, TagStyle};
use nujijushi::render::{CAPTION_LIMIT, Renderer, build_context};

fn post() -> Post {
    use nujijushi::model::{Author, MediaItem};
    Post {
        id: 7,
        source: "pixiv".into(),
        source_id: "123456".into(),
        url: Some("https://www.pixiv.net/artworks/123456".into()),
        author: Some(Author {
            id: 3,
            source: "pixiv".into(),
            source_id: "42".into(),
            name: Some("Nightjar".into()),
            handle: Some("nightjar_".into()),
            url: Some("https://www.pixiv.net/users/42".into()),
            blocked: false,
        }),
        title: Some("Quiet harbour".into()),
        description: Some(
            "Harbour at 4am, still water, one lamp still burning.\n\
             Painted for the night shift crowd."
                .into(),
        ),
        rating: Rating::Sfw,
        posted_at: None,
        imported_at: chrono::Utc::now(),
        nsfw_override: None,
        state: PostState::Approved,
        notes: None,
        tags: vec![
            "original".into(),
            "landscape".into(),
            "digital_art".into(),
            "harbour".into(),
        ],
        media: (0..3)
            .map(|ordinal| MediaItem {
                id: ordinal as i64 + 1,
                post_id: 7,
                ordinal: ordinal as i64,
                blob_hash: format!("hash{ordinal}"),
                ext: "png".into(),
                mime: Some("image/png".into()),
                width: Some(1400),
                height: Some(900),
                bytes: 2048,
                is_video: false,
                poster_hash: None,
                sensitive: false,
                meta_json: None,
            })
            .collect(),
    }
}

fn render(channel: &Channel, post: &Post, nsfw: bool, spoiler: bool) -> String {
    let renderer = Renderer::new().unwrap();
    let context = build_context(channel, post, nsfw, spoiler, CAPTION_LIMIT);
    renderer.render(channel, &context).unwrap().text
}

#[test]
fn default_caption() {
    let channel = Channel::new("night shift", "@night_shift");
    insta::assert_snapshot!("default_caption", render(&channel, &post(), false, false));
}

#[test]
fn nsfw_caption_with_spoiler_and_plain_tags() {
    let mut channel = Channel::new("after dark", "@after_dark");
    channel.nsfw_policy = NsfwPolicy::Spoil;
    channel.tag_style = TagStyle::Plain;
    channel.attribution = Attribution::Link;
    channel.max_tags = 3;
    let mut post = post();
    post.rating = Rating::Explicit;
    insta::assert_snapshot!("nsfw_caption", render(&channel, &post, true, true));
}

#[test]
fn minimal_caption_without_author_or_description() {
    let mut channel = Channel::new("bare", "@bare");
    channel.include_source_tag = false;
    channel.attribution = Attribution::None;
    let mut post = post();
    post.author = None;
    post.description = None;
    post.title = None;
    post.url = None;
    insta::assert_snapshot!("minimal_caption", render(&channel, &post, false, false));
}

#[test]
fn forty_tags_are_capped_and_hashtagged() {
    let mut channel = Channel::new("tagged", "@tagged");
    channel.max_tags = 40;
    // the source tag is off by default now, so ask for it explicitly here
    channel.include_source_tag = true;
    let mut post = post();
    post.tags = (0..40).map(|index| format!("tag{index}")).collect();
    let rendered = render(&channel, &post, false, false);
    let tag_line = rendered
        .lines()
        .find(|line| line.starts_with("#tag0"))
        .expect("tag line");
    assert_eq!(
        tag_line.split_whitespace().count(),
        41,
        "40 tags + source tag"
    );
    insta::assert_snapshot!("many_tags", rendered);
}

#[test]
fn generated_hashtags_keep_character_and_franchise_in_one_link() {
    let mut channel = Channel::new("tagged", "@tagged");
    let mut post = post();
    post.author = None;
    post.description = None;
    post.url = None;
    post.tags = [
        "character_name_(franchise_name)",
        "character_name_franchise_name",
        "fate/stay night",
        "初音ミク",
        "第٢章",
        "2026",
        "(?)",
    ]
    .map(String::from)
    .into();
    let default = render(&channel, &post, false, false);
    insta::assert_snapshot!("telegram_hashtag_variants", default);

    channel.template = Some("{{ tags | hashtags }}".into());
    assert_eq!(render(&channel, &post, false, false), default);

    channel.tag_style = TagStyle::Plain;
    channel.template = Some("{{ hashtags }}".into());
    assert!(render(&channel, &post, false, false).contains("character_name_(franchise_name)"));
}

#[test]
fn very_long_description_is_clipped_to_the_limit() {
    let channel = Channel::new("long", "@long");
    let mut post = post();
    // a single unbroken paragraph defeats the word filter on purpose
    post.description = Some("x".repeat(4000));
    let rendered = render(&channel, &post, false, false);
    assert!(rendered.chars().count() <= CAPTION_LIMIT);
    let first = rendered.chars().take(40).collect::<String>();
    insta::assert_snapshot!(
        "long_description_tail",
        rendered.chars().rev().take(60).collect::<String>()
    );
    assert!(first.starts_with("<b>Nightjar</b>"));
}

#[test]
fn a_handle_only_author_reads_as_the_handle() {
    // a stored row that only has a handle (an older import, or one whose user
    // object carried no display name): the handle is the only name there is
    let channel = Channel::new("x", "@x");
    let mut post = post();
    if let Some(author) = post.author.as_mut() {
        author.source = "twitter".into();
        author.name = None;
        author.handle = Some("asobinin".into());
    }
    let rendered = render(&channel, &post, false, false);
    assert!(rendered.starts_with("<b>asobinin</b>"), "{rendered}");
    assert!(
        !rendered.contains('@'),
        "a handle is plain text, never a Telegram mention: {rendered}"
    );
}

#[test]
fn a_name_that_is_the_handle_is_not_printed_twice() {
    // rows imported before 0.2.0 stored the handle as the name
    let channel = Channel::new("x", "@x");
    let mut post = post();
    if let Some(author) = post.author.as_mut() {
        author.source = "twitter".into();
        author.name = Some("asobinin".into());
        author.handle = Some("asobinin".into());
    }
    let rendered = render(&channel, &post, false, false);
    assert!(rendered.starts_with("<b>asobinin</b>"), "{rendered}");
    assert!(!rendered.contains("(@asobinin)"), "{rendered}");
}

#[test]
fn compact_and_detailed_presets() {
    let mut channel = Channel::new("night shift", "@night_shift");
    channel.template_name = "compact".into();
    insta::assert_snapshot!("compact_caption", render(&channel, &post(), false, false));
    channel.template_name = "detailed".into();
    insta::assert_snapshot!("detailed_caption", render(&channel, &post(), false, false));
}
