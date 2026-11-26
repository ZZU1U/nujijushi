//! X web transaction IDs. The key discovery and animation format are based on
//! twscrape v0.21.1 and XClientTransaction. See the adjacent license notice.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use futures_util::{StreamExt, stream};
use regex_automata::meta::Regex;
use ring::digest;
use url::Url;

use super::{ASSETS, Error, Twitter};

const MAX_ASSETS: usize = 64;
const MAX_IMPORT_DEPTH: usize = 2;
const CONCURRENT_ASSETS: usize = 6;
const X_EPOCH: i64 = 1_682_924_400;

macro_rules! pattern {
    ($name:ident, $pattern:literal) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| {
            Regex::builder()
                .syntax(
                    regex_automata::util::syntax::Config::new()
                        .unicode(false)
                        .utf8(false),
                )
                .build($pattern)
                .expect("constant X signing pattern")
        });
    };
}

pattern!(META_TAG, r"(?i)<meta\b[^>]*>");
pattern!(ATTRIBUTE, r#"([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#);
pattern!(SVG, r"(?is)<svg\b([^>]*)>(.*?)</svg\s*>");
pattern!(GROUP, r"(?is)<g(?:\s[^>]*)?>(.*?)</g\s*>");
pattern!(PATH, r"(?i)<path\b[^>]*>");
pattern!(
    PAGE_ASSET,
    r"https://abs\.twimg\.com/(?:x-web/|responsive-web/client-web/)[\w./-]+\.js"
);
pattern!(LEGACY_MAIN, r"/responsive-web/client-web/main\.[\w.-]+\.js");
pattern!(WEBPACK_ENTRY, r#"(\d+)\s*:\s*"([^"]+)""#);
pattern!(
    NAMED_ASSET,
    r#"["'\x60]((?:[\w./-]*/)?(?:ondemand\.s|sign\.o)[\w.-]*\.js)["'\x60]"#
);
pattern!(
    STATIC_IMPORT,
    r#"\b(?:from|import)\s*["'\x60]([\w./-]+\.js)["'\x60]"#
);
pattern!(
    LAZY_IMPORT,
    r#"\bimport\(\s*["'\x60]([\w./-]+\.js)["'\x60]\s*\)\.then\(\s*\(?\w+\)?\s*=>\s*\w+\.default\(\)"#
);
pattern!(INDEX, r"\(\w\[(\d{1,2})\],\s*16\)");

pub(super) struct Signer {
    key: Vec<u8>,
    animation: String,
}

pub(super) async fn load(client: &Twitter) -> Result<Signer, Error> {
    let html = client.bootstrap("/tesla").await?;
    match from_page(client, &html).await {
        Err(Error::Signing) => {
            let alternate = client.bootstrap("/i/jf/").await?;
            from_page(client, &alternate).await
        }
        result => result,
    }
}

async fn from_page(client: &Twitter, html: &str) -> Result<Signer, Error> {
    let assets = page_assets(html)?;
    let key = verification_key(html)?;
    let paths = animation_paths(html);
    let path = paths
        .get(usize::from(key[5]) % paths.len().max(1))
        .ok_or(Error::Signing)?;
    let indices = ScriptSearch::new(client).find(assets).await?;
    let animation = animation_key(path, &key, &indices)?;
    Ok(Signer { key, animation })
}

impl Signer {
    pub(super) fn sign(&self, path: &str, now: i64) -> Result<String, Error> {
        self.encode(path, now, rand::random())
    }

    fn encode(&self, path: &str, now: i64, mask: u8) -> Result<String, Error> {
        let time = now
            .checked_sub(X_EPOCH)
            .and_then(|ts| u32::try_from(ts).ok())
            .ok_or(Error::Signing)?;
        let message = format!("GET!{path}!{time}obfiowerehiring{}", self.animation);
        let hash = digest::digest(&digest::SHA256, message.as_bytes());
        let payload = self
            .key
            .iter()
            .copied()
            .chain(time.to_le_bytes())
            .chain(hash.as_ref()[..16].iter().copied())
            .chain([3]);
        Ok(STANDARD_NO_PAD.encode(
            std::iter::once(mask)
                .chain(payload.map(|byte| byte ^ mask))
                .collect::<Vec<_>>(),
        ))
    }
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    ATTRIBUTE.captures_iter(tag).find_map(|captures| {
        let key = captures.get_group(1)?;
        if !tag[key].eq_ignore_ascii_case(name) {
            return None;
        }
        let value = captures.get_group(2).or_else(|| captures.get_group(3))?;
        Some(&tag[value])
    })
}

fn verification_key(html: &str) -> Result<Vec<u8>, Error> {
    let value = META_TAG
        .find_iter(html)
        .find_map(|tag| {
            let tag = &html[tag.range()];
            (attribute(tag, "name") == Some("twitter-site-verification"))
                .then(|| attribute(tag, "content"))
                .flatten()
        })
        .ok_or(Error::Signing)?;
    let key = STANDARD.decode(value.trim()).map_err(|_| Error::Signing)?;
    (6..=256)
        .contains(&key.len())
        .then_some(key)
        .ok_or(Error::Signing)
}

fn animation_paths(html: &str) -> Vec<&str> {
    SVG.captures_iter(html)
        .filter_map(|captures| {
            let attrs = &html[captures.get_group(1)?];
            if !attribute(attrs, "id")?.starts_with("loading-x-anim") {
                return None;
            }
            let body = &html[captures.get_group(2)?];
            let group = GROUP.captures_iter(body).next()?;
            let paths = &body[group.get_group(1)?];
            let second = PATH.find_iter(paths).nth(1)?;
            attribute(&paths[second.range()], "d")
        })
        .collect()
}

fn page_assets(html: &str) -> Result<Vec<Url>, Error> {
    let mut assets: Vec<_> = PAGE_ASSET
        .find_iter(html)
        .filter_map(|found| Url::parse(&html[found.range()]).ok())
        .filter(is_asset)
        .collect();
    let modern = assets.iter().any(|url| url.path().starts_with("/x-web/"));
    if modern {
        assets.retain(|url| url.path().starts_with("/x-web/"));
        if assets
            .iter()
            .any(|url| url.path().contains("/entry-client-logged-out"))
        {
            return Err(Error::Authentication);
        }
        // Entry modules usually lead to the signing chunk more directly than vendor preloads.
        assets.sort_by_key(|url| !url.path().contains("/entry-client"));
    } else {
        if let Some(path) = LEGACY_MAIN.find(html) {
            let url = format!("https://{ASSETS}{}", &html[path.range()]);
            assets.extend(Url::parse(&url).ok().filter(is_asset));
        }
        assets.extend(webpack_signing_asset(html));
    }
    if assets.is_empty() && html.contains("/cdn-cgi/challenge-platform/") {
        return Err(Error::Authentication);
    }
    if assets.is_empty() && (html.contains("/i/flow/login") || html.contains("/account/access")) {
        return Err(Error::Authentication);
    }
    Ok(assets)
}

fn webpack_signing_asset(html: &str) -> Option<Url> {
    let entries = || {
        WEBPACK_ENTRY.captures_iter(html).filter_map(|captures| {
            Some((&html[captures.get_group(1)?], &html[captures.get_group(2)?]))
        })
    };
    let (name, _) = entries().find(|(_, value)| *value == "ondemand.s")?;
    let (_, hash) = entries().find(|(chunk, value)| {
        *chunk == name
            && matches!(value.len(), 7 | 16)
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })?;
    Url::parse(&format!(
        "https://{ASSETS}/responsive-web/client-web/ondemand.s.{hash}a.js"
    ))
    .ok()
    .filter(is_asset)
}

pub(super) fn is_asset(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(ASSETS)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path().ends_with(".js")
        && (url.path().starts_with("/x-web/")
            || url.path().starts_with("/responsive-web/client-web/"))
}

#[derive(Default)]
struct Script {
    indices: Vec<usize>,
    named: Vec<Url>,
    lazy: Vec<Url>,
    imports: Vec<Url>,
}

impl Script {
    fn parse(url: &Url, text: &str) -> Self {
        let links = |pattern: &Regex| {
            pattern
                .captures_iter(text)
                .filter_map(|captures| {
                    let path = &text[captures.get_group(1)?];
                    url.join(path).ok().filter(is_asset)
                })
                .collect()
        };
        Self {
            indices: INDEX
                .captures_iter(text)
                .filter_map(|captures| text[captures.get_group(1)?].parse().ok())
                .take(9)
                .collect(),
            named: links(&NAMED_ASSET),
            lazy: links(&LAZY_IMPORT),
            imports: links(&STATIC_IMPORT),
        }
    }
}

struct ScriptSearch<'a> {
    client: &'a Twitter,
    scripts: HashMap<Url, Script>,
    seen: HashSet<Url>,
    first_failure: Option<Error>,
}

impl<'a> ScriptSearch<'a> {
    fn new(client: &'a Twitter) -> Self {
        Self {
            client,
            scripts: HashMap::new(),
            seen: HashSet::new(),
            first_failure: None,
        }
    }

    async fn load(&mut self, urls: &[Url]) -> Result<(), Error> {
        let slots = MAX_ASSETS - self.seen.len();
        let requests: Vec<_> = urls
            .iter()
            .filter(|url| self.seen.insert((*url).clone()))
            .take(slots)
            .cloned()
            .collect();
        let client = self.client;
        let mut replies = stream::iter(requests)
            .map(|url| async move {
                let result = client.script(url.clone()).await;
                (url, result)
            })
            .buffer_unordered(CONCURRENT_ASSETS);
        while let Some((url, result)) = replies.next().await {
            match result {
                Ok(text) => {
                    self.scripts.insert(url.clone(), Script::parse(&url, &text));
                }
                Err(Error::RateLimited) => return Err(Error::RateLimited),
                Err(error) => {
                    self.first_failure.get_or_insert(error);
                }
            }
        }
        Ok(())
    }

    fn candidate(&self, urls: &[Url]) -> Option<Vec<usize>> {
        urls.iter()
            .filter_map(|url| self.scripts.get(url))
            .find_map(|script| {
                (2..=8)
                    .contains(&script.indices.len())
                    .then(|| script.indices.clone())
            })
    }

    async fn find(mut self, mut level: Vec<Url>) -> Result<Vec<usize>, Error> {
        let direct: Vec<_> = level
            .iter()
            .filter(|url| named_script(url))
            .cloned()
            .collect();
        self.load(&direct).await?;
        if let Some(indices) = self.candidate(&direct) {
            return Ok(indices);
        }
        let mut expanded = HashSet::new();
        for _ in 0..=MAX_IMPORT_DEPTH {
            self.load(&level).await?;
            let mut candidates = Vec::new();
            let mut next = Vec::new();
            for url in level {
                if expanded.insert(url.clone())
                    && let Some(script) = self.scripts.get(&url)
                {
                    candidates.extend(script.named.iter().chain(&script.lazy).cloned());
                    next.extend(script.imports.iter().cloned());
                }
            }
            self.load(&candidates).await?;
            if let Some(indices) = self.candidate(&candidates) {
                return Ok(indices);
            }
            if next.is_empty() || self.seen.len() >= MAX_ASSETS {
                break;
            }
            level = next;
        }
        if self.scripts.is_empty() {
            return Err(self.first_failure.unwrap_or(Error::Signing));
        }
        Err(Error::Signing)
    }
}

fn named_script(url: &Url) -> bool {
    url.path()
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with("ondemand.s") || name.starts_with("sign.o"))
}

