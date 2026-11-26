use chrono::{DateTime, Utc};
use serde_json::Value;
use url::Url;

use super::{Error, numeric_id, valid_handle};

pub struct Status {
    pub id: u64,
    pub author: Author,
    pub text: String,
    pub hashtags: Vec<String>,
    pub posted_at: DateTime<Utc>,
    pub sensitive: bool,
    pub media: Vec<Media>,
}

pub struct Author {
    pub id: u64,
    pub handle: Option<String>,
    pub name: Option<String>,
}

pub struct Media {
    pub kind: MediaKind,
    pub url: Url,
    pub extension: &'static str,
    pub sensitive: bool,
    pub metadata: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
}

pub(super) fn parse(response: Value, id: u64) -> Result<Status, Error> {
    if let Some(errors) = response.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        return Err(graphql_error(errors));
    }
    let instructions = response
        .pointer("/data/threaded_conversation_with_injections_v2/instructions")
        .and_then(Value::as_array)
        .ok_or(Error::Response("post timeline"))?;
    let (tweet, wrapped) = focal(instructions, id)?;
    let legacy = tweet
        .get("legacy")
        .ok_or(Error::Response("post metadata"))?;
    let user = tweet.pointer("/core/user_results/result");
    let user_id = user
        .and_then(|user| {
            user.get("rest_id")
                .or_else(|| user.pointer("/legacy/id_str"))
        })
        .and_then(as_id);
    let legacy_user_id = legacy.get("user_id_str").and_then(as_id);
    if matches!((user_id, legacy_user_id), (Some(a), Some(b)) if a != b) {
        return Err(Error::Response("post author identity"));
    }
    let author = Author {
        id: user_id
            .or(legacy_user_id)
            .ok_or(Error::Response("post author identity"))?,
        handle: user
            .and_then(|user| {
                ["/core/screen_name", "/legacy/screen_name"]
                    .iter()
                    .find_map(|path| {
                        user.pointer(path)
                            .and_then(Value::as_str)
                            .filter(|handle| valid_handle(handle))
                    })
            })
            .map(str::to_owned),
        name: user
            .and_then(|user| {
                user.pointer("/core/name")
                    .or_else(|| user.pointer("/legacy/name"))
            })
            .and_then(Value::as_str)
            .map(str::to_owned),
    };
    let note = tweet.pointer("/note_tweet/note_tweet_results/result");
    let text = note
        .and_then(|note| note.get("text"))
        .or_else(|| legacy.get("full_text"))
        .and_then(Value::as_str)
        .ok_or(Error::Response("post text"))?
        .to_owned();
    let hashtags = [
        legacy.pointer("/entities/hashtags"),
        note.and_then(|note| note.pointer("/entity_set/hashtags")),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_array)
    .flatten()
    .filter_map(|tag| tag.get("text").and_then(Value::as_str).map(str::to_owned))
    .collect();
    let posted_at = legacy
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(|text| DateTime::parse_from_str(text, "%a %b %d %H:%M:%S %z %Y").ok())
        .map(|date| date.to_utc())
        .ok_or(Error::Response("post timestamp"))?;
    let sensitive = [tweet, legacy, wrapped].iter().any(|value| {
        value.get("possibly_sensitive").and_then(Value::as_bool) == Some(true) || warning(value)
    });

    if tweet
        .get("retweeted_status_result")
        .is_some_and(|value| !value.is_null())
        || legacy
            .get("retweeted_status_result")
            .is_some_and(|value| !value.is_null())
        || legacy
            .get("retweeted_status_id_str")
            .and_then(as_id)
            .is_some()
    {
        return Err(Error::UnsupportedMedia);
    }
    let attachments = legacy
        .pointer("/extended_entities/media")
        .or_else(|| legacy.pointer("/entities/media"));
    let attachments = match attachments {
        Some(value) => value
            .as_array()
            .ok_or(Error::Response("post attachments"))?
            .as_slice(),
        None => &[],
    };
    if attachments.is_empty() {
        return Err(
            if legacy.get("is_quote_status").and_then(Value::as_bool) == Some(true)
                || tweet
                    .get("quoted_status_result")
                    .is_some_and(|value| !value.is_null())
                || legacy.get("quoted_status_id_str").and_then(as_id).is_some()
            {
                Error::UnsupportedMedia
            } else {
                Error::NoMedia
            },
        );
    }
    let media = attachments
        .iter()
        .map(|item| attachment(item, id))
        .collect::<Result<_, _>>()?;
    Ok(Status {
        id,
        author,
        text,
        hashtags,
        posted_at,
        sensitive,
        media,
    })
}

fn graphql_error(errors: &[Value]) -> Error {
    let codes = errors
        .iter()
        .filter_map(|error| error.get("code")?.as_i64());
    for code in codes {
        match code {
            32 | 89 | 99 | 215 | 326 => return Error::Authentication,
            88 => return Error::RateLimited,
            34 | 63 | 144 | 179 | 220 => return Error::Unavailable,
            _ => {}
        }
    }
    Error::Response("post detail")
}

fn focal(instructions: &[Value], id: u64) -> Result<(&Value, &Value), Error> {
    let entry_id = format!("tweet-{id}");
    for instruction in instructions {
        let entries = instruction.get("entries").and_then(Value::as_array);
        for entry in entries
            .into_iter()
            .flatten()
            .chain(instruction.get("entry"))
        {
            if let Some(result) = entry.pointer("/content/itemContent/tweet_results/result")
                && let Some(found) = matching(entry, result, &entry_id, id)?
            {
                return Ok(found);
            }
            if let Some(items) = entry.pointer("/content/items").and_then(Value::as_array)
                && let Some(found) = find_in_module(items, &entry_id, id)?
            {
                return Ok(found);
            }
        }
        if let Some(items) = instruction.get("moduleItems").and_then(Value::as_array)
            && let Some(found) = find_in_module(items, &entry_id, id)?
        {
            return Ok(found);
        }
    }
    Err(Error::Unavailable)
}

