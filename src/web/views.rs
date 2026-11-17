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
];

pub const APP_CSS: &str = include_str!("../../static/app.css");

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
            if value.chars().count() <= max {
                value
            } else {
                let cut: String = value.chars().take(max.saturating_sub(1)).collect();
                format!("{cut}…")
            }
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
                }),
            )
            .unwrap();
        assert!(html.contains("nujijushi · dashboard"));
        assert!(html.contains("recent sends"));
        assert!(html.contains(r#"class="num">3"#));
    }
}
