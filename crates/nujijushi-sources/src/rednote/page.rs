use chrono::{DateTime, Utc};
use serde_json::Value;
use url::Url;

use super::{Error, map_error_code};

pub struct Note {
    pub id: String,
    pub url: String,
    pub caption_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<Author>,
    pub tags: Vec<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub sensitive: bool,
    pub media: Vec<Media>,
}

pub struct Author {
    pub id: String,
    pub name: Option<String>,
}

pub struct Media {
    pub url: Url,
    pub kind: MediaKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

pub(super) fn parse(html: &str, id: &str, url: String) -> Result<Note, Error> {
    let state = embedded_state(html).ok_or(Error::Page)?;
    if let Some(error) = response_error(&state) {
        return Err(error);
    }
    let note = state.get("note").ok_or(Error::Page)?;
    if let Some(error) = response_error(note) {
        return Err(error);
    }
    let map = note
        .get("noteDetailMap")
        .and_then(Value::as_object)
        .ok_or(Error::Page)?;
    let entry = map
        .iter()
        .find_map(|(key, entry)| key.eq_ignore_ascii_case(id).then_some(entry))
        .ok_or(if map.is_empty() {
            Error::Unavailable
        } else {
            Error::Identity
        })?;
    if let Some(error) = response_error(entry) {
        return Err(error);
    }
    let note = entry
        .get("note")
        .filter(|note| note.is_object())
        .ok_or(Error::Page)?;
    for key in ["noteId", "id"] {
        if note.get(key).is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(id))
        }) {
            return Err(Error::Identity);
        }
    }
    let media = match field(note, "type") {
        Some("normal") => gallery(note)?,
        Some("video") => vec![video(note)?],
        _ => return Err(Error::UnsupportedNote),
    };
    let author = note.get("user").and_then(|user| {
        Some(Author {
            id: field(user, "userId")?.to_owned(),
            name: field(user, "nickname")
                .or_else(|| field(user, "nickName"))
                .map(str::to_owned),
        })
    });
    let tags = items(note.get("tagList"))
        .filter_map(|tag| field(tag, "name"))
        .map(str::to_owned)
        .collect();
    Ok(Note {
        caption_url: None,
        id: id.to_owned(),
        url,
        title: field(note, "title").map(str::to_owned),
        description: field(note, "desc").map(str::to_owned),
        author,
        tags,
        published_at: note
            .get("time")
            .or_else(|| note.get("createTime"))
            .and_then(timestamp),
        sensitive: note.get("isSensitive").and_then(Value::as_bool) == Some(true),
        media,
    })
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn items(value: Option<&Value>) -> impl Iterator<Item = &Value> {
    value.and_then(Value::as_array).into_iter().flatten()
}

fn response_error(value: &Value) -> Option<Error> {
    ["errorCode", "error_code", "code"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(integer).and_then(map_error_code))
}

fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match integer(value) {
        Some(time) if time >= 100_000_000_000 => DateTime::from_timestamp_millis(time),
        Some(time) if time > 0 => DateTime::from_timestamp(time, 0),
        _ => DateTime::parse_from_rfc3339(value.as_str()?)
            .ok()
            .map(|time| time.to_utc()),
    }
}

fn gallery(note: &Value) -> Result<Vec<Media>, Error> {
    let photos = note
        .get("imageList")
        .and_then(Value::as_array)
        .filter(|photos| !photos.is_empty())
        .ok_or(Error::MissingImage)?;
    photos.iter().map(image).collect()
}

fn image(photo: &Value) -> Result<Media, Error> {
    if photo.get("livePhoto").and_then(Value::as_bool) == Some(true) {
        return Err(Error::UnsupportedNote);
    }
    let advertised = items(photo.get("infoList")).filter_map(|variant| {
        let priority = match field(variant, "imageScene") {
            Some("WB_DFT") => 3,
            Some("WB_PRV") => 1,
            _ => 2,
        };
        let url = cdn_url(field(variant, "url")?)?;
        Some((priority, area(variant), url))
    });
    let defaults = [("urlDefault", 3), ("url", 2), ("urlPre", 1)]
        .into_iter()
        .filter_map(|(key, priority)| Some((priority, 0, cdn_url(field(photo, key)?)?)));
    let url = defaults
        .chain(advertised)
        .max_by_key(|(priority, pixels, _)| (*priority, *pixels))
        .map(|(_, _, url)| url)
        .ok_or(Error::MissingImage)?;
    Ok(Media {
        url,
        kind: MediaKind::Image,
    })
}

