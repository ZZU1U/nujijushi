//! Page templates and rendering helpers.

use minijinja::{AutoEscape, Environment};
use serde_json::Value;

use crate::error::{Error, Result};

const TEMPLATES: &[(&str, &str)] = &[
    ("base.html", include_str!("../../templates/base.html")),
    ("login.html", include_str!("../../templates/login.html")),
    (
        "dashboard.html",
        include_str!("../../templates/dashboard.html"),
    ),
    ("posts.html", include_str!("../../templates/posts.html")),
    ("watches.html", include_str!("../../templates/watches.html")),
    (
        "duplicates.html",
        include_str!("../../templates/duplicates.html"),
    ),
    (
        "post_detail.html",
        include_str!("../../templates/post_detail.html"),
    ),
    (
        "channels.html",
        include_str!("../../templates/channels.html"),
    ),
    (
        "channel_edit.html",
        include_str!("../../templates/channel_edit.html"),
    ),
    ("queue.html", include_str!("../../templates/queue.html")),
    ("preview.html", include_str!("../../templates/preview.html")),
    ("import.html", include_str!("../../templates/import.html")),
    ("users.html", include_str!("../../templates/users.html")),
    ("jobs.html", include_str!("../../templates/jobs.html")),
];

pub const APP_CSS: &str = include_str!("../../static/app.css");

/// Progressive-enhancement script; every page still works without it.
pub const APP_JS: &str = include_str!("../../static/app.js");

pub struct PageRenderer {
    env: Environment<'static>,
}

impl PageRenderer {
    pub fn new() -> Result<Self> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|name| {
            if name.ends_with(".html") {
                AutoEscape::Html
            } else {
                AutoEscape::None
            }
        });
        env.add_filter("shorten", |value: String, max: usize| {
            crate::render::truncate::shorten(&value, max)
        });
        env.add_filter("one_line", |value: String| {
            value.split_whitespace().collect::<Vec<_>>().join(" ")
        });
        for (name, source) in TEMPLATES {
            env.add_template(name, source).map_err(Error::Template)?;
        }
        Ok(Self { env })
    }

    pub fn render(&self, name: &str, ctx: &Value) -> Result<String> {
        let template = self.env.get_template(name).map_err(Error::Template)?;
        template.render(json_to_value(ctx)).map_err(Error::Template)
    }
}

