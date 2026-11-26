//! Read-only access to Pixiv's web artwork and creator endpoints.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;

use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, COOKIE, HeaderValue, REFERER};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Deserializer, de::DeserializeOwned};
use serde_json::Value;
use url::Url;

const ORIGIN: &str = "https://www.pixiv.net";
const SUMMARY_BATCH: usize = 48;
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Pixiv requires a valid session with access to this content")]
    Authentication,
    #[error("Pixiv rate limit exceeded")]
    RateLimited,
    #[error("Pixiv artwork or creator is unavailable")]
    Unavailable,
    #[error("Pixiv request failed (HTTP {0})")]
    Http(StatusCode),
    #[error("Pixiv request timed out")]
    Timeout,
    #[error("Pixiv request failed")]
    Transport,
    #[error("Pixiv returned invalid or incomplete {0}")]
    InvalidResponse(&'static str),
    #[error("Pixiv returned incomplete creator summaries")]
    MissingSummaries,
    #[error("Pixiv did not provide all original artwork pages")]
    MissingOriginal,
    #[error("Pixiv Ugoira requires an animation downloader")]
    Ugoira,
}

pub struct Pixiv {
    http: Client,
    cookie: Option<HeaderValue>,
    origin: String,
}

pub struct Artist {
    pub id: u64,
    pub name: String,
    pub account: Option<String>,
}

pub struct Artwork {
    pub id: u64,
    pub artist: Artist,
    pub title: String,
    pub description: String,
    pub published_at: DateTime<Utc>,
    pub restriction: u8,
    pub tags: Vec<String>,
    pub pages: Vec<Original>,
    pub metadata: Value,
}

pub struct Original {
    pub url: Url,
    pub width: u32,
    pub height: u32,
}

pub struct FeedItem {
    pub id: u64,
    pub published_at: DateTime<Utc>,
}

pub fn artwork_url(id: u64) -> String {
    format!("{ORIGIN}/artworks/{id}")
}

pub fn artwork_id(input: &str) -> Option<u64> {
    let url = Url::parse(input).ok()?;
    if !matches!(url.host_str(), Some("pixiv.net" | "www.pixiv.net"))
        || !matches!(url.scheme(), "http" | "https")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let segments: Vec<_> = url.path().trim_matches('/').split('/').collect();
    match segments.as_slice() {
        ["artworks" | "i", id] => parse_id(id),
        [locale, "artworks", id]
            if (locale.len() == 2 && locale.bytes().all(|b| b.is_ascii_lowercase()))
                || matches!(*locale, "zh-cn" | "zh-tw") =>
        {
            parse_id(id)
        }
        ["member_illust.php"] => {
            let mut ids = url.query_pairs().filter(|(key, _)| key == "illust_id");
            let (_, id) = ids.next()?;
            (ids.next().is_none()).then(|| parse_id(&id)).flatten()
        }
        _ => None,
    }
}

impl Pixiv {
    /// Supply a client with redirects disabled. The session header is sensitive
    /// and belongs only on the fixed Pixiv metadata origin.
    pub fn new(http: Client, mut cookie: Option<HeaderValue>) -> Self {
        if let Some(value) = &mut cookie {
            value.set_sensitive(true);
        }
        Self {
            http,
            cookie,
            origin: ORIGIN.into(),
        }
    }

    #[cfg(feature = "test-support")]
    pub fn with_test_origin(mut self, origin: &Url) -> Self {
        self.origin = origin.origin().ascii_serialization();
        self
    }

