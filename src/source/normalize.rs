//! Map messy per-site metadata (gallery-dl output) onto [`NormalizedPost`].

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::db::tags;
use crate::model::Rating;
use crate::source::{NormalizedPost, PostAuthor};

/// Rating words that appear in booru/pixiv metadata.
fn rating_from_words(raw: &str) -> Option<Rating> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "s" | "safe" | "sfw" | "general" | "g" => Some(Rating::Sfw),
        "q" | "questionable" | "suggestive" | "sensitive" | "mature" => Some(Rating::Questionable),
        "e" | "x" | "explicit" | "nsfw" | "porn" | "adult" => Some(Rating::Explicit),
        _ => None,
    }
}

/// Fall back to tag heuristics when the platform gives us nothing.
pub fn rating_from_tags(tags: &[String]) -> Rating {
    const EXPLICIT_HINTS: &[&str] = &[
        "explicit",
        "nsfw",
        "porn",
        "hentai",
        "nude",
        "naked",
        "sex",
        "xxx",
        "cum",
        "genitalia",
        "penis",
        "vagina",
        "anal",
        "oral",
        "masturbation",
        "orgasm",
        "creampie",
    ];
    const SUGGESTIVE_HINTS: &[&str] = &[
        "suggestive",
        "questionable",
        "ecchi",
        "lingerie",
        "underwear",
        "panties",
        "topless",
        "cleavage",
        "lewd",
        "seductive",
        "bikini",
        "swimsuit",
        "bondage",
        "nsfw_light",
    ];
    let has = |hints: &[&str]| tags.iter().any(|t| hints.iter().any(|h| t.contains(h)));
    if has(EXPLICIT_HINTS) {
        Rating::Explicit
    } else if has(SUGGESTIVE_HINTS) {
        Rating::Questionable
    } else {
        Rating::Sfw
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        match value.get(*key) {
            Some(Value::String(s)) if !s.trim().is_empty() => return Some(s.trim().to_string()),
            Some(Value::Number(n)) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

/// `first_string` that also accepts dotted paths, for the shapes where gallery-dl
/// nests the author (`{"user": {"name": …, "nick": …}}`).
fn first_string_path(value: &Value, paths: &[&str]) -> Option<String> {
    for path in paths {
        let mut cursor = value;
        let mut resolved = true;
        for key in path.split('.') {
            match cursor.get(key) {
                Some(next) => cursor = next,
                None => {
                    resolved = false;
                    break;
                }
            }
        }
        if !resolved {
            continue;
        }
        match cursor {
            Value::String(s) if !s.trim().is_empty() => return Some(s.trim().to_string()),
            Value::Number(n) => return Some(n.to_string()),
            _ => {}
        }
    }
    None
}

/// Any truthy "this one is sensitive" flag anywhere in the metadata. gallery-dl
/// nests them (a retweet or quote carries its own object, media items carry a
/// `sensitive` flag of their own), and the cost of missing one is adult media
/// reaching a channel that believed the post was sfw — so scan the whole tree.
///
/// `pair` calls this per staged file, so a flag on one image of a tweet is kept
/// per item instead of being flattened into the post.
pub fn sensitive(value: &Value) -> bool {
    const KEYS: &[&str] = &[
        "possibly_sensitive",
        "sensitive",
        "nsfw",
        "is_nsfw",
        "adult",
        "explicit_content",
    ];
    match value {
        Value::Object(map) => map.iter().any(|(key, val)| {
            (KEYS.contains(&key.to_ascii_lowercase().as_str()) && truthy(val)) || sensitive(val)
        }),
        Value::Array(items) => items.iter().any(sensitive),
        _ => false,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64().unwrap_or_default() != 0,
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "true" | "yes" | "1" | "sensitive" | "nsfw"
        ),
        _ => false,
    }
}

/// Tags may be an array, a comma/space separated string, or an object with arrays.
fn collect_tags(value: &Value, keys: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // A single string is a space/comma separated list (booru `tag_string`),
    // while array elements are already one tag each and must not be split.
    fn split_list(raw: &str) -> Vec<String> {
        raw.split([',', ' ', '\t', '\n'])
            .filter(|t| !t.trim().is_empty())
            .map(|t| t.to_string())
            .collect()
    }

    for key in keys {
        match value.get(*key) {
            Some(Value::Array(items)) => {
                for item in items {
                    match item {
                        Value::String(name) => out.push(name.clone()),
                        Value::Object(obj) => {
                            if let Some(Value::String(name)) = obj.get("name") {
                                out.push(name.clone());
                            } else if let Some(Value::String(tag)) = obj.get("tag") {
                                out.push(tag.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(Value::String(s)) => out.extend(split_list(s)),
            Some(Value::Object(map)) => {
                for (_, nested) in map {
                    match nested {
                        Value::String(s) => out.push(s.clone()),
                        Value::Array(items) => {
                            for item in items {
                                if let Value::String(s) = item {
                                    out.push(s.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    tags::normalize_all(out)
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(n) = value.as_i64() {
        // seconds or milliseconds since epoch
        let secs = if n > 100_000_000_000 { n / 1000 } else { n };
        return Utc.timestamp_opt(secs, 0).single();
    }
    if let Some(s) = value.as_str() {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = DateTime::parse_from_str(s, "%a %b %d %H:%M:%S %z %Y") {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%#z") {
            return Some(dt.with_timezone(&Utc));
        }
        if let Ok(dt) = DateTime::parse_from_str(&format!("{s} +0000"), "%Y-%m-%d %H:%M:%S %z") {
            return Some(dt.with_timezone(&Utc));
        }
    }
    None
}

fn first_timestamp(value: &Value, keys: &[&str]) -> Option<DateTime<Utc>> {
    for key in keys {
        if let Some(v) = value.get(*key)
            && let Some(dt) = parse_timestamp(v)
        {
            return Some(dt);
        }
    }
    None
}

/// Strip trailing hashtag spam blocks and collapse whitespace in descriptions.
pub fn tidy_description(raw: &str) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in raw.replace('\r', "").lines() {
        if line.trim().is_empty() {
            // keep the paragraph break; the fold below collapses runs of them
            lines.push(String::new());
            continue;
        }
        let scrubbed = scrub_tokens(line.trim_end());
        if scrubbed.trim().is_empty() {
            // the line was nothing but hashtags/handles
            continue;
        }
        lines.push(scrubbed);
    }
    let joined = lines.join("\n");
    let squeezed = joined
        .lines()
        .fold(Vec::<String>::new(), |mut acc: Vec<String>, line| {
            if line.trim().is_empty() {
                if !acc.last().map(|l: &String| l.is_empty()).unwrap_or(true) {
                    acc.push(String::new());
                }
            } else {
                acc.push(line.trim().to_string());
            }
            acc
        });
    let text = squeezed.join("\n").trim().to_string();
    // a post body is shown in one caption: keep it bounded
    let text = crate::render::truncate::shorten(&text, DESCRIPTION_LIMIT);
    if text.is_empty() { None } else { Some(text) }
}

/// Longest stored description; longer ones are cut with an ellipsis.
pub const DESCRIPTION_LIMIT: usize = 1000;

/// Drop `#hashtag` and `@mention` tokens: Telegram treats both as markup and a
/// caption repeating them reads badly. Only a leading marker counts, so `C#` and
/// `user@host` survive.
fn scrub_tokens(line: &str) -> String {
    line.split_whitespace()
        .filter(|token| !(token.starts_with('#') || token.starts_with('@')))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build a post from one gallery-dl metadata object.
pub fn from_gallery_metadata(
    meta: &Value,
    files: Vec<crate::source::PostFile>,
    fallback_source: &str,
    fallback_url: Option<&str>,
) -> Option<NormalizedPost> {
    let category = first_string(meta, &["category"]).unwrap_or_else(|| fallback_source.to_string());
    let source = normalize_source_name(&category);

    let source_id = first_string(
        meta,
        &[
            "id",
            "post_id",
            "tweet_id",
            "media_id",
            "illust_id",
            "artwork_id",
            "status_id",
            "shortcode",
            "pageid",
            "sha1",
            "filename",
        ],
    )?;

    let mut post = NormalizedPost::new(source.clone(), source_id);

    post.url = first_string(meta, &["post_url", "url", "webpage_url", "tweet_url"])
        .or_else(|| fallback_url.map(|u| u.to_string()));

    // `title` and `caption` overlap on several sites: never duplicate one into the other.
    post.title = first_string(meta, &["title", "subject"]).and_then(|t| tidy_description(&t));
    post.description = first_string(
        meta,
        &["description", "content", "commentary", "note", "caption"],
    )
    .and_then(|d| tidy_description(&d))
    .filter(|d| Some(d) != post.title.as_ref());
    if post.title.is_none() && post.description.is_none() {
        post.title = first_string(meta, &["caption"]).and_then(|t| tidy_description(&t));
    }

    let mut tags = collect_tags(
        meta,
        &[
            "tags",
            "tag_string_general",
            "tag_string_character",
            "hashtags",
            "tag_string",
            "tag_string_copyright",
            "tag_string_artist",
            "tag_string_meta",
        ],
    );
    if let Some(name) = &post.title {
        for word in name.split_whitespace().filter(|w| w.starts_with('#')) {
            if let Some(tag) = tags::normalize(word) {
                tags.push(tag);
            }
        }
        tags = tags::normalize_all(tags);
    }
    post.tags = tags;

    // rating: explicit field first, then flags, then tag heuristics
    let mut rating = first_string(meta, &["rating", "x_restrict", "safety", "rating_general"])
        .and_then(|r| rating_from_words(&r))
        // the platform's own "sensitive" flag beats guessing from tags
        // the platform's own "sensitive" flag beats guessing from tags, and a
        // flag that only sits on one staged item still rates the whole post
        .or_else(|| {
            (sensitive(meta) || files.iter().any(|file| file.sensitive)).then_some(Rating::Explicit)
        });
    if rating.is_none()
        && let Some(x) = meta.get("x_restrict").and_then(|v| v.as_i64())
    {
        rating = Some(match x {
            0 => Rating::Sfw,
            1 => Rating::Questionable,
            _ => Rating::Explicit,
        });
    }
    post.rating = rating.unwrap_or_else(|| rating_from_tags(&post.tags));

    post.posted_at = first_timestamp(
        meta,
        &[
            "date",
            "posted_at",
            "created_at",
            "upload_date",
            "timestamp",
            "published_at",
            "create_date",
        ],
    );

    // gallery-dl's twitter extractor reports the handle as `username` and the
    // display name as `fullname` / `user.name`. The handle is the stable ASCII
    // identity (and the only thing present for some tweets), so on twitter the
    // artist *is* the handle: name stays empty and `Author::display` falls back.
    let twitter = source == "twitter";
    let author_handle = first_string_path(
        meta,
        &[
            "screen_name",
            "user_account",
            "handle",
            "user_nick",
            "username",
            "user.screen_name",
            "user.username",
            "user.nick",
        ],
    );
    let author_name = if twitter {
        None
    } else {
        first_string_path(
            meta,
            &[
                "user_name",
                "artist_name",
                "creator_name",
                "author_name",
                "display_name",
                "nick",
                "full_name",
                "artist",
                "uploader",
                "user.name",
            ],
        )
    };
    let author_id = first_string_path(
        meta,
        &[
            "user_id",
            "userid",
            "artist_id",
            "creator_id",
            "uid",
            "author_id",
            "user.id",
        ],
    )
    .or_else(|| author_handle.clone());
    if author_id.is_some() || author_name.is_some() || author_handle.is_some() {
        post.author = Some(PostAuthor {
            source_id: author_id.unwrap_or_else(|| "unknown".to_string()),
            name: author_name,
            handle: author_handle,
            url: first_string(
                meta,
                &["user_url", "artist_url", "creator_url", "author_url"],
            ),
        });
    }

    post.files = files;
    post.raw_json = serde_json::to_string(meta).ok();
    Some(post)
}

pub fn normalize_source_name(category: &str) -> String {
    let raw = category.trim().to_ascii_lowercase();
    let name = raw.trim_start_matches("www.").trim_end_matches('.');
    // accept both extractor names ("pixiv") and bare hosts ("www.pixiv.net")
    let mapped = if name == "x" || name.ends_with("x.com") || name.ends_with("twitter.com") {
        "twitter"
    } else if name.ends_with("pixiv.net") || name.contains("pixiv") {
        "pixiv"
    } else if name.ends_with("danbooru.donmai.us") || name.contains("danbooru") {
        "danbooru"
    } else if name.contains("gelbooru") {
        "gelbooru"
    } else if name.contains("artstation") {
        "artstation"
    } else if name.contains("deviantart") {
        "deviantart"
    } else if name.contains("mastodon") || name == "fediverse" {
        "mastodon"
    } else if name.contains("bsky") || name.contains("bluesky") {
        "bluesky"
    } else if name.contains("tumblr") {
        "tumblr"
    } else if name.is_empty() {
        "unknown"
    } else {
        return name.to_string();
    };
    mapped.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rating_words() {
        assert_eq!(rating_from_words("s"), Some(Rating::Sfw));
        assert_eq!(rating_from_words("sensitive"), Some(Rating::Questionable));
        assert_eq!(rating_from_words("e"), Some(Rating::Explicit));
        assert_eq!(rating_from_words("wat"), None);
    }

    #[test]
    fn tag_heuristics() {
        assert_eq!(rating_from_tags(&["nsfw".into()]), Rating::Explicit);
        assert_eq!(rating_from_tags(&["lingerie".into()]), Rating::Questionable);
        assert_eq!(rating_from_tags(&["landscape".into()]), Rating::Sfw);
    }

    #[test]
    fn pixiv_shape() {
        let meta = json!({
            "category": "pixiv",
            "id": 123456,
            "title": "Sunset",
            "caption": "a warm evening",
            "tags": ["original", "landscape", "sunset"],
            "user_name": "SomeArtist",
            "user_id": 4242,
            "x_restrict": 1,
            "date": "2024-05-01T12:00:00+00:00",
            "post_url": "https://www.pixiv.net/artworks/123456"
        });
        let post = from_gallery_metadata(&meta, vec![], "pixiv", None).unwrap();
        assert_eq!(post.source, "pixiv");
        assert_eq!(post.source_id, "123456");
        assert_eq!(post.rating, Rating::Questionable);
        assert_eq!(
            post.author.as_ref().unwrap().name.as_deref(),
            Some("SomeArtist")
        );
        assert!(post.tags.contains(&"landscape".to_string()));
        assert_eq!(post.description.as_deref(), Some("a warm evening"));
        assert!(post.posted_at.is_some());
    }

    #[test]
    fn twitter_shape() {
        let meta = json!({
            "category": "twitter",
            "tweet_id": 99,
            "content": "line one\n#art #test",
            "username": "handle",
            "possibly_sensitive": true,
            "hashtags": ["art", "test"],
            "date": 1714564800,
        });
        let post = from_gallery_metadata(&meta, vec![], "twitter", None).unwrap();
        assert_eq!(post.source, "twitter");
        assert_eq!(post.source_id, "99");
        assert_eq!(post.rating, Rating::Explicit);
        assert_eq!(post.description.as_deref(), Some("line one"));
        // the handle is the artist, and doubles as the id without a user_id
        let author = post.author.as_ref().unwrap();
        assert_eq!(author.handle.as_deref(), Some("handle"));
        assert_eq!(author.name, None);
        assert_eq!(author.source_id, "handle");
        assert_eq!(author.display(), "handle");
    }

    #[test]
    fn twitter_author_survives_both_gallery_dl_shapes() {
        // modern shape: flat user_id plus a display name we deliberately ignore
        let flat = json!({
            "category": "twitter",
            "tweet_id": 1,
            "username": "asobinin",
            "fullname": "アソビニン",
            "user_id": 4242,
        });
        let post = from_gallery_metadata(&flat, vec![], "twitter", None).unwrap();
        let author = post.author.unwrap();
        assert_eq!(author.source_id, "4242");
        assert_eq!(author.handle.as_deref(), Some("asobinin"));
        assert_eq!(
            author.name, None,
            "twitter keeps the handle, not the display name"
        );

        // older shape: everything nested under `user`
        let nested = json!({
            "category": "twitter",
            "tweet_id": 2,
            "user": { "id": 77, "nick": "nested_handle", "name": "Nested Name" },
        });
        let post = from_gallery_metadata(&nested, vec![], "twitter", None).unwrap();
        let label = post.author.as_ref().unwrap().display();
        let author = post.author.unwrap();
        assert_eq!(author.source_id, "77");
        assert_eq!(author.handle.as_deref(), Some("nested_handle"));
        assert_eq!(label, "nested_handle");

        // other sources still prefer the human name
        let pixiv = json!({
            "category": "pixiv",
            "illust_id": 5,
            "user_name": "Nightjar",
            "user_account": "nightjar_",
            "user_id": 9,
        });
        let post = from_gallery_metadata(&pixiv, vec![], "pixiv", None).unwrap();
        assert_eq!(post.author.as_ref().unwrap().display(), "Nightjar");
    }

    #[test]
    fn sensitive_flag_is_found_where_gallery_dl_nests_it() {
        let rating = |meta: Value| {
            from_gallery_metadata(&meta, vec![], "twitter", None)
                .unwrap()
                .rating
        };

        // flat, the usual tweet shape
        assert_eq!(
            rating(json!({"tweet_id": 1, "possibly_sensitive": true})),
            Rating::Explicit
        );
        // a retweet carries the original tweet in its own object
        assert_eq!(
            rating(json!({
                "tweet_id": 2,
                "retweet": {"tweet_id": 1, "possibly_sensitive": true},
            })),
            Rating::Explicit,
            "a nested retweet flag must not be lost"
        );
        // media items can be flagged one by one
        assert_eq!(
            rating(json!({
                "tweet_id": 3,
                "media": [{"url": "a.jpg", "sensitive": true}],
            })),
            Rating::Explicit
        );
        // extractors that hand the flag over as a string
        assert_eq!(
            rating(json!({"tweet_id": 4, "possibly_sensitive": "true"})),
            Rating::Explicit
        );

        // and an explicit "no" stays sfw: no false positives
        assert_eq!(
            rating(json!({
                "tweet_id": 5,
                "possibly_sensitive": false,
                "media": [{"sensitive": "no"}],
            })),
            Rating::Sfw
        );
    }

    #[test]
    fn tidy_drops_tag_only_lines() {
        let out = tidy_description("hello\n\n\n#art #nsfw\n  world  \n").unwrap();
        assert_eq!(out, "hello\n\nworld");
        assert!(tidy_description("#only #tags").is_none());
    }

    #[test]
    fn tidy_strips_hashtags_and_mentions_and_caps_length() {
        let out = tidy_description("great art #drawing @friend\n\n\n\nmore #art").unwrap();
        assert_eq!(
            out, "great art\n\nmore",
            "tags removed, one blank line kept"
        );
        // a marker inside a token is not a tag
        assert_eq!(
            tidy_description("written in C#, by me@example.com").unwrap(),
            "written in C#, by me@example.com"
        );
        let long = "word ".repeat(400);
        let capped = tidy_description(&long).unwrap();
        assert!(
            capped.chars().count() <= DESCRIPTION_LIMIT,
            "got {} chars",
            capped.chars().count()
        );
        assert!(capped.ends_with('…'), "a cut body ends with an ellipsis");
    }

    #[test]
    fn missing_id_is_rejected() {
        assert!(from_gallery_metadata(&json!({"title": "x"}), vec![], "pixiv", None).is_none());
    }

    #[test]
    fn a_sensitive_file_rates_the_post_without_a_metadata_flag() {
        // the flag lives on one image's sidecar, the tweet metadata is clean
        let meta = json!({ "tweet_id": 42, "username": "asobinin" });
        let files = vec![crate::source::PostFile {
            path: std::path::PathBuf::from("/tmp/a.png"),
            is_video: false,
            sensitive: true,
            meta: None,
        }];
        let post = from_gallery_metadata(&meta, files, "twitter", None).unwrap();
        assert_eq!(post.rating, Rating::Explicit);
    }
}
