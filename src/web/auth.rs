//! Admin authentication using the UI token or its derived session cookie.

use axum::extract::{Request, State};
use axum::http::{Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use std::sync::Arc;

use super::WebState;

pub const COOKIE: &str = "nujijushi_session";

/// `key:hash(token)` - deterministic per token, no state needed.
pub fn derive(token: &str, purpose: &str) -> String {
    derive_at(token, purpose, "")
}

pub(super) fn derive_at(token: &str, purpose: &str, base_path: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    // Keep existing root sessions valid and scope new mounts independently.
    if !base_path.is_empty() {
        hasher.update(b"nujijushi-web-base-path\0");
        hasher.update(base_path.as_bytes());
        hasher.update(b"\0");
    }
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

fn check_bearer(req: &Request, ws: &WebState) -> bool {
    let mut values = req.headers().get_all(header::AUTHORIZATION).iter();
    let Some((scheme, token)) = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
    else {
        return false;
    };
    values.next().is_none()
        && scheme.eq_ignore_ascii_case("bearer")
        && !token.is_empty()
        && ws
            .app
            .cfg
            .ui_token
            .as_deref()
            .is_some_and(|expected| constant_time_eq(token, expected.trim()))
}

/// Middleware: everything except the login page and static assets requires a session.
pub async fn require_session(
    State(ws): State<Arc<WebState>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    // Monitoring endpoints expose only health, build metadata and counts.
    if path == "/login"
        || path == "/api/v1/login"
        || path.starts_with("/admin-assets/")
        || path == "/metrics"
        || path == "/healthz"
        || path == crate::bot::WEBHOOK_PATH
    {
        return next.run(req).await;
    }
    let api = path.starts_with("/api/v1/") || path == "/api/status";
    let has_bearer_header = api
        && req
            .headers()
            .get_all(header::AUTHORIZATION)
            .iter()
            .any(|value| {
                value.to_str().ok().is_some_and(|value| {
                    value
                        .split_ascii_whitespace()
                        .next()
                        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"))
                })
            });
    let authorized = if has_bearer_header {
        check_bearer(&req, &ws)
    } else {
        check(&req, &ws)
    };
    if authorized {
        return next.run(req).await;
    }
    if path.starts_with("/api/v1/") || has_bearer_header {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            [(header::CACHE_CONTROL, "no-store")],
            axum::Json(serde_json::json!({"error": "Sign in to continue"})),
        )
            .into_response();
    }
    let target = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("next", &req.uri().to_string())
        .finish();
    (
        [(header::CACHE_CONTROL, "no-store")],
        Redirect::to(&ws.url(&format!("/login?{target}"))),
    )
        .into_response()
}

pub(super) fn local_page(value: &str) -> Option<Uri> {
    if !value.starts_with('/') || value.starts_with("//") || value.contains('\\') {
        return None;
    }
    let uri = value.parse::<Uri>().ok()?;
    (uri.scheme().is_none() && uri.authority().is_none()).then_some(uri)
}

pub(super) fn cookie(name: &str, value: &str, path: &str, max_age: u32, secure: bool) -> String {
    let mut cookie =
        format!("{name}={value}; Path={path}; HttpOnly; SameSite=Strict; Max-Age={max_age}");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
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
        assert_ne!(a, derive_at("secret", "session", "/archive"));
        assert!(a.starts_with("session:"));
    }

    #[test]
    fn compares_in_constant_time() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }

    #[test]
    fn cookies_can_be_marked_secure() {
        let plain = cookie("n", "v", "/", 60, false);
        assert!(!plain.contains("Secure"), "{plain}");
        assert!(plain.contains("HttpOnly") && plain.contains("SameSite=Strict"));
        let secure = cookie("n", "v", "/", 60, true);
        assert!(secure.contains("; Secure"), "{secure}");
    }
}