    pub async fn artwork(&self, id: u64) -> Result<Artwork, Error> {
        let path = format!("/ajax/illust/{id}");
        let metadata = self.get(&path, &[], "artwork detail").await?;
        let detail: Detail = decode(metadata.clone(), "artwork detail")?;
        if detail.id != id {
            return Err(Error::InvalidResponse("artwork identity"));
        }
        match detail.kind {
            0 | 1 => {}
            2 => return Err(Error::Ugoira),
            _ => return Err(Error::InvalidResponse("artwork type")),
        }
        if detail.page_count == 0 {
            return Err(Error::MissingOriginal);
        }
        let pages = if detail.page_count == 1 {
            vec![Page {
                urls: detail.urls,
                width: detail.width,
                height: detail.height,
            }]
        } else {
            decode(
                self.get(&format!("{path}/pages"), &[], "artwork pages")
                    .await?,
                "artwork pages",
            )?
        };
        if pages.len() != detail.page_count {
            return Err(Error::MissingOriginal);
        }
        let mut seen_urls = BTreeSet::new();
        let mut originals = Vec::with_capacity(pages.len());
        for Page {
            urls,
            width,
            height,
        } in pages
        {
            let url = urls
                .original
                .and_then(|raw| Url::parse(&raw).ok())
                .filter(is_original)
                .ok_or(Error::MissingOriginal)?;
            if !seen_urls.insert(url.clone()) {
                return Err(Error::MissingOriginal);
            }
            originals.push(Original { url, width, height });
        }
        Ok(Artwork {
            id,
            artist: Artist {
                id: detail.user_id,
                name: detail.user_name,
                account: detail.user_account,
            },
            title: detail.title,
            description: detail.description,
            published_at: detail.create_date,
            restriction: detail.x_restrict,
            tags: detail.tags.tags.into_iter().map(|item| item.tag).collect(),
            pages: originals,
            metadata,
        })
    }

    pub async fn creator_index(&self, id: u64) -> Result<BTreeSet<u64>, Error> {
        let index: Profile = decode(
            self.get(
                &format!("/ajax/user/{id}/profile/all"),
                &[],
                "creator listing",
            )
            .await?,
            "creator listing",
        )?;
        index
            .illusts
            .ids()
            .chain(index.manga.ids())
            .map(|key| parse_id(&key).ok_or(Error::InvalidResponse("creator artwork ID")))
            .collect()
    }

    pub async fn creator_summaries(&self, id: u64, ids: &[u64]) -> Result<Vec<FeedItem>, Error> {
        let path = format!("/ajax/user/{id}/profile/illusts");
        let mut result = Vec::with_capacity(ids.len());
        for batch in ids.chunks(SUMMARY_BATCH) {
            let mut params = vec![
                ("work_category", "illustManga".into()),
                ("is_first_page", "0".into()),
            ];
            params.extend(batch.iter().map(|id| ("ids[]", id.to_string())));
            let summaries: Summaries = decode(
                self.get(&path, &params, "creator summaries").await?,
                "creator summaries",
            )?;
            for (key, summary) in summaries.works {
                if key != summary.id.to_string()
                    || !batch.contains(&summary.id)
                    || summary.user_id != id
                {
                    return Err(Error::InvalidResponse("creator artwork identity"));
                }
                result.push(FeedItem {
                    id: summary.id,
                    published_at: summary.create_date,
                });
            }
        }
        if result.len() != ids.len() {
            Err(Error::MissingSummaries)
        } else {
            Ok(result)
        }
    }

    async fn get(
        &self,
        path: &str,
        query: &[(&str, String)],
        operation: &'static str,
    ) -> Result<Value, Error> {
        let mut request = self
            .http
            .get(format!("{}{path}", self.origin))
            .query(query)
            .header(ACCEPT, "application/json")
            .header(REFERER, ORIGIN);
        if let Some(cookie) = &self.cookie {
            request = request.header(COOKIE, cookie.clone());
        }
        let mut response = request.send().await.map_err(transport)?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Error::Authentication),
            StatusCode::NOT_FOUND | StatusCode::GONE => return Err(Error::Unavailable),
            StatusCode::TOO_MANY_REQUESTS => return Err(Error::RateLimited),
            status if !status.is_success() => return Err(Error::Http(status)),
            _ => {}
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            if chunk.len() > MAX_JSON_BYTES - bytes.len() {
                return Err(Error::InvalidResponse(operation));
            }
            bytes.extend_from_slice(&chunk);
        }
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse(operation))?;
        if !envelope.error {
            return envelope.body.ok_or(Error::InvalidResponse(operation));
        }
        let message = envelope.message.unwrap_or_default().to_ascii_lowercase();
        if message.contains("rate limit") || message.contains("too many requests") {
            return Err(Error::RateLimited);
        }
        if message.contains("login")
            || message.contains("log in")
            || message.contains("sign in")
            || (self.cookie.is_some() && message == "an unknown error occurred")
        {
            return Err(Error::Authentication);
        }
        Err(Error::Unavailable)
    }
}

