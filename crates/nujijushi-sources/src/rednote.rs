//! Read one RedNote/Xiaohongshu note through the first-party web page.

mod page;

use reqwest::header::{ACCEPT, COOKIE, HeaderValue, LOCATION, USER_AGENT};
use reqwest::{Client, StatusCode};
use url::Url;

pub use page::{Author, Media, MediaKind, Note};

const MAX_REDIRECTS: usize = 5;
const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;
const WEB_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Use an individual RedNote note or app share link")]
    InvalidLink,
    #[error("RedNote redirected outside its supported note and share URLs")]
    InvalidRedirect,
    #[error("RedNote exceeded the share-link redirect limit")]
    RedirectLimit,
    #[error("RedNote requires an authorized session to view this note")]
    Authentication,
    #[error("RedNote could not open the share link. Copy a fresh share link from the app")]
    ShareLink,
    #[error("The RedNote note is unavailable")]
    Unavailable,
    #[error("RedNote rate limit exceeded")]
    RateLimited,
    #[error("RedNote returned HTTP {0}")]
    Http(StatusCode),
    #[error("RedNote request failed")]
    Transport,
    #[error("RedNote returned an unsupported or incomplete note page")]
    Page,
    #[error("RedNote returned a different note than requested")]
    Identity,
    #[error("RedNote did not provide every gallery image")]
    MissingImage,
    #[error("RedNote did not provide a direct MP4 for this video note")]
    UnsupportedVideo,
    #[error("RedNote returned an unsupported note type")]
    UnsupportedNote,
}

pub struct RedNote {
    http: Client,
    #[cfg(feature = "test-support")]
    test_origin: Option<Url>,
}

