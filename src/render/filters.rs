//! minijinja filters used by caption templates.

use minijinja::value::Value;
use minijinja::{Environment, Error as MjError, ErrorKind};

/// Filters report failures through minijinja's own error type.
type FilterResult<T> = std::result::Result<T, MjError>;

use crate::error::Result;

pub fn register(env: &mut Environment<'static>) -> Result<()> {
    env.add_filter("truncate_words", truncate_words);
    env.add_filter("hashtags", hashtags);
    env.add_filter("tag_links", tag_links);
    env.add_filter("one_line", one_line);
    Ok(())
}

/// Keep at most `max` characters, cutting on a word boundary and adding an ellipsis.
pub fn truncate_words(value: String, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out = String::with_capacity(max + 1);
    let mut cut_at = 0usize;
    for (count, (index, ch)) in trimmed.char_indices().enumerate() {
        if count >= max {
            break;
        }
        out.push(ch);
        if ch.is_whitespace() || matches!(ch, ',' | '.') {
            cut_at = index + ch.len_utf8();
        }
    }
    if cut_at > 0 {
        out.truncate(cut_at);
    }
    let out = out
        .trim_end()
        .trim_end_matches([',', '.', ';', ':'])
        .trim_end();
    format!("{out}…")
}

/// `#tag #other` (Telegram-safe: tags are already normalized to underscores).
pub fn hashtags(value: Value) -> FilterResult<Value> {
    let tags: Vec<String> = collect_strings(&value)?;
    let rendered = tags
        .iter()
        .filter(|tag| !tag.is_empty())
        .map(|tag| format!("#{}", tag.replace(' ', "_")))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Value::from_safe_string(rendered))
}

/// Plain comma separated tags.
pub fn tag_links(value: Value, base: String) -> FilterResult<Value> {
    let tags = collect_strings(&value)?;
    let rendered = tags
        .iter()
        .map(|tag| {
            format!(
                "<a href=\"{}{}\">{}</a>",
                htmlescape::encode_minimal(&base),
                htmlescape::encode_minimal(tag),
                htmlescape::encode_minimal(tag)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Value::from_safe_string(rendered))
}

pub fn one_line(value: String) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_strings(value: &Value) -> FilterResult<Vec<String>> {
    if value.is_undefined() || value.is_none() {
        return Ok(Vec::new());
    }
    if let Some(text) = value.as_str() {
        return Ok(vec![text.to_string()]);
    }
    if let Ok(iter) = value.try_iter() {
        return Ok(iter
            .filter_map(|item| item.as_str().map(|s| s.to_string()))
            .collect());
    }
    Err(MjError::new(
        ErrorKind::InvalidOperation,
        "expected a list of tags",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_word_boundary() {
        assert_eq!(truncate_words("hello world".into(), 100), "hello world");
        assert_eq!(truncate_words("hello wonderful world".into(), 12), "hello…");
        assert_eq!(truncate_words("abcdefghij".into(), 5), "abcde…");
        assert_eq!(truncate_words("short.".into(), 6), "short.");
        assert_eq!(truncate_words("  padded  ".into(), 20), "padded");
    }

    #[test]
    fn hashtags_are_safe_html() {
        let value = Value::from(vec!["digital_art".to_string(), "nsfw".to_string()]);
        let out = hashtags(value).unwrap();
        assert_eq!(out.as_str(), Some("#digital_art #nsfw"));
    }

    #[test]
    fn hashtags_handles_empty_list() {
        let out = hashtags(Value::from(Vec::<String>::new())).unwrap();
        assert_eq!(out.as_str(), Some(""));
    }

    #[test]
    fn tag_links_escape() {
        let value = Value::from(vec!["a b".to_string()]);
        let out = tag_links(value, "https://x/?t=".into()).unwrap();
        assert_eq!(out.as_str(), Some("<a href=\"https://x/?t=a b\">a b</a>"));
    }

    #[test]
    fn one_line_collapses() {
        assert_eq!(one_line("a\n b\t c".into()), "a b c");
    }
}
