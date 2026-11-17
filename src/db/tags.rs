//! Tag normalization, aliases and content rules.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;
use crate::model::TagRuleAction;

/// Telegram-hashtag-safe canonical form: lowercase, trimmed, spaces to `_`.
pub fn normalize(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .trim_start_matches('#')
        .to_lowercase()
        .chars()
        .map(|c| match c {
            ' ' | '-' | '\t' | '\n' => '_',
            c if c.is_alphanumeric()
                || c == '_'
                || c == '('
                || c == ')'
                || c == '!'
                || c == '?'
                || c == '\''
                || c == '.'
                || c == ':' =>
            {
                c
            }
            _ => '_',
        })
        .collect();

    let squeezed: String =
        cleaned
            .chars()
            .fold(String::with_capacity(cleaned.len()), |mut acc, c| {
                if c == '_' && acc.ends_with('_') {
                    return acc;
                }
                acc.push(c);
                acc
            });

    let trimmed = squeezed.trim_matches('_');
    if trimmed.is_empty() || trimmed.len() > 48 || trimmed.chars().all(|c| c == '_') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Normalize a list: drop invalid, dedupe, preserve order.
pub fn normalize_all<I, S>(raw: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for item in raw {
        if let Some(tag) = normalize(item.as_ref())
            && !out.contains(&tag)
        {
            out.push(tag);
        }
    }
    out
}

pub fn load_aliases(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT from_tag, to_tag FROM tag_aliases")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (from, to) = row?;
        map.insert(from, to);
    }
    Ok(map)
}

/// Apply aliases (one level, then re-normalize the target).
pub fn apply_aliases(tags: Vec<String>, aliases: &HashMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let mapped = aliases
            .get(&tag)
            .and_then(|to| normalize(to))
            .unwrap_or(tag);
        if !out.contains(&mapped) {
            out.push(mapped);
        }
    }
    out
}

pub fn set_alias(conn: &Connection, from: &str, to: &str) -> Result<()> {
    let from = normalize(from).unwrap_or_else(|| from.trim().to_lowercase());
    let to = normalize(to).unwrap_or_else(|| to.trim().to_lowercase());
    conn.execute(
        "INSERT INTO tag_aliases (from_tag, to_tag) VALUES (?1, ?2)
         ON CONFLICT (from_tag) DO UPDATE SET to_tag = excluded.to_tag",
        rusqlite::params![from, to],
    )?;
    Ok(())
}

pub fn set_rule(conn: &Connection, tag: &str, action: TagRuleAction) -> Result<()> {
    let tag = normalize(tag).unwrap_or_else(|| tag.trim().to_lowercase());
    conn.execute(
        "INSERT INTO tag_rules (tag, action) VALUES (?1, ?2)
         ON CONFLICT (tag) DO UPDATE SET action = excluded.action",
        rusqlite::params![tag, action.as_str()],
    )?;
    Ok(())
}

pub fn remove_rule(conn: &Connection, tag: &str) -> Result<bool> {
    let tag = normalize(tag).unwrap_or_else(|| tag.trim().to_lowercase());
    Ok(conn.execute("DELETE FROM tag_rules WHERE tag = ?1", [tag])? > 0)
}

pub struct TagRules {
    pub block: Vec<String>,
    pub nsfw: Vec<String>,
}

impl TagRules {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare("SELECT tag, action FROM tag_rules")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut block = Vec::new();
        let mut nsfw = Vec::new();
        for row in rows {
            let (tag, action) = row?;
            match TagRuleAction::parse(&action) {
                TagRuleAction::Block => block.push(tag),
                TagRuleAction::Nsfw => nsfw.push(tag),
            }
        }
        Ok(Self { block, nsfw })
    }

    pub fn hits_nsfw(&self, tags: &[String]) -> bool {
        tags.iter().any(|t| self.nsfw.iter().any(|r| r == t))
    }

    pub fn blocked_hit<'a>(&self, tags: &'a [String]) -> Option<&'a String> {
        tags.iter().find(|t| self.block.iter().any(|r| r == *t))
    }
}

fn upsert_tag(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO tags (name, display) VALUES (?1, ?1) ON CONFLICT (name) DO NOTHING",
        [name],
    )?;
    let id = conn
        .query_row("SELECT id FROM tags WHERE name = ?1", [name], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or_else(|| crate::error::Error::other(format!("tag row missing: {name}")))?;
    Ok(id)
}

pub fn link(conn: &Connection, post_id: i64, tag: &str, origin: &str) -> Result<()> {
    let Some(name) = normalize(tag) else {
        return Ok(());
    };
    let tag_id = upsert_tag(conn, &name)?;
    conn.execute(
        "INSERT INTO post_tags (post_id, tag_id, origin) VALUES (?1, ?2, ?3)
         ON CONFLICT (post_id, tag_id) DO NOTHING",
        rusqlite::params![post_id, tag_id, origin],
    )?;
    Ok(())
}

pub fn unlink(conn: &Connection, post_id: i64, tag: &str) -> Result<bool> {
    let Some(name) = normalize(tag) else {
        return Ok(false);
    };
    let removed = conn.execute(
        "DELETE FROM post_tags WHERE post_id = ?1
         AND tag_id IN (SELECT id FROM tags WHERE name = ?2)",
        rusqlite::params![post_id, name],
    )?;
    Ok(removed > 0)
}

pub fn for_post(conn: &Connection, post_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.name FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
         WHERE pt.post_id = ?1 ORDER BY pt.rowid",
    )?;
    let rows = stmt.query_map([post_id], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn most_used(conn: &Connection, limit: i64) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT t.name, COUNT(*) AS n FROM post_tags pt JOIN tags t ON t.id = pt.tag_id
         GROUP BY t.id ORDER BY n DESC, t.name LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_to_hashtag_safe() {
        assert_eq!(normalize("#Digital Art").as_deref(), Some("digital_art"));
        assert_eq!(normalize("  Blue   Sky ").as_deref(), Some("blue_sky"));
        assert_eq!(normalize("loli"), Some("loli".to_string()));
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("#"), None);
        assert_eq!(normalize(&"x".repeat(60)), None);
        assert_eq!(normalize("a/b"), Some("a_b".to_string()));
    }

    #[test]
    fn dedupes_and_drops_bad() {
        let tags = normalize_all(["Art", "#art", "", "  ", "two words"]);
        assert_eq!(tags, vec!["art", "two_words"]);
    }

    #[test]
    fn alias_mapping() {
        let mut aliases = HashMap::new();
        aliases.insert("artwork".to_string(), "art".to_string());
        aliases.insert("twt".to_string(), "twitter".to_string());
        let mapped = apply_aliases(vec!["artwork".into(), "art".into(), "twt".into()], &aliases);
        assert_eq!(mapped, vec!["art", "twitter"]);
    }
}
