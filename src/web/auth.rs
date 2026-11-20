//! Minimal single-user auth: a signed cookie derived from the UI token.

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use std::sync::Arc;

use super::WebState;

pub const COOKIE: &str = "nujijushi_session";

/// `key:hash(token)` - deterministic per token, no state needed.
pub fn derive(token: &str, purpose: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nujijushi-ui-v1");
    hasher.update(purpose.as_bytes());
    hasher.update(token.as_bytes());
    format!("{purpose}:{}", hasher.finalize().to_hex())
}

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn cookie_value(headers: &header::HeaderMap) -> Option<String> {
    read_cookie(headers, COOKIE).map(str::to_owned)
}

pub(super) fn read_cookie<'a>(headers: &'a header::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

pub fn check(req: &Request, ws: &WebState) -> bool {
    cookie_value(req.headers())
        .map(|value| constant_time_eq(&value, &ws.session))
        .unwrap_or(false)
}

/// Middleware: everything except the login page and static assets requires a session.
pub async fn require_session(
    State(ws): State<Arc<WebState>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    // metrics sits outside the session gate on purpose: a scraper has no cookie,
    // and the document only carries counts (never titles, captions or tokens)
    if path == "/login"
        || path.starts_with("/static/")
        || path == "/metrics"
        || path == crate::bot::WEBHOOK_PATH
    {
        return next.run(req).await;
    }
    if check(&req, &ws) {
        return next.run(req).await;
    }
    let target = if req.uri().query().is_some() {
        format!("/login?next={}", encode(&path))
    } else {
        "/login".to_string()
    };
    Redirect::to(&target).into_response()
}

fn encode(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '/' | '.' => c.to_string(),
            other => format!("%{:02X}", other as u32 & 0xff),
        })
        .collect()
}

/// `Set-Cookie` header for a successful login.
pub fn session_cookie(value: &str) -> String {
    format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000")
}

pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_values_are_stable_and_distinct() {
        let a = derive("secret", "session");
        assert_eq!(a, derive("secret", "session"));
        assert_ne!(a, derive("secret", "csrf"));
        assert_ne!(a, derive("other", "session"));
        assert!(a.starts_with("session:"));
    }

    #[test]
    fn compares_in_constant_time() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}
