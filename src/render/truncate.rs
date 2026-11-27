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

/// Telegram measures message and caption length in UTF-16 code units, not
/// Unicode scalar values: an emoji counts as two. Every limit check uses this.
pub(crate) fn telegram_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Longest prefix of `text` that fits in `max` UTF-16 code units.
pub(crate) fn take_units(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    let mut used = 0;
    for ch in text.chars() {
        let width = ch.len_utf16();
        if used + width > max {
            break;
        }
        used += width;
        out.push(ch);
    }
    out
}

/// Fit `text` into `limit` UTF-16 code units, closing any HTML we cut through.
pub fn fit_caption(text: String, limit: usize) -> RenderedCaption {
    let limit = limit.max(32);
    let length = telegram_len(&text);
    if length <= limit {
        return RenderedCaption {
            text,
            truncated: false,
            length,
            limit,
        };
    }

    // Closing the tags we cut through costs units, so the cut has to leave
    // room for them: shrink the budget until the finished string fits, otherwise
    // a markup-heavy caption silently exceeds Telegram's hard limit.
    let mut budget = limit.saturating_sub(1);
    let text = loop {
        let prefix = take_units(&text, budget);
        let repaired = close_open_tags(&prefix);
        let trimmed = repaired
            .trim_end()
            .trim_end_matches([',', '.', ';', ':', '-'])
            .trim_end();
        let candidate = format!("{trimmed}…");
        let overflow = telegram_len(&candidate).saturating_sub(limit);
        if overflow == 0 || budget == 0 {
            break candidate;
        }
        budget = budget.saturating_sub(overflow.max(1));
    };
    let length = telegram_len(&text);
    RenderedCaption {
        text,
        truncated: true,
        length,
        limit,
    }
}

/// Cut plain text to at most `max` UTF-16 code units, ending in an ellipsis when
/// it had to be cut. The web `shorten` filter and the transport share this rule.
pub fn shorten(text: &str, max: usize) -> String {
    if telegram_len(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let cut = take_units(text, max - 1);
    format!("{cut}…")
}

/// Cut HTML to `limit` without leaving a tag open: Telegram rejects unbalanced
/// markup outright, so a plain unit cut would lose the whole message.
pub fn shorten_html(html: &str, limit: usize) -> String {
    fit_caption(html.to_string(), limit).text
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
    fn limits_count_utf16_units_not_scalar_values() {
        assert_eq!(telegram_len("a"), 1);
        assert_eq!(telegram_len("🎨"), 2);

        // Exactly 1024 units: 512 astral characters.
        let fits = "🎨".repeat(512);
        let out = fit_caption(fits.clone(), 1024);
        assert!(!out.truncated);
        assert_eq!(out.length, 1024);

        // 600 emoji are 1200 units, so Telegram would reject them uncut.
        let out = fit_caption("🎨".repeat(600), 1024);
        assert!(out.truncated);
        assert!(out.text.encode_utf16().count() <= 1024);
        assert_eq!(out.length, out.text.encode_utf16().count());

        let cut = shorten(&"🎨".repeat(10), 5);
        assert_eq!(telegram_len(&cut), 5, "two emoji plus the ellipsis");
        assert_eq!(cut.chars().count(), 3);

        let html = format!("<b>{}</b>", "🎨".repeat(600));
        let cut = shorten_html(&html, 100);
        assert!(telegram_len(&cut) <= 100);
        assert!(cut.contains("</b>"));
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

    #[test]
    fn shorten_keeps_short_text_and_ellipsises_the_rest() {
        assert_eq!(shorten("short", 10), "short");
        assert_eq!(shorten("exactly-10", 10), "exactly-10");
        let cut = shorten("0123456789abc", 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
        assert_eq!(shorten("anything", 0), "");
        // multi-byte text is cut on char boundaries, not bytes
        assert_eq!(shorten("アソビニン", 3).chars().count(), 3);
    }

    #[test]
    fn shorten_html_closes_what_it_cuts_through() {
        let html = format!("<b>{}</b>", "x".repeat(300));
        let cut = shorten_html(&html, 40);
        assert!(
            cut.chars().count() <= 40,
            "got {} chars",
            cut.chars().count()
        );
        assert!(cut.contains("<b>"));
        assert!(
            cut.contains("</b>"),
            "unbalanced markup would be rejected: {cut}"
        );
    }
}