/// serde_json -> minijinja value tree.
///
/// `null` becomes an empty string: templates can test it for truthiness and it
/// never prints a stray "None" into the page.
pub fn json_to_value(json: &Value) -> minijinja::Value {
    use minijinja::Value as MjValue;
    match json {
        Value::Null => MjValue::from(""),
        Value::Bool(value) => MjValue::from(*value),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                MjValue::from(int)
            } else if let Some(uint) = number.as_u64() {
                MjValue::from(uint)
            } else {
                MjValue::from(number.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(text) => MjValue::from(text.clone()),
        Value::Array(items) => {
            MjValue::from(items.iter().map(json_to_value).collect::<Vec<MjValue>>())
        }
        Value::Object(map) => MjValue::from(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_value(value)))
                .collect::<std::collections::BTreeMap<String, MjValue>>(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn templates_compile() {
        let renderer = PageRenderer::new().unwrap();
        for (name, _) in TEMPLATES {
            assert!(renderer.env.get_template(name).is_ok(), "{name} missing");
        }
    }

    #[test]
    fn renders_dashboard_shell() {
        let renderer = PageRenderer::new().unwrap();
        let html = renderer
            .render(
                "dashboard.html",
                &json!({
                    "page": "dashboard",
                    "csrf": "tok",
                    "flash": null,
                    "stats": {"posts": 3, "approved": 1, "queued": 2, "sent": 5, "unknown": 0},
                    "channels": [],
                    "sends": [],
                    "jobs": [],
                    "recent_unknown": [],
                    "scheduler": {"stale": false, "age": "4s", "last_tick": "2026-01-01T00:00:00Z"},
                }),
            )
            .unwrap();
        assert!(html.contains("nujijushi · dashboard"));
        assert!(html.contains("recent sends"));
        assert!(!html.contains("href=\"/duplicates\""));
        assert!(html.contains(r#"class="num">3"#));
    }

    #[test]
    fn renders_users_with_effective_role_and_real_counts() {
        let renderer = PageRenderer::new().unwrap();
        let html = renderer
            .render(
                "users.html",
                &json!({
                    "page": "users",
                    "csrf": "tok",
                    "flash": null,
                    "users": [
                        {
                            "tg_id": 7,
                            "label": "@owner",
                            "role": "admin",
                            "db_role": "guest",
                            "archived": 12,
                            "pushed": 5,
                            "sent": 3,
                            "config_admin": true,
                            "last_seen": "2026-01-01 10:00",
                        },
                        {
                            "tg_id": 8,
                            "label": "@spammer",
                            "role": "banned",
                            "db_role": "banned",
                            "archived": 0,
                            "pushed": 0,
                            "sent": 0,
                            "config_admin": false,
                            "last_seen": "",
                        }
                    ],
                }),
            )
            .unwrap();
        // a config admin is shown as admin even while the db row still says guest
        assert!(html.contains(">admin</span>"), "{html}");
        assert!(!html.contains(">guest</span>"), "the stale db role leaks");
        assert!(html.contains(">12</td>"));
        assert!(html.contains(">banned</span>"));
    }

    #[test]
    fn renders_pushed_cards_dimmed_with_a_channel_badge() {
        let renderer = PageRenderer::new().unwrap();
        let card = |id: i64, pushed: i64, delivered: i64, channels: Option<&str>| {
            json!({
                "id": id,
                "source": "twitter",
                "rating": "sfw",
                "state": "approved",
                "artist": "asobinin",
                "media_count": 1,
                "thumb": null,
                "nsfw": false,
                "snippet": "hello",
                "pushed": pushed,
                "delivered": delivered,
                "channels": channels,
                "imported": "01-01 10:00",
            })
        };
        let html = renderer
            .render(
                "posts.html",
                &json!({
                    "page": "posts",
                    "csrf": "tok",
                    "flash": null,
                    "cards": [
                        card(1, 0, 0, None),
                        card(2, 1, 0, Some("night-shift")),
                        card(3, 1, 1, Some("main")),
                    ],
                    "total": 3,
                    "page_no": 1,
                    "has_next": false,
                    "filters": {"state": null, "source": null, "tag": null, "nsfw": null, "search": null},
                    "sources": [],
                    "top_tags": [],
                    "channels": [],
                    "states": [],
                }),
            )
            .unwrap();
        assert_eq!(
            html.matches("card post pushed").count(),
            2,
            "only the two pushed cards are dimmed"
        );
        assert!(html.contains("queued · night-shift"), "{html}");
        assert!(html.contains("sent · main"));
        assert!(html.contains("Select all on page"));
        assert_eq!(html.matches("href=\"/duplicates\"").count(), 1);
        assert!(html.contains("class=\"hint post-duplicates-link\""));
        assert!(html.contains("for=\"post-pick-2\""));
        assert!(html.contains("name=\"sel_2\" id=\"post-pick-2\""));
        assert!(html.contains("aria-label=\"Open post #2\""));
    }

    /// A silent queue must be explainable: the banner only shows when the
    /// scheduler has not ticked (as in a `serve`-only setup).
    #[test]
    fn dashboard_says_when_nothing_is_scheduling() {
        let renderer = PageRenderer::new().unwrap();
        let base = json!({
            "page": "dashboard",
            "csrf": "tok",
            "flash": null,
            "stats": {"posts": 0, "approved": 0, "queued": 0, "sent": 0, "unknown": 0},
            "channels": [],
            "sends": [],
            "jobs": [],
            "recent_unknown": [],
        });
        let html = renderer.render("dashboard.html", &base).unwrap();
        assert!(!html.contains("nothing is publishing on a schedule"));

        let mut stale = base.clone();
        stale["scheduler"] = json!({
            "stale": true, "age_secs": 7200, "age": "2h",
            "last_tick": "2026-01-01T00:00:00Z",
        });
        let html = renderer.render("dashboard.html", &stale).unwrap();
        assert!(
            html.contains("nothing is publishing on a schedule"),
            "banner missing: {html}"
        );
        assert!(html.contains("last ran 2h ago"), "age missing: {html}");
        assert!(html.contains("nujijushi run"), "hint missing: {html}");
    }
}
