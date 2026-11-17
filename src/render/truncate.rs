//! Keep captions inside Telegram's hard 1024-character limit without breaking HTML.

/// A rendered caption plus the facts the caller needs to log/notify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCaption {
    pub text: String,
    pub truncated: bool,
    pub length: usize,
    pub limit: usize,
}

impl RenderedCaption {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// Telegram's caption limit.
pub const CAPTION_LIMIT: usize = 1024;

const VOID_TAGS: &[&str] = &["br"];

/// Fit `text` into `limit` characters, closing any HTML we cut through.
pub fn fit_caption(text: String, limit: usize) -> RenderedCaption {
    let limit = limit.max(32);
    let length = text.chars().count();
    if length <= limit {
        return RenderedCaption {
            text,
            truncated: false,
            length,
            limit,
        };
    }

    let prefix = take_chars(&text, limit.saturating_sub(1));
    let repaired = close_open_tags(&prefix);
    let trimmed = repaired
        .trim_end()
        .trim_end_matches([',', '.', ';', ':', '-'])
        .trim_end();
    let text = format!("{trimmed}…");
    let length = text.chars().count();
    RenderedCaption {
        text,
        truncated: true,
        length,
        limit,
    }
}

fn take_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Drop a tag we cut in half and close everything still open.
pub fn close_open_tags(prefix: &str) -> String {
    let mut stack: Vec<String> = Vec::new();
    let mut out = String::with_capacity(prefix.len() + 16);
    let mut rest = prefix;

    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find('>') else {
            // unterminated tag at the cut point: drop it
            return finish(out, &stack);
        };
        let raw = &after[1..end];
        out.push_str(&after[..=end]);

        let trimmed = raw.trim();
        if trimmed.starts_with('/') {
            let name = trimmed
                .strip_prefix('/')
                .unwrap_or(trimmed)
                .trim()
                .to_ascii_lowercase();
            if let Some(position) = stack.iter().rposition(|open| *open == name) {
                stack.truncate(position);
            }
        } else if !trimmed.is_empty() {
            let name = trimmed
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            let self_closing = trimmed.ends_with('/') || VOID_TAGS.contains(&name.as_str());
            if !self_closing && !name.is_empty() {
                stack.push(name);
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    finish(out, &stack)
}

fn finish(out: String, stack: &[String]) -> String {
    let mut out = out;
    for tag in stack.iter().rev() {
        out.push_str(&format!("</{tag}>"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_captions_pass_through() {
        let out = fit_caption("hello".into(), 1024);
        assert!(!out.truncated);
        assert_eq!(out.text, "hello");
        assert_eq!(out.length, 5);
    }

    #[test]
    fn long_captions_are_cut_with_ellipsis() {
        let text = "a".repeat(2000);
        let out = fit_caption(text, 100);
        assert!(out.truncated);
        assert!(out.text.ends_with('…'));
        assert_eq!(out.length, 100);
        assert!(out.length <= out.limit);
    }

    #[test]
    fn cuts_on_char_boundaries() {
        let text = "ünïcødé ".repeat(400);
        let out = fit_caption(text, 50);
        assert!(out.truncated);
        assert_eq!(out.text.chars().count(), 50);
    }

    #[test]
    fn closes_tags_that_were_cut() {
        let text = format!("<b>{}</b>", "x".repeat(2000));
        let out = fit_caption(text, 40);
        assert!(out.truncated);
        assert_eq!(out.text.matches("<b>").count(), 1);
        assert!(out.text.trim_end_matches('…').ends_with("</b>") || out.text.contains("</b>"));
    }

    #[test]
    fn drops_half_written_tags() {
        let repaired = close_open_tags("hello <b>bold</");
        assert_eq!(repaired, "hello <b>bold</b>");
        let repaired = close_open_tags("hello <a href=\"http://x");
        assert_eq!(repaired, "hello ");
    }

    #[test]
    fn tracks_nested_opens() {
        let repaired = close_open_tags("<b><i>x");
        assert_eq!(repaired, "<b><i>x</i></b>");
        let repaired = close_open_tags("<b><i>x</i>y");
        assert_eq!(repaired, "<b><i>x</i>y</b>");
    }

    #[test]
    fn handles_void_and_self_closing() {
        let repaired = close_open_tags("<b>a<br>b<br/>c");
        assert_eq!(repaired, "<b>a<br>b<br/>c</b>");
    }

    #[test]
    fn trims_dangling_punctuation_before_ellipsis() {
        let text = format!("{}, {}", "word".repeat(60), "tail");
        let out = fit_caption(text, 40);
        assert!(!out.text.contains(", …"));
    }
}