fn video(note: &Value) -> Result<Media, Error> {
    let streams = note
        .pointer("/video/media/stream")
        .and_then(Value::as_object)
        .ok_or(Error::UnsupportedVideo)?;
    let (_, url) = streams
        .iter()
        .flat_map(|(codec, streams)| items(Some(streams)).map(move |stream| (codec, stream)))
        .filter_map(|(codec, stream)| {
            let format = field(stream, "format").or_else(|| field(stream, "mimeType"));
            if format.is_some_and(|format| {
                !matches!(format.to_ascii_lowercase().as_str(), "mp4" | "video/mp4")
            }) {
                return None;
            }
            let url = field(stream, "masterUrl")
                .into_iter()
                .chain(items(stream.get("backupUrls")).filter_map(Value::as_str))
                .filter_map(cdn_url)
                .find(|url| {
                    let path = url.path().to_ascii_lowercase();
                    !path.ends_with(".m3u8")
                        && !path.ends_with(".mpd")
                        && (format.is_some()
                            || path.ends_with(".mp4")
                            || matches!(codec.as_str(), "h264" | "h265" | "av1" | "h266"))
                })?;
            let bitrate = stream.get("videoBitrate").and_then(integer).unwrap_or(0);
            Some(((area(stream), bitrate, codec == "h264"), url))
        })
        .max_by_key(|(quality, _)| *quality)
        .ok_or(Error::UnsupportedVideo)?;
    Ok(Media {
        url,
        kind: MediaKind::Video,
    })
}

fn area(value: &Value) -> u64 {
    let dimension = |key| {
        value
            .get(key)
            .and_then(integer)
            .and_then(|value| u32::try_from(value).ok())
            .map_or(0, u64::from)
    };
    dimension("width") * dimension("height")
}

fn cdn_url(raw: &str) -> Option<Url> {
    let mut url = if raw.starts_with("//") {
        Url::parse(&format!("https:{raw}"))
    } else {
        Url::parse(raw)
    }
    .ok()?;
    let host = url.host_str()?;
    if !(matches!(host, "xhscdn.com" | "rednotecdn.com" | "ci.xiaohongshu.com")
        || host.ends_with(".xhscdn.com")
        || host.ends_with(".rednotecdn.com"))
        || !matches!(url.scheme(), "http" | "https")
        || url.port().is_some()
        || url.password().is_some()
        || !url.username().is_empty()
    {
        return None;
    }
    url.set_scheme("https").ok()?;
    url.set_fragment(None);
    Some(url)
}

/// RedNote serializes the note alongside other page stores that can contain
/// JavaScript undefined and empty sets. Read the known literals as data, leaving
/// quoted text and the rest of the script alone.
fn embedded_state(html: &str) -> Option<Value> {
    html.split("window.__INITIAL_STATE__")
        .skip(1)
        .find_map(|part| {
            let object = part.trim_start().strip_prefix('=')?.trim_start();
            parse_object(object)
        })
}

fn parse_object(input: &str) -> Option<Value> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut output = Vec::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut pos = 0;
    while let Some(&byte) = bytes.get(pos) {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if let Some((literal, decoded)) = page_literal(bytes, pos) {
            output.extend_from_slice(decoded);
            pos += literal.len();
            continue;
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        output.push(byte);
                        return serde_json::from_slice(&output).ok();
                    }
                }
                _ => {}
            }
        }
        output.push(byte);
        pos += 1;
    }
    None
}

fn page_literal(bytes: &[u8], pos: usize) -> Option<(&'static [u8], &'static [u8])> {
    let (literal, decoded): (&[u8], &[u8]) = match bytes[pos] {
        b'u' => (b"undefined", b"null"),
        b'n' => (b"new Set([])", b"[]"),
        _ => return None,
    };
    let identifier = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$');
    (bytes[pos..].starts_with(literal)
        && !pos
            .checked_sub(1)
            .is_some_and(|prev| identifier(bytes[prev]))
        && !bytes
            .get(pos + literal.len())
            .is_some_and(|next| identifier(*next)))
    .then_some((literal, decoded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_embedded_state_without_modifying_strings_or_running_scripts() {
        let html = r#"<script>window.__INITIAL_STATE__ = {"text":"undefined } \"new Set([])\"","empty":undefined,"store":{"seen":new Set([])}}; throw Error();</script>"#;
        assert_eq!(
            embedded_state(html),
            Some(
                json!({"text": "undefined } \"new Set([])\"", "empty": null, "store": {"seen": []}})
            )
        );
        assert!(embedded_state("window.__INITIAL_STATE__ = fetch('/bad')").is_none());
        assert!(
            embedded_state(r#"window.__INITIAL_STATE__ = {"store": new Set(fetch('/bad'))}"#)
                .is_none()
        );
    }

    #[test]
    fn returns_errors_for_stream_only_video_and_incomplete_gallery() {
        let stream_only = json!({"video":{"media":{"stream":{"h264":[
            {"masterUrl":"https://sns-video.xhscdn.com/clip.m3u8"}
        ]}}}});
        assert!(matches!(video(&stream_only), Err(Error::UnsupportedVideo)));
        let incomplete = json!({"imageList":[
            {"urlDefault":"https://sns-img.xhscdn.com/first"},
            {"urlDefault":"https://xhscdn.com.example.test/second"}
        ]});
        assert!(matches!(gallery(&incomplete), Err(Error::MissingImage)));
    }

    #[test]
    fn accepts_rednote_cdn_over_https_and_rejects_lookalike_hosts() {
        let image = cdn_url("http://sns-web-i10.rednotecdn.com/example").unwrap();
        assert_eq!(image.scheme(), "https");
        assert!(cdn_url("https://sns-web-i10.rednotecdn.com.example.test/example").is_none());
    }
}
