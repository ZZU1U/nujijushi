//! Map messy per-site metadata (gallery-dl output) onto [`NormalizedPost`].

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::db::tags;
use crate::model::Rating;
use crate::source::{NormalizedPost, PostAuthor};

use super::fanbox;

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

/// `first_string_path`, but only a value `accept` agrees with.
fn first_string_path_matching(
    value: &Value,
    paths: &[&str],
    accept: impl Fn(&str) -> bool,
) -> Option<String> {
    paths
        .iter()
        .filter_map(|path| first_string_path(value, &[*path]))
        .find(|candidate| accept(candidate))
}

/// Twitter screen names are `[A-Za-z0-9_]` (1–50 chars, optional leading `@`).
///
/// Display names are free text — `tamanegi@FANBOX` is one — so this is the check
/// that keeps a display name out of the handle slot.
fn looks_like_screen_name(raw: &str) -> bool {
    let name = raw.trim().trim_start_matches('@');
    !name.is_empty()
        && name.len() <= 50
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `@handle` in `https://x.com/<handle>/status/<id>`.
///
/// Last resort when the metadata has no usable handle. Twitter's non-profile
/// paths (`/i/`, `/home`, …) answer `None`; a repost's URL names the account that
/// posted it, which is the honest answer for a shape we cannot resolve.
fn handle_from_url(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let (host, tail) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    if !(host.ends_with("x.com") || host.ends_with("twitter.com")) {
        return None;
    }
    let first = tail.split(['/', '?', '#']).next()?;
    match first {
        "" | "i" | "home" | "search" | "explore" | "hashtag" | "intent" | "share" | "login"
        | "settings" => None,
        handle if looks_like_screen_name(handle) => Some(handle.to_string()),
        _ => None,
    }
}

/// Is this display name just the handle, spelled differently (`@Name` vs `name`)?
/// Such a name adds nothing: captions print `**name** (handle)`.
fn same_handle(name: &str, handle: Option<&str>) -> bool {
    let Some(handle) = handle else {
        return false;
    };
    let clean = |value: &str| value.trim().trim_start_matches('@').to_lowercase();
    !clean(handle).is_empty() && clean(name) == clean(handle)
}

/// Any truthy "this one is sensitive" flag anywhere in the metadata. gallery-dl
/// nests them (a retweet or quote carries its own object, media items carry a
/// `sensitive` flag of their own), and the cost of missing one is adult media
/// reaching a channel that believed the post was sfw — so scan the whole tree.
///
/// `pair` calls this per staged file, so a flag on one image of a tweet is kept
/// per item instead of being flattened into the post.
pub fn sensitive(value: &Value) -> bool {
    // On FANBOX the post and the creator can each have a separate adult flag.
    value.get("hasAdultContent").is_some_and(truthy) || has_sensitive_flag(value)
}

fn has_sensitive_flag(value: &Value) -> bool {
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
            (KEYS.contains(&key.to_ascii_lowercase().as_str()) && truthy(val))
                || has_sensitive_flag(val)
        }),
        Value::Array(items) => items.iter().any(has_sensitive_flag),
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