fn animation_key(path: &str, key: &[u8], indices: &[usize]) -> Result<String, Error> {
    let mut values = indices
        .iter()
        .map(|&index| key.get(index).copied().ok_or(Error::Signing));
    let row = usize::from(values.next().ok_or(Error::Signing)?? % 16);
    let time = values.try_fold(1.0, |time, value| Ok(time * f64::from(value? % 16)))?;
    let frame = path
        .get(9..)
        .ok_or(Error::Signing)?
        .split('C')
        .nth(row)
        .ok_or(Error::Signing)?;
    let channels: Vec<_> = frame
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(str::parse::<u8>)
        .collect::<Result<_, _>>()
        .map_err(|_| Error::Signing)?;
    let frame: [u8; 11] = channels.try_into().map_err(|_| Error::Signing)?;
    let time = (time / 10.0_f64).round() * 10.0 / 4096.0;
    Ok(interpolated_animation(frame, time))
}

fn interpolated_animation(frame: [u8; 11], time: f64) -> String {
    let curves = std::array::from_fn(|i| {
        let start = if i % 2 == 0 { 0.0 } else { -1.0 };
        round2(f64::from(frame[7 + i]) / 255.0 * (1.0 - start) + start)
    });
    let factor = bezier_value(curves, time);
    let mut result = String::new();
    for i in 0..3 {
        let mixed = f64::from(frame[i]) * (1.0 - factor) + f64::from(frame[i + 3]) * factor;
        use std::fmt::Write as _;
        let _ = write!(
            result,
            "{:x}",
            mixed.clamp(0.0, 255.0).round_ties_even() as u8
        );
    }
    let angle = ((f64::from(frame[6]) / 255.0 * 300.0 + 60.0).floor() * factor).to_radians();
    let (sin, cos) = angle.sin_cos();
    for value in [cos, -sin, sin, cos] {
        matrix_hex(&mut result, round2(value).abs());
    }
    result.push_str("00");
    result
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round_ties_even() / 100.0
}

