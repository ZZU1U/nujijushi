//! Caption rendering: one minijinja environment shared by publishing and previews.

pub mod caption;
pub mod filters;
pub mod truncate;

use std::collections::HashMap;

use minijinja::{AutoEscape, Environment};

use crate::error::{Error, Result};

pub use caption::{CaptionContext, build_context};
pub use truncate::{CAPTION_LIMIT, RenderedCaption};

/// Bundled caption templates; a channel can also carry its own inline template.
pub const BUILTIN_TEMPLATES: &[(&str, &str)] = &[
    (
        "default",
        include_str!("../../templates/captions/default.html.j2"),
    ),
    (
        "compact",
        include_str!("../../templates/captions/compact.html.j2"),
    ),
    (
        "detailed",
        include_str!("../../templates/captions/detailed.html.j2"),
    ),
];

pub struct Renderer {
    env: Environment<'static>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(|name| {
            if name.contains("html") || name.contains("j2") {
                AutoEscape::Html
            } else {
                AutoEscape::None
            }
        });
        for (name, source) in BUILTIN_TEMPLATES {
            env.add_template(name, source).map_err(template_error)?;
        }
        filters::register(&mut env)?;
        Ok(Self { env })
    }

    pub fn template_names(&self) -> Vec<String> {
        BUILTIN_TEMPLATES
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect()
    }

    /// Render a caption with the channel's template (inline template wins).
    pub fn render(
        &self,
        channel: &crate::model::Channel,
        ctx: &CaptionContext,
    ) -> Result<RenderedCaption> {
        let source = match channel.template.as_deref() {
            Some(inline) if !inline.trim().is_empty() => inline.to_string(),
            _ => match self.template_source(&channel.template_name) {
                Ok(source) => source,
                Err(err) => {
                    // A bad name must not take the channel out of service. New
                    // writes are validated, so this only heals legacy rows.
                    tracing::warn!(
                        channel = channel.id,
                        template = %channel.template_name,
                        "unknown caption template; using the default: {err}"
                    );
                    self.template_source("default")?
                }
            },
        };
        self.render_source(&source, ctx, channel.silent)
    }

    /// Validate everything a channel needs to render export/publish captions.
    pub fn validate_channel(&self, channel: &crate::model::Channel) -> Result<()> {
        match channel.template.as_deref() {
            Some(inline) if !inline.trim().is_empty() => self.validate_template(inline),
            _ => self.template_source(&channel.template_name).map(|_| ()),
        }
    }

    /// Render an arbitrary template body (used by the live preview in the web UI).
    pub fn render_source(
        &self,
        source: &str,
        ctx: &CaptionContext,
        _silent: bool,
    ) -> Result<RenderedCaption> {
        let name = "inline.html.j2";
        let mut env = self.env.clone();
        env.add_template_owned(name.to_string(), source.to_string())
            .map_err(template_error)?;
        let template = env.get_template(name).map_err(template_error)?;
        let text = template.render(&ctx.value).map_err(template_error)?;
        let cleaned = normalize_whitespace(&text);
        Ok(truncate::fit_caption(cleaned, ctx.limit))
    }

    pub fn template_source(&self, name: &str) -> Result<String> {
        BUILTIN_TEMPLATES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, source)| (*source).to_string())
            .ok_or_else(|| Error::invalid(format!("unknown caption template: {name}")))
    }

    /// Validate a user template without rendering it for a real post.
    pub fn validate_template(&self, source: &str) -> Result<()> {
        let mut env = self.env.clone();
        env.add_template_owned("validate.html.j2".to_string(), source.to_string())
            .map_err(template_error)?;
        let sample = caption::sample_context();
        env.get_template("validate.html.j2")
            .map_err(template_error)?
            .render(&sample.value)
            .map_err(template_error)?;
        Ok(())
    }
}

fn template_error(err: minijinja::Error) -> Error {
    Error::Template(err)
}

/// Trim the caption and collapse runs of blank lines (templates are whitespace-happy).
pub fn normalize_whitespace(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut blank_run = 0usize;
    for line in text.replace("\r\n", "\n").split('\n') {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
            lines.push(String::new());
        } else {
            blank_run = 0;
            lines.push(trimmed.to_string());
        }
    }
    while lines.first().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

/// Templates available to a channel, for UI dropdowns.
pub fn template_catalog() -> HashMap<String, &'static str> {
    BUILTIN_TEMPLATES
        .iter()
        .map(|(n, s)| ((*n).to_string(), *s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_blank_lines() {
        assert_eq!(normalize_whitespace("\n\na\n\n\n\nb  \n\n"), "a\n\nb");
        assert_eq!(normalize_whitespace("   "), "");
    }

    #[test]
    fn renderer_exposes_default_template() {
        let renderer = Renderer::new().unwrap();
        assert!(renderer.template_names().contains(&"default".to_string()));
        assert!(
            renderer
                .template_source("default")
                .unwrap()
                .contains("author.label")
        );
        assert!(renderer.template_source("nope").is_err());
    }

    #[test]
    fn an_unknown_template_name_is_rejected_but_still_renders() {
        let renderer = Renderer::new().unwrap();
        let mut channel = crate::model::Channel::new("preview", "@preview");
        channel.template_name = "does-not-exist".into();
        assert!(
            renderer.validate_channel(&channel).is_err(),
            "new writes must reject an unknown name"
        );
        let rendered = renderer
            .render(&channel, &caption::sample_context())
            .unwrap();
        assert!(
            !rendered.text.is_empty(),
            "legacy rows fall back to the default template instead of breaking"
        );
    }
}