fn transport(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Transport
    }
}

fn is_original(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("i.pximg.net")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with("/img-original/")
}

fn parse_id(value: &str) -> Option<u64> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<NonZeroU64>().ok().map(NonZeroU64::get)
}

fn deserialize_id<'de, D: Deserializer<'de>>(de: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(NonZeroU64),
        Text(String),
    }
    match Id::deserialize(de)? {
        Id::Number(id) => Ok(id.get()),
        Id::Text(text) => {
            parse_id(&text).ok_or_else(|| serde::de::Error::custom("invalid Pixiv ID"))
        }
    }
}

fn decode<T: DeserializeOwned>(value: Value, operation: &'static str) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::InvalidResponse(operation))
}

#[derive(Deserialize)]
struct Envelope {
    error: bool,
    body: Option<Value>,
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Detail {
    #[serde(deserialize_with = "deserialize_id")]
    id: u64,
    #[serde(deserialize_with = "deserialize_id")]
    user_id: u64,
    user_name: String,
    user_account: Option<String>,
    title: String,
    description: String,
    create_date: DateTime<Utc>,
    x_restrict: u8,
    #[serde(rename = "illustType")]
    kind: u8,
    page_count: usize,
    width: u32,
    height: u32,
    #[serde(default)]
    urls: ImageUrls,
    tags: Tags,
}

#[derive(Default, Deserialize)]
struct ImageUrls {
    original: Option<String>,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    urls: ImageUrls,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Tags {
    tags: Vec<Tag>,
}

#[derive(Deserialize)]
struct Tag {
    tag: String,
}

#[derive(Deserialize)]
struct Profile {
    illusts: ArtworkIds,
    manga: ArtworkIds,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ArtworkIds {
    Map(BTreeMap<String, ()>),
    Empty([(); 0]),
}

impl ArtworkIds {
    fn ids(self) -> impl Iterator<Item = String> {
        match self {
            Self::Map(ids) => ids.into_keys(),
            Self::Empty([]) => BTreeMap::new().into_keys(),
        }
    }
}

#[derive(Deserialize)]
struct Summaries {
    works: BTreeMap<String, Summary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    #[serde(deserialize_with = "deserialize_id")]
    id: u64,
    #[serde(deserialize_with = "deserialize_id")]
    user_id: u64,
    create_date: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_individual_artwork_links_only() {
        for input in [
            "https://www.pixiv.net/artworks/42",
            "https://www.pixiv.net/en/artworks/42?p=3",
            "https://pixiv.net/i/42",
            "https://www.pixiv.net/member_illust.php?mode=medium&illust_id=42",
        ] {
            assert_eq!(artwork_id(input), Some(42));
        }
        for input in [
            "https://www.pixiv.net/users/42",
            "https://www.pixiv.net/novel/show.php?id=42",
            "https://www.pixiv.net/artworks/0",
            "https://pixiv.net.evil.test/artworks/42",
            "https://someone@pixiv.net/artworks/42",
            "https://www.pixiv.net/member_illust.php?illust_id=42&illust_id=43",
        ] {
            assert_eq!(artwork_id(input), None);
        }
    }
}
