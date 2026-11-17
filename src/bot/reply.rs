//! Telegram reply formatting: escaping, chunking and sending.

use crate::error::{Error, Result};
use crate::render::truncate::close_open_tags;
use crate::tg::Transport;

/// Telegram's message limit.
pub const MAX_CHARS: usize = 4096;

pub fn esc(text: &str) -> String {
    htmlescape::encode_minimal(text)
}

/// Split a reply into Telegram-sized chunks on line boundaries.
///
/// A single overlong line is cut at a character boundary and any tag left open
/// by the cut is closed, so no chunk is invalid HTML.
pub fn chunk(text: &str, max: usize) -> Vec<String> {
    let max = max.max(16);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        let line_len = line.chars().count();
        if line_len > max {
            if !current.trim().is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.extend(hard_split(line, max));
            continue;
        }
        let projected = current.chars().count() + line_len + 1;
        if !current.is_empty() && projected > max {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

fn hard_split(line: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while rest.chars().count() > max {
        let head: String = rest.chars().take(max).collect();
        let consumed = head.len();
        out.push(close_open_tags(&head));
        rest = &rest[consumed..];
    }
    if !rest.trim().is_empty() {
        out.push(close_open_tags(rest));
    }
    out
}

pub async fn send(transport: &dyn Transport, chat: &str, html: &str) -> Result<()> {
    if html.trim().is_empty() {
        return Ok(());
    }
    for part in chunk(html, MAX_CHARS) {
        transport
            .send_text(chat, &part)
            .await
            .map_err(|err| Error::Telegram(err.to_string()))?;
    }
    Ok(())
}

/// `#12` mention for a post.
pub fn post_ref(id: i64) -> String {
    format!("#{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_user_text() {
        assert_eq!(esc("<b> & \"x\""), "&lt;b&gt; &amp; &quot;x&quot;");
    }

    #[test]
    fn chunks_on_line_boundaries() {
        let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        let text = lines.join("\n");
        let chunks = chunk(&text, 30);
        assert!(chunks.len() >= 3);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 30, "chunk too long: {chunk:?}");
        }
        let rejoined = chunks.join("\n");
        assert_eq!(rejoined.lines().count(), 10);
    }

    #[test]
    fn short_text_is_one_chunk() {
        let chunks = chunk("hello", MAX_CHARS);
        assert_eq!(chunks, vec!["hello".to_string()]);
        assert!(chunk("   ", MAX_CHARS).is_empty());
    }

    #[test]
    fn hard_split_closes_tags() {
        let line = format!("<b>{}</b>", "x".repeat(200));
        let chunks = chunk(&line, 100);
        assert!(chunks.len() >= 2);
        // every chunk that opens <b> must close it before the next chunk starts
        assert!(chunks[0].ends_with("</b>"));
        assert_eq!(
            chunks
                .iter()
                .map(|c| c.matches("<b>").count())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            chunks
                .iter()
                .map(|c| c.matches("</b>").count())
                .sum::<usize>(),
            2
        );
    }
}
