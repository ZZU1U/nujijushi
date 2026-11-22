//! FANBOX creator and media identities shared by imports and watches.

use std::path::Path;

use serde_json::Value;
use url::Url;

pub(super) const DISABLE_EMBEDS: &str = "extractor.fanbox.embeds=false";

pub(super) fn creator_id(id: &str) -> Option<&str> {
    (!id.is_empty()
        && id.len() <= 63
        && !id.eq_ignore_ascii_case("www")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte)))
    .then_some(id)
}

pub(super) fn creator_from_feed(url: &Url) -> Option<&str> {
    let path = url.path().trim_matches('/');
    let creator = match url.host_str()? {
        "fanbox.cc" | "www.fanbox.cc" => path
            .strip_suffix("/posts")
            .unwrap_or(path)
            .strip_prefix('@')?,
        host if matches!(path, "" | "posts") => host.strip_suffix(".fanbox.cc")?,
        _ => return None,
    };
    creator_id(creator)
}

pub(super) fn creator_url(creator: &str) -> String {
    format!("https://{creator}.fanbox.cc")
}

pub(super) fn restricted(meta: &Value) -> bool {
    meta.get("isRestricted").and_then(Value::as_bool) == Some(true)
}

pub(super) fn supported_media(meta: &Value, url: &Value) -> bool {
    let supported = |ext: &str| crate::blob::is_image_ext(ext) || crate::blob::is_video_ext(ext);
    if let Some(extension) = meta.get("extension").and_then(Value::as_str) {
        return supported(extension);
    }
    url.as_str()
        .and_then(|url| Url::parse(url).ok())
        .is_some_and(|url| {
            Path::new(url.path())
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(supported)
        })
}