pub(crate) fn copyright_tags(metadata: &Value) -> Vec<String> {
    collect_tags(metadata, &["tag_string_copyright", "tags_copyright"])
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
    let is_fanbox = source == "fanbox";

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

    if source == "pixiv"
        && !category.contains("novel")
        && !post
            .url
            .as_deref()
            .is_some_and(|url| url.contains("/novel/"))
        && !fallback_url.is_some_and(|url| url.contains("/novel/"))
        && !meta
            .get("subcategory")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.contains("novel"))
    {
        let artwork_id = first_string(meta, &["illust_id", "artwork_id", "id", "post_id"]);
        if let Some(id) = artwork_id.filter(|id| {
            id.bytes().all(|byte| byte.is_ascii_digit()) && !id.trim_start_matches('0').is_empty()
        }) {
            post.url = Some(format!("https://www.pixiv.net/artworks/{id}"));
        }
    }

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

    // FANBOX retains the original ISO timestamp, including its timezone.
    let fanbox_date = if is_fanbox {
        first_timestamp(meta, &["publishedDatetime"])
    } else {
        None
    };
    post.posted_at = fanbox_date.or_else(|| {
        first_timestamp(
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
        )
    });

    // gallery-dl's author fields are source-specific, and two of them disagree:
    //   twitter (extractor/twitter.py): `user.name` is the *screen name* and
    //     `user.nick` the display name (`"name": core["screen_name"],
    //     "nick": core["name"]`) — the display name is free text and often
    //     contains `@`, so it must never be mistaken for the handle;
    //   pixiv: `user.account` is the handle, `user.name` the display name.
    // So the handle and the name come from separate, source-aware lists, a
    // twitter handle has to *look* like one, and the URL is the last resort.
    let twitter = source == "twitter" || source == "x";
    let handle_keys: &[&str] = if twitter {
        &[
            // raw GraphQL user (gallery-dl's default, `metadata-user = false`):
            // `{"core": {"screen_name": handle, "name": display}}`
            "user.core.screen_name",
            "author.core.screen_name",
            "user.legacy.screen_name",
            "author.legacy.screen_name",
            // flattened user (`metadata-user = true`): name = screen name
            "user.name",
            "author.name",
            "screen_name",
            "user.screen_name",
            "user_account",
            "handle",
            "username",
            "user.username",
            "user_nick",
            "user.nick",
        ]
    } else {
        &[
            "user.account",
            "user_account",
            "screen_name",
            "user.screen_name",
            "handle",
            "user_nick",
            "username",
            "user.username",
            "user.nick",
        ]
    };
    let author_handle = if twitter {
        first_string_path_matching(meta, handle_keys, looks_like_screen_name)
            .or_else(|| post.url.as_deref().and_then(handle_from_url))
    } else if is_fanbox {
        meta.get("creatorId")
            .and_then(Value::as_str)
            .and_then(fanbox::creator_id)
            .map(str::to_ascii_lowercase)
    } else {
        first_string_path(meta, handle_keys)
    };
    let name_keys: &[&str] = if twitter {
        &[
            // raw GraphQL user: the display name sits next to the screen name
            "user.core.name",
            "author.core.name",
            "user.legacy.name",
            "author.legacy.name",
            // flattened user: `nick` is the display name
            "user.nick",
            "author.nick",
            "user_name",
            "fullname",
            "display_name",
            "full_name",
            "nick",
            "user.name",
        ]
    } else {
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
        ]
    };
    let author_name = first_string_path_matching(meta, name_keys, |name| {
        !same_handle(name, author_handle.as_deref())
    });
    let mut author_id = first_string_path(
        meta,
        &[
            "user_id",
            "userid",
            "artist_id",
            "creator_id",
            "uid",
            "author_id",
            "user.id",
            // raw GraphQL user: the numeric id is `rest_id`
            "user.rest_id",
            "author.rest_id",
        ],
    )
    .or_else(|| author_handle.clone());
    let mut author_url = first_string(
        meta,
        &["user_url", "artist_url", "creator_url", "author_url"],
    );
    if is_fanbox {
        if let Some(creator) = author_handle.as_deref() {
            author_id = Some(creator.to_owned());
            author_url = Some(fanbox::creator_url(creator));
        } else {
            author_id = author_id.or_else(|| first_string_path(meta, &["user.userId"]));
        }
    }
    if author_id.is_some() || author_name.is_some() || author_handle.is_some() {
        post.author = Some(PostAuthor {
            source_id: author_id.unwrap_or_else(|| "unknown".to_string()),
            name: author_name,
            handle: author_handle,
            url: author_url,
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
    let mapped = if matches!(name, "fanbox" | "fanbox.cc") || name.ends_with(".fanbox.cc") {
        "fanbox"
    } else if name == "x" || name.ends_with("x.com") || name.ends_with("twitter.com") {
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
    fn pixiv_artwork_urls_replace_image_links_on_import() {
        let cdn = "https://i.pximg.net/img-original/123_p0.png";
        let artwork = json!({"category":"pixiv", "id":123, "url":cdn});
        let artwork = from_gallery_metadata(&artwork, vec![], "pixiv", Some(cdn)).unwrap();
        assert_eq!(
            artwork.url.as_deref(),
            Some("https://www.pixiv.net/artworks/123")
        );
        let unknown = json!({"category":"pixiv", "id":"filename", "url":cdn});
        let unknown = from_gallery_metadata(&unknown, vec![], "pixiv", None).unwrap();
        assert_eq!(unknown.url.as_deref(), Some(cdn));
        let novel = json!({"category":"pixiv", "subcategory":"novel", "id":123, "url":cdn});
        let novel = from_gallery_metadata(&novel, vec![], "pixiv", None).unwrap();
        assert_eq!(novel.url.as_deref(), Some(cdn));
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

    /// gallery-dl's twitter user object is `{name: <screen name>, nick: <display
    /// name>}` (`extractor/twitter.py`: `"name": core["screen_name"], "nick":
    /// core["name"]`). Swapping the two put a display name in the handle slot,
    /// which is exactly what `@tamanegi@FANBOX` was.
    #[test]
    fn twitter_author_survives_both_gallery_dl_shapes() {
        // modern shape: flat handle plus a display name, which is kept now
        let flat = json!({
            "category": "twitter",
            "tweet_id": 1,
            "username": "asobinin",
            "fullname": "アソビニン",
            "user_id": 4242,
        });
        let author = from_gallery_metadata(&flat, vec![], "twitter", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(author.source_id, "4242");
        assert_eq!(author.handle.as_deref(), Some("asobinin"));
        assert_eq!(author.name.as_deref(), Some("アソビニン"));
        assert_eq!(author.display(), "アソビニン");

        // nested shape: `user.name` is the handle, `user.nick` the display name
        let nested = json!({
            "category": "twitter",
            "tweet_id": 2,
            "user": { "id": 77, "name": "nested_handle", "nick": "Nested Name" },
        });
        let author = from_gallery_metadata(&nested, vec![], "twitter", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(author.source_id, "77");
        assert_eq!(author.handle.as_deref(), Some("nested_handle"));
        assert_eq!(author.name.as_deref(), Some("Nested Name"));
        assert_eq!(author.display(), "Nested Name");

        // other sources still prefer the human name, and pixiv's account is the
        // handle (gallery-dl's pixiv user dict: `account` + `name`)
        let pixiv = json!({
            "category": "pixiv",
            "illust_id": 5,
            "user_name": "Nightjar",
            "user_account": "nightjar_",
            "user_id": 9,
        });
        let author = from_gallery_metadata(&pixiv, vec![], "pixiv", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(author.display(), "Nightjar");
        assert_eq!(author.handle.as_deref(), Some("nightjar_"));
    }

    /// The reported case: the display name carries an `@`, and the handle only
    /// exists in the URL (`x.com/<handle>/status/…`).
    #[test]
    fn a_display_name_never_becomes_the_twitter_handle() {
        let meta = json!({
            "category": "twitter",
            "tweet_id": 3,
            "post_url": "https://x.com/tamaneg2346/status/3",
            "user": { "id": 7, "name": "tamanegi@FANBOX", "nick": "tamanegi@FANBOX" },
        });
        let author = from_gallery_metadata(&meta, vec![], "twitter", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(
            author.handle.as_deref(),
            Some("tamaneg2346"),
            "the display name is not a handle, so the URL answers"
        );
        assert_eq!(author.name.as_deref(), Some("tamanegi@FANBOX"));
        assert_eq!(author.display(), "tamanegi@FANBOX");

        // a handle that only lives in `nick` is still accepted (defensive: some
        // shapes put the screen name there)
        let only_nick = json!({
            "category": "twitter",
            "tweet_id": 4,
            "user": { "id": 8, "nick": "asobinin" },
        });
        let author = from_gallery_metadata(&only_nick, vec![], "twitter", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(author.handle.as_deref(), Some("asobinin"));

        // a display name that *is* the handle is dropped, so captions cannot
        // print `**@x** (@x)`
        let same = json!({
            "category": "twitter",
            "tweet_id": 5,
            "user": { "id": 9, "name": "asobinin", "nick": "@Asobinin" },
        });
        let author = from_gallery_metadata(&same, vec![], "twitter", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(author.handle.as_deref(), Some("asobinin"));
        assert_eq!(author.name, None, "the same word twice is noise");
    }

    /// gallery-dl's *default* emits the raw GraphQL user object
    /// (`metadata-user = false` → `_transform_user = util.identity`), where the
    /// handle is `core.screen_name` and the display name `core.name`.
    #[test]
    fn the_raw_graphql_user_shape_is_understood_too() {
        let meta = json!({
            "category": "twitter",
            "tweet_id": 6,
            "user": {
                "rest_id": "11",
                "core": { "screen_name": "tamaneg2346", "name": "tamanegi@FANBOX" },
            },
        });
        let author = from_gallery_metadata(&meta, vec![], "twitter", None)
            .unwrap()
            .author
            .unwrap();
        assert_eq!(author.source_id, "11", "rest_id is the numeric id");
        assert_eq!(author.handle.as_deref(), Some("tamaneg2346"));
        assert_eq!(author.name.as_deref(), Some("tamanegi@FANBOX"));
    }

    #[test]
    fn the_url_only_answers_for_a_twitter_profile_path() {
        assert_eq!(
            handle_from_url("https://twitter.com/SomeOne/status/1"),
            Some("SomeOne".into())
        );
        assert_eq!(
            handle_from_url("https://x.com/SomeOne/status/1?s=20"),
            Some("SomeOne".into())
        );
        // twitter's own non-profile paths, and other sites, are not handles
        assert_eq!(handle_from_url("https://x.com/i/status/1"), None);
        assert_eq!(handle_from_url("https://x.com/home"), None);
        assert_eq!(handle_from_url("https://t.me/night_art/42"), None);
        assert_eq!(handle_from_url("not a url"), None);
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
