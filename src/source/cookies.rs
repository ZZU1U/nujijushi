use std::collections::BTreeSet;

use reqwest::header::HeaderValue;
use url::Url;

/// Find an unexpired, root-path Netscape cookie visible to a given HTTPS host.
pub(super) fn value<'a>(jar: &'a str, host: &str, name: &str, now: i64) -> Option<&'a str> {
    entries(jar, now)
        .filter(|entry| {
            entry.name == name
                && entry.path == "/"
                && !entry.value.is_empty()
                && entry.matches_host(host)
        })
        .max_by_key(|entry| host.eq_ignore_ascii_case(entry.domain))
        .map(|entry| entry.value)
}

pub(super) fn has_for_host(jar: &str, host: &str, now: i64) -> bool {
    entries(jar, now).any(|entry| entry.matches_host(host) && !entry.value.is_empty())
}

pub(super) fn header_for(jar: &str, url: &Url, now: i64) -> Option<HeaderValue> {
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?;
    let mut cookies: Vec<_> = entries(jar, now)
        .filter(|entry| entry.matches_host(host) && entry.matches_path(url.path()))
        .collect();
    cookies.sort_by_key(|entry| {
        std::cmp::Reverse((entry.path.len(), entry.domain.eq_ignore_ascii_case(host)))
    });
    let mut seen = BTreeSet::new();
    let line = cookies
        .into_iter()
        .filter(|entry| seen.insert(entry.name))
        .map(|entry| format!("{}={}", entry.name, entry.value))
        .collect::<Vec<_>>()
        .join("; ");
    if line.is_empty() {
        return None;
    }
    let mut header = HeaderValue::from_str(&line).ok()?;
    header.set_sensitive(true);
    Some(header)
}

struct Entry<'a> {
    domain: &'a str,
    subdomains: bool,
    path: &'a str,
    name: &'a str,
    value: &'a str,
}

impl Entry<'_> {
    fn matches_host(&self, host: &str) -> bool {
        host.eq_ignore_ascii_case(self.domain)
            || (self.subdomains
                && self.domain.contains('.')
                && host
                    .split_once('.')
                    .is_some_and(|(_, parent)| parent.eq_ignore_ascii_case(self.domain)))
    }

    fn matches_path(&self, request_path: &str) -> bool {
        request_path.strip_prefix(self.path).is_some_and(|tail| {
            tail.is_empty() || self.path.ends_with('/') || tail.starts_with('/')
        })
    }
}

fn entries(jar: &str, now: i64) -> impl Iterator<Item = Entry<'_>> {
    jar.lines().filter_map(move |line| {
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') {
            return None;
        }
        let mut fields = line.split('\t');
        let domain = fields.next()?.trim_start_matches('.');
        let subdomains = fields.next()?.eq_ignore_ascii_case("TRUE");
        let path = fields.next()?;
        fields.next()?; // Secure-only cookies are valid on HTTPS origins.
        let expiry: i64 = fields.next()?.parse().ok()?;
        let name = fields.next()?;
        let value = fields.next()?;
        if fields.next().is_some()
            || (expiry != 0 && expiry <= now)
            || !path.starts_with('/')
            || name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == 0x60 || b"!#$%&'*+-.^_|~".contains(&b))
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b';' | b',' | b'"' | b'\\'))
        {
            return None;
        }
        Some(Entry {
            domain,
            subdomains,
            path,
            name,
            value,
        })
    })
}