fn find_in_module<'a>(
    items: &'a [Value],
    entry_id: &str,
    id: u64,
) -> Result<Option<(&'a Value, &'a Value)>, Error> {
    for item in items {
        if let Some(result) = item.pointer("/item/itemContent/tweet_results/result")
            && let Some(found) = matching(item, result, entry_id, id)?
        {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn matching<'a>(
    entry: &Value,
    result: &'a Value,
    entry_id: &str,
    id: u64,
) -> Result<Option<(&'a Value, &'a Value)>, Error> {
    let exact_entry = entry.get("entryId").and_then(Value::as_str) == Some(entry_id);
    let tweet = result.get("tweet").unwrap_or(result);
    let identities = [
        tweet.get("rest_id"),
        tweet.pointer("/legacy/id_str"),
        result.get("rest_id"),
    ];
    let matching_id = identities
        .iter()
        .flatten()
        .any(|value| as_id(value) == Some(id));
    if !exact_entry && !matching_id {
        return Ok(None);
    }
    if matches!(
        tweet.get("__typename").and_then(Value::as_str),
        Some("TweetUnavailable" | "TweetTombstone")
    ) || (exact_entry && tweet.is_null())
    {
        return Err(Error::Unavailable);
    }
    if !matching_id
        || identities
            .into_iter()
            .flatten()
            .any(|value| as_id(value) != Some(id))
    {
        return Err(Error::Response("post identity"));
    }
    Ok(Some((tweet, result)))
}

fn as_id(value: &Value) -> Option<u64> {
    value
        .as_str()
        .and_then(numeric_id)
        .or_else(|| value.as_u64().filter(|id| *id > 0))
}

fn attachment(item: &Value, id: u64) -> Result<Media, Error> {
    if item
        .get("source_status_id_str")
        .and_then(as_id)
        .is_some_and(|source| source != id)
    {
        return Err(Error::UnsupportedMedia);
    }
    let (kind, url, extension) = match item.get("type").and_then(Value::as_str) {
        Some("photo") => {
            let (url, extension) = original_photo(item)?;
            (MediaKind::Photo, url, extension)
        }
        Some("video" | "animated_gif") => (MediaKind::Video, direct_video(item)?, "mp4"),
        _ => return Err(Error::Response("post attachment type")),
    };
    let sensitive = warning(item)
        || ["possibly_sensitive", "sensitive"]
            .iter()
            .any(|field| item.get(*field).and_then(Value::as_bool) == Some(true));
    let metadata = serde_json::json!({
        "id": item.get("id_str"),
        "type": item.get("type"),
        "width": item.pointer("/original_info/width"),
        "height": item.pointer("/original_info/height"),
        "alt_text": item.get("ext_alt_text"),
        "sensitive": sensitive,
    });
    Ok(Media {
        kind,
        url,
        extension,
        sensitive,
        metadata,
    })
}

fn warning(value: &Value) -> bool {
    value.get("sensitive_media_warning").is_some_and(|warning| {
        ["adult_content", "graphic_violence", "other"]
            .iter()
            .any(|field| warning.get(*field).and_then(Value::as_bool) == Some(true))
    })
}

fn original_photo(item: &Value) -> Result<(Url, &'static str), Error> {
    let mut url = item
        .get("media_url_https")
        .and_then(Value::as_str)
        .and_then(|raw| media_url(raw, "pbs.twimg.com", "/media/"))
        .ok_or(Error::Response("original photo URL"))?;
    let (path, suffix) = url
        .path()
        .rsplit_once('.')
        .ok_or(Error::Response("photo format"))?;
    let extension = match suffix {
        "jpg" => "jpg",
        "jpeg" => "jpeg",
        "png" => "png",
        "webp" => "webp",
        "gif" => "gif",
        _ => return Err(Error::Response("photo format")),
    };
    let path = path.to_owned();
    url.set_path(&path);
    url.set_query(None);
    url.query_pairs_mut()
        .append_pair("format", extension)
        .append_pair("name", "orig");
    Ok((url, extension))
}

fn direct_video(item: &Value) -> Result<Url, Error> {
    let variants = item
        .pointer("/video_info/variants")
        .and_then(Value::as_array)
        .ok_or(Error::Response("video variants"))?;
    let mut mp4 = Vec::new();
    let mut has_stream = false;
    for variant in variants {
        let Some(url) = variant
            .get("url")
            .and_then(Value::as_str)
            .and_then(|raw| media_url(raw, "video.twimg.com", "/"))
        else {
            continue;
        };
        match variant.get("content_type").and_then(Value::as_str) {
            Some("video/mp4") => {
                let bitrate = variant.get("bitrate").and_then(Value::as_u64).unwrap_or(0);
                mp4.push((bitrate, url));
            }
            Some(
                "application/x-mpegURL" | "application/x-mpegurl" | "application/vnd.apple.mpegurl",
            ) => {
                has_stream = true;
            }
            _ => {}
        }
    }
    mp4.into_iter()
        .max_by_key(|(bitrate, _)| *bitrate)
        .map(|(_, url)| url)
        .ok_or(if has_stream {
            Error::UnsupportedMedia
        } else {
            Error::Response("direct video URL")
        })
}

fn media_url(raw: &str, host: &str, prefix: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    (url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with(prefix)
        && url.fragment().is_none())
    .then_some(url)
}
