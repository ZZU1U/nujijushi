//! Authenticated, read-only requests for one X status at a time.

mod detail;
mod operation;
mod signing;

use reqwest::header::{AUTHORIZATION, COOKIE, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use reqwest::{Client, RequestBuilder, StatusCode};
use url::Url;

pub use detail::{Author, Media, MediaKind, Status};

const WEB: &str = "https://x.com";
const ASSETS: &str = "abs.twimg.com";
const USER_AGENT_WEB: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("X rejected the signed-in session or requires an account check")]
    Authentication,
    #[error("The signed-in X account cannot view this post, or the post was removed")]
    Unavailable,
    #[error("X rate limit exceeded")]
    RateLimited,
    #[error("X returned HTTP {0} while fetching {1}")]
    Http(StatusCode, &'static str),
    #[error("X request failed while fetching {0}")]
    Transport(&'static str),
    #[error("X returned invalid or incomplete {0}")]
    Response(&'static str),
    #[error("X web signing data could not be read")]
    Signing,
    #[error("The X post has no attached media")]
    NoMedia,
    #[error("This X post requires gallery-dl for its media")]
    UnsupportedMedia,
}

pub struct StatusLink<'a> {
    pub id: u64,
    pub handle: Option<&'a str>,
}

pub fn status_url(id: u64) -> String {
    format!("{WEB}/i/web/status/{id}")
}

pub fn status_link(url: &Url) -> Option<StatusLink<'_>> {
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(
            url.host_str()?,
            "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com" | "mobile.twitter.com"
        )
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let segments: Vec<_> = url.path().trim_matches('/').split('/').collect();
    let (handle, id, suffix) = match segments.as_slice() {
        ["i", "web", "status", id, suffix @ ..] => (None, *id, suffix),
        ["i", "status", id, suffix @ ..] => (None, *id, suffix),
        [handle, "status", id, suffix @ ..] if valid_handle(handle) => (Some(*handle), *id, suffix),
        _ => return None,
    };
    let valid_suffix = match suffix {
        [] => true,
        ["photo" | "video", index] => numeric_id(index).is_some(),
        _ => false,
    };
    if !valid_suffix {
        return None;
    }
    Some(StatusLink {
        id: numeric_id(id)?,
        handle,
    })
}

fn numeric_id(value: &str) -> Option<u64> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|id| *id > 0)
}

fn valid_handle(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 50
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub struct Twitter {
    http: Client,
    cookie: HeaderValue,
    csrf: HeaderValue,
    #[cfg(feature = "test-support")]
    test_origin: Option<Url>,
}

impl Twitter {
    /// Supply the shared HTTP client with redirects disabled.
    pub fn new(http: Client, mut cookie: HeaderValue, mut csrf: HeaderValue) -> Self {
        cookie.set_sensitive(true);
        csrf.set_sensitive(true);
        Self {
            http,
            cookie,
            csrf,
            #[cfg(feature = "test-support")]
            test_origin: None,
        }
    }

    #[cfg(feature = "test-support")]
    pub fn with_test_origin(mut self, origin: &Url) -> Self {
        self.test_origin = Some(origin.clone());
        self
    }

    pub async fn status(&self, id: u64) -> Result<Status, Error> {
        let signer = signing::load(self).await?;
        let path = operation::DETAIL_PATH;
        let signature = signer.sign(path, chrono::Utc::now().timestamp())?;
        // This bearer identifies X's public web client. User credentials are the scoped cookies.
        let request = self
            .web_get(path)
            .query(&operation::detail_query(id))
            .header(AUTHORIZATION, operation::WEB_BEARER)
            .header("x-csrf-token", self.csrf.clone())
            .header("x-client-transaction-id", signature)
            .header("x-twitter-active-user", "yes")
            .header("x-twitter-auth-type", "OAuth2Session")
            .header("x-twitter-client-language", "en")
            .header(ORIGIN, WEB)
            .header(REFERER, status_url(id));
        let body = read(request, "post detail").await?;
        let value = serde_json::from_str(&body).map_err(|_| Error::Response("post detail"))?;
        detail::parse(value, id)
    }

    fn web_get(&self, path: &str) -> RequestBuilder {
        let url = Url::parse(&format!("{WEB}{path}")).expect("fixed X origin and paths");
        self.http
            .get(self.request_url(url))
            .header(USER_AGENT, USER_AGENT_WEB)
            .header(COOKIE, self.cookie.clone())
    }

    async fn bootstrap(&self, path: &str) -> Result<String, Error> {
        read(self.web_get(path), "web session").await
    }

    async fn script(&self, url: Url) -> Result<String, Error> {
        if !signing::is_asset(&url) {
            return Err(Error::Signing);
        }
        let request = self
            .http
            .get(self.request_url(url))
            .header(USER_AGENT, USER_AGENT_WEB);
        read(request, "web signing asset").await
    }

    fn request_url(&self, url: Url) -> Url {
        #[cfg(feature = "test-support")]
        if let Some(origin) = &self.test_origin {
            let mut local = origin.clone();
            local.set_path(url.path());
            local.set_query(url.query());
            return local;
        }
        url
    }
}

async fn read(request: RequestBuilder, context: &'static str) -> Result<String, Error> {
    let mut response = request
        .send()
        .await
        .map_err(|_| Error::Transport(context))?;
    match response.status() {
        StatusCode::UNAUTHORIZED => return Err(Error::Authentication),
        StatusCode::FORBIDDEN if context == "post detail" => return Err(Error::Unavailable),
        StatusCode::FORBIDDEN if context == "web session" => return Err(Error::Authentication),
        StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
        status if status.is_redirection() && context == "web session" => {
            let login = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|location| {
                    location.contains("/login") || location.contains("/account/access")
                });
            if login {
                return Err(Error::Authentication);
            }
            return Err(Error::Http(status, context));
        }
        status if !status.is_success() => return Err(Error::Http(status, context)),
        _ => {}
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::Transport(context))?
    {
        if chunk.len() > MAX_BODY - body.len() {
            return Err(Error::Response(context));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| Error::Response(context))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_routes_individual_x_status_links() {
        for path in [
            "https://x.com/i/web/status/42",
            "https://x.com/i/status/42",
            "https://twitter.com/Artist/status/42/photo/2?t=123",
            "https://mobile.twitter.com/Artist/status/42/video/1",
        ] {
            let url = Url::parse(path).unwrap();
            assert_eq!(status_link(&url).unwrap().id, 42);
        }
        for path in [
            "https://x.com/Artist/media",
            "https://x.com/Artist/status/42/likes",
            "https://x.com/Artist/status/42/photo/0",
            "https://x.com.evil.test/Artist/status/42",
            "https://name@x.com/Artist/status/42",
        ] {
            assert!(status_link(&Url::parse(path).unwrap()).is_none());
        }
    }
}