fn bezier_value([x1, y1, x2, y2]: [f64; 4], time: f64) -> f64 {
    let sample = |a, b, t: f64| {
        3.0 * a * (1.0 - t).powi(2) * t + 3.0 * b * (1.0 - t) * t.powi(2) + t.powi(3)
    };
    if time <= 0.0 {
        return time
            * if x1 > 0.0 {
                y1 / x1
            } else if y1 == 0.0 && x2 > 0.0 {
                y2 / x2
            } else {
                0.0
            };
    }
    if time >= 1.0 {
        let slope = if x2 < 1.0 {
            (y2 - 1.0) / (x2 - 1.0)
        } else if x2 == 1.0 && x1 < 1.0 {
            (y1 - 1.0) / (x1 - 1.0)
        } else {
            0.0
        };
        return 1.0 + slope * (time - 1.0);
    }
    let mut low = 0.0;
    let mut high = 1.0;
    let mut mid = 0.5;
    for _ in 0..53 {
        mid = (low + high) / 2.0;
        let x = sample(x1, x2, mid);
        if (x - time).abs() < 0.00001 {
            break;
        }
        if x < time {
            low = mid;
        } else {
            high = mid;
        }
    }
    sample(y1, y2, mid)
}

fn matrix_hex(out: &mut String, value: f64) {
    // The web protocol represents the exact binary fraction in base 16.
    let digits = b"0123456789abcdef";
    out.push(char::from(digits[value as usize]));
    let mut fraction = value.fract();
    while fraction > 0.0 {
        fraction *= 16.0;
        let digit = fraction as usize;
        out.push(char::from(digits[digit]));
        fraction -= digit as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_id_matches_a_reference_vector() {
        let key: Vec<u8> = (0..32).collect();
        let path = "M 10,30 C16 32 64 16 32 64 0 0 0 255 255";
        let animation = animation_key(path, &key, &[0, 0]).unwrap();
        assert_eq!(animation, "102040100100");
        let signer = Signer { key, animation };
        assert_eq!(
            signer
                .encode("/i/api/graphql/test/TweetDetail", X_EPOCH + 12345, 42)
                .unwrap(),
            "KiorKCkuLywtIiMgISYnJCU6Ozg5Pj88PTIzMDE2NzQ1ExoqKm3UOeQJmtMOa1Hj0SlZHgcp"
        );
    }

    #[test]
    fn animation_matches_nonzero_reference_time() {
        let key: Vec<u8> = (0..32).collect();
        let path = "M 10,30 C0 30 255 80 200 16 200 0 127 255 255";
        assert_eq!(
            animation_key(path, &key, &[0, 9, 13]).unwrap(),
            "223f80fd70a3d70a3d70266666666666660266666666666660fd70a3d70a3d700"
        );
    }

    #[test]
    fn accepts_legacy_chunk_hash_lengths_and_rejects_foreign_assets() {
        for hash in ["1234abc", "1234abc567ef89ab"] {
            let html = format!(r#"1:"ondemand.s" 1:"{hash}""#);
            assert_eq!(
                webpack_signing_asset(&html).unwrap().path(),
                format!("/responsive-web/client-web/ondemand.s.{hash}a.js")
            );
        }
        let url = Url::parse("https://abs.twimg.com/x-web/x-web/start.js").unwrap();
        let script = Script::parse(
            &url,
            r#"import "./assets/child.js"; import("./assets/key.js").then(m=>m.default()); import "../../../escape.js";"#,
        );
        assert_eq!(script.imports.len(), 1);
        assert_eq!(script.lazy[0].path(), "/x-web/x-web/assets/key.js");
        assert!(!is_asset(
            &Url::parse("https://abs.twimg.com.evil.test/x-web/key.js").unwrap()
        ));
    }
}
