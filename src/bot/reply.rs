//! Telegram reply formatting: escaping, chunking and sending.

use crate::error::{Error, Result};
use crate::render::truncate::{close_open_tags, take_units, telegram_len};
use crate::tg::Transport;

/// Telegram's message limit, in UTF-16 code units.
pub const MAX_CHARS: usize = 4096;

pub fn esc(text: &str) -> String {
    htmlescape::encode_minimal(text)
}

/// Split a reply into Telegram-sized chunks on line boundaries.
///
/// A single overlong line is cut at a character boundary and any tag left open
/// by the cut is closed, so no chunk is invalid HTML and no chunk exceeds the
/// UTF-16 limit Telegram actually enforces.
pub fn chunk(text: &str, max: usize) -> Vec<String> {
    let max = max.max(16);
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        let line_len = telegram_len(line);
        if line_len > max {
            if !current.trim().is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.extend(hard_split(line, max));
            continue;
        }
        let projected = telegram_len(&current) + line_len + 1;
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
    while telegram_len(rest) > max {
        // Closing the tags we cut through costs units, so shrink the prefix
        // until the repaired chunk fits the limit.
        let mut budget = max.saturating_sub(1);
        let (consumed, part) = loop {
            let head = take_units(rest, budget);
            if head.is_empty() {
                // Pathological markup: stop splitting. `send_text` clips the rest.
                break (0, None);
            }
            let consumed = head.len();
            let part = close_open_tags(&head);
            let overflow = telegram_len(&part).saturating_sub(max);
            if overflow == 0 || budget == 0 {
                break (consumed, Some(part));
            }
            budget = budget.saturating_sub(overflow.max(1));
        };
        let Some(part) = part else { break };
        if !part.is_empty() {
            out.push(part);
        }
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

    #[test]
    fn chunks_fit_the_utf16_limit_even_with_closers() {
        let emoji = "🎨".repeat(3000); // 6000 units
        let chunks = chunk(&emoji, MAX_CHARS);
        assert!(chunks.len() >= 2);
        assert!(
            chunks.iter().all(|c| c.encode_utf16().count() <= MAX_CHARS),
            "an emoji counts as two units"
        );

        // A single markup-heavy line must leave room for the closing tags.
        let line = format!("<b>{}</b>", "x".repeat(5000));
        let chunks = chunk(&line, 100);
        assert!(!chunks.is_empty());
        assert!(
            chunks.iter().all(|c| c.encode_utf16().count() <= 100),
            "got {chunks:?}"
        );
        assert!(chunks[0].ends_with("</b>"));
    }
}