impl RedNote {
    /// Supply the shared HTTP client with automatic redirects disabled.
    pub fn new(http: Client) -> Self {
        Self {
            http,
            #[cfg(feature = "test-support")]
            test_origin: None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn with_test_origin(mut self, origin: &Url) -> Self {
        self.test_origin = Some(origin.clone());
        self
    }

    /// The session callback is called only for validated first-party note URLs.
    pub async fn note(
        &self,
        mut url: Url,
        session: impl Fn(&Url) -> Option<HeaderValue>,
    ) -> Result<Note, Error> {
        if !matches!(url.scheme(), "http" | "https") || !clean_authority(&url) {
            return Err(Error::InvalidLink);
        }
        url.set_scheme("https").map_err(|_| Error::InvalidLink)?;
        url.set_fragment(None);
        if note_id(&url).is_none() && !is_share_link(&url) {
            return Err(Error::InvalidLink);
        }
        let mut expected = note_id(&url).map(str::to_ascii_lowercase);
        for redirects in 0..=MAX_REDIRECTS {
            let mut request = self
                .http
                .get(self.request_url(&url))
                .header(USER_AGENT, WEB_USER_AGENT)
                .header(ACCEPT, "text/html,application/xhtml+xml");
            if note_id(&url).is_some()
                && let Some(mut header) = session(&url)
            {
                header.set_sensitive(true);
                request = request.header(COOKIE, header);
            }
            let response = request.send().await.map_err(|_| Error::Transport)?;
            if response.status().is_redirection() {
                if redirects == MAX_REDIRECTS {
                    return Err(Error::RedirectLimit);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(Error::InvalidRedirect)?;
                let mut next = url.join(location).map_err(|_| Error::InvalidRedirect)?;
                if next.scheme() != "https" || !clean_authority(&next) || !is_site_url(&next) {
                    return Err(Error::InvalidRedirect);
                }
                next.set_fragment(None);
                if is_note_host(next.host_str().unwrap_or_default())
                    && let Some(error) = page_error(&next)
                {
                    return Err(error);
                }
                if let Some(id) = note_id(&next) {
                    if expected
                        .as_ref()
                        .is_some_and(|expected| !id.eq_ignore_ascii_case(expected))
                    {
                        return Err(Error::Identity);
                    }
                    expected = Some(id.to_ascii_lowercase());
                    inherit_share_params(&url, &mut next);
                } else if expected.is_some() || !is_share_link(&next) {
                    return Err(Error::InvalidRedirect);
                }
                url = next;
                continue;
            }
            match response.status() {
                StatusCode::OK => {}
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    return Err(Error::Authentication);
                }
                StatusCode::NOT_FOUND | StatusCode::GONE => return Err(Error::Unavailable),
                StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
                status => return Err(Error::Http(status)),
            }
            let id = expected.ok_or(Error::ShareLink)?;
            let html = read_page(response).await?;
            let canonical = canonical_url(&url, &id);
            let mut note = page::parse(&html, &id, canonical)?;
            note.caption_url = caption_url(url, &id);
            return Ok(note);
        }
        Err(Error::RedirectLimit)
    }

    fn request_url(&self, url: &Url) -> Url {
        #[cfg(feature = "test-support")]
        if let Some(origin) = &self.test_origin {
            let mut local = origin.clone();
            local.set_path(url.path());
            local.set_query(url.query());
            return local;
        }
        url.clone()
    }
}

pub fn is_note_host(host: &str) -> bool {
    matches!(
        host,
        "rednote.com" | "www.rednote.com" | "xiaohongshu.com" | "www.xiaohongshu.com"
    )
}

pub fn is_site_url(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| is_note_host(host) || is_share_host(host))
}

fn is_share_host(host: &str) -> bool {
    matches!(host, "xhslink.com" | "xhslink.cn")
}

fn clean_authority(url: &Url) -> bool {
    url.username().is_empty() && url.password().is_none() && url.port().is_none()
}

fn is_share_link(url: &Url) -> bool {
    if !url.host_str().is_some_and(is_share_host) {
        return false;
    }
    let mut path = url.path().trim_matches('/').split('/');
    let code = match (path.next(), path.next(), path.next()) {
        (Some(code), None, None) | (Some("a" | "o"), Some(code), None) => code,
        _ => return false,
    };
    !code.is_empty()
        && code.len() <= 128
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn note_id(url: &Url) -> Option<&str> {
    if !url.host_str().is_some_and(is_note_host) {
        return None;
    }
    let path: Vec<_> = note_path(url).split('/').collect();
    let id = match path.as_slice() {
        ["explore", id] | ["discovery", "item", id] => id,
        ["user", "profile", author, id] if valid_id(author) => id,
        _ => return None,
    };
    valid_id(id).then_some(*id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 24 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn note_path(url: &Url) -> &str {
    let path = url.path().trim_matches('/');
    path.strip_prefix("en/").unwrap_or(path)
}

/// Keep only RedNote's share parameters on the original route used by the link.
pub fn caption_url(mut url: Url, id: &str) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") || !clean_authority(&url) {
        return None;
    }
    let params: Vec<_> = url
        .query_pairs()
        .filter(|(key, value)| {
            matches!(key.as_ref(), "xsec_token" | "xsec_source") && !value.is_empty()
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let has_note_link = note_id(&url).is_some_and(|found| found.eq_ignore_ascii_case(id))
        && params.iter().any(|(key, _)| key == "xsec_token");
    if !(has_note_link || is_share_link(&url)) {
        return None;
    }
    url.set_scheme("https").ok()?;
    url.set_fragment(None);
    url.set_query(None);
    if !params.is_empty() {
        url.query_pairs_mut().extend_pairs(params);
    }
    Some(url.into())
}

/// A presentation URL with no credentials, share query, or opaque short code.
pub fn display_url(url: &Url) -> Option<String> {
    if !is_site_url(url) {
        return None;
    }
    Some(match note_id(url) {
        Some(id) => canonical_url(url, &id.to_ascii_lowercase()),
        None if url.host_str().is_some_and(is_share_host) => "RedNote share link".into(),
        None => "RedNote link".into(),
    })
}

fn canonical_url(url: &Url, id: &str) -> String {
    let route = if url.path().starts_with("/en/") {
        "en/discovery/item"
    } else {
        "explore"
    };
    format!(
        "https://{}/{route}/{id}",
        url.host_str().unwrap_or("www.rednote.com")
    )
}

fn inherit_share_params(from: &Url, to: &mut Url) {
    let inherited: Vec<_> = ["xsec_token", "xsec_source"]
        .into_iter()
        .filter(|name| {
            !to.query_pairs()
                .any(|(key, value)| key == *name && !value.is_empty())
        })
        .filter_map(|name| {
            let (_, value) = from
                .query_pairs()
                .find(|(key, value)| key == name && !value.is_empty())?;
            Some((name, value.into_owned()))
        })
        .collect();
    if !inherited.is_empty() {
        let keep: Vec<_> = to
            .query_pairs()
            .filter(|(key, _)| !inherited.iter().any(|(name, _)| key == *name))
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        to.query_pairs_mut()
            .clear()
            .extend_pairs(keep)
            .extend_pairs(inherited);
    }
}

fn page_error(url: &Url) -> Option<Error> {
    let code = url.query_pairs().find_map(|(name, value)| {
        matches!(name.as_ref(), "error_code" | "errorCode" | "code")
            .then(|| value.parse::<i64>().ok())
            .flatten()
    });
    if let Some(error) = code.and_then(map_error_code) {
        return Some(error);
    }
    match note_path(url) {
        "login" | "website-login" | "captcha" => Some(Error::Authentication),
        "404" => Some(Error::Unavailable),
        _ => None,
    }
}

fn map_error_code(code: i64) -> Option<Error> {
    match code {
        300031 => Some(Error::ShareLink),
        401 | 403 => Some(Error::Authentication),
        404 | 410 => Some(Error::Unavailable),
        429 => Some(Error::RateLimited),
        _ => None,
    }
}

async fn read_page(mut response: reqwest::Response) -> Result<String, Error> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
        if chunk.len() > MAX_HTML_BYTES - bytes.len() {
            return Err(Error::Page);
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| Error::Page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_urls_redact_opaque_codes_and_restore_missing_parameters() {
        let short = Url::parse("https://xhslink.cn/a/opaque-code?xsec_token=private").unwrap();
        assert_eq!(display_url(&short).as_deref(), Some("RedNote share link"));
        let mut note = Url::parse("https://www.rednote.com/discovery/item/674051740000000007027a15?xsec_token=&xsec_source=new").unwrap();
        inherit_share_params(&short, &mut note);
        assert_eq!(note.query(), Some("xsec_source=new&xsec_token=private"));
        assert_eq!(
            display_url(&note).as_deref(),
            Some("https://www.rednote.com/explore/674051740000000007027a15")
        );
        let localized = Url::parse(
            "https://www.rednote.com/en/discovery/item/674051740000000007027a15?xsec_token=private",
        )
        .unwrap();
        assert_eq!(note_id(&localized), Some("674051740000000007027a15"));
        assert_eq!(
            display_url(&localized).as_deref(),
            Some("https://www.rednote.com/en/discovery/item/674051740000000007027a15")
        );
    }
}
