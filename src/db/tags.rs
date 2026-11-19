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

fn link(conn: &Connection, post_id: i64, name: &str, origin: &str) -> Result<()> {
    let tag_id = upsert_tag(conn, name)?;
    conn.execute(
        "INSERT INTO post_tags (post_id, tag_id, origin) VALUES (?1, ?2, ?3)
         ON CONFLICT (post_id, tag_id, origin) DO NOTHING",
        rusqlite::params![post_id, tag_id, origin],
    )?;
    Ok(())
}

pub(super) fn refresh_source(conn: &Connection, post_id: i64, tags: &[String]) -> Result<()> {
    let names = apply_aliases(normalize_all(tags), &load_aliases(conn)?);
    conn.execute(
        "DELETE FROM post_tags WHERE post_id = ?1 AND origin = 'source'",
        [post_id],
    )?;
    for name in &names {
        link(conn, post_id, name, "source")?;
    }
    Ok(())
}

pub fn add_manual(conn: &Connection, post_id: i64, raw: &[String]) -> Result<()> {
    let aliases = load_aliases(conn)?;
    for name in apply_aliases(normalize_all(raw), &aliases) {
        link(conn, post_id, &name, "manual")?;
    }
    Ok(())
}

pub fn replace_manual(conn: &mut Connection, post_id: i64, raw: &[String]) -> Result<()> {
    super::transaction(conn, |conn| replace_manual_in(conn, post_id, raw))
}

pub(crate) fn replace_manual_in(conn: &Connection, post_id: i64, raw: &[String]) -> Result<()> {
    let existing = provenance(conn, post_id)?;
    let mut aliases = load_aliases(conn)?;
    // The form includes stored names. Remap only names newly entered by the operator.
    for tag in existing.iter().filter(|tag| tag.manual) {
        aliases.remove(&tag.name);
    }
    let names = apply_aliases(normalize_all(raw), &aliases);
    for tag in existing.iter().filter(|tag| tag.manual) {
        if !names.contains(&tag.name) {
            conn.execute(
                "DELETE FROM post_tags WHERE post_id = ?1 AND tag_id = ?2 AND origin = 'manual'",
                rusqlite::params![post_id, tag.id],
            )?;
        }
    }
    for name in &names {
        link(conn, post_id, name, "manual")?;
    }
    Ok(())
}

pub fn suppress_source(conn: &Connection, post_id: i64, tag_id: i64) -> Result<bool> {
    Ok(conn.execute(
        "INSERT INTO post_tag_removals (post_id, tag_id)
         SELECT post_id, tag_id FROM post_tags
         WHERE post_id = ?1 AND tag_id = ?2 AND origin = 'source'
         ON CONFLICT (post_id, tag_id) DO NOTHING",
        rusqlite::params![post_id, tag_id],
    )? > 0)
}

pub fn restore_source(conn: &Connection, post_id: i64, tag_id: i64) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM post_tag_removals WHERE post_id = ?1 AND tag_id = ?2",
        rusqlite::params![post_id, tag_id],
    )? > 0)
}

pub fn remove(conn: &Connection, post_id: i64, raw: &[String]) -> Result<()> {
    let aliases = load_aliases(conn)?;
    let active = provenance(conn, post_id)?;
    for name in normalize_all(raw) {
        let canonical = aliases.get(&name).and_then(|to| normalize(to));
        let tag = active.iter().find(|tag| tag.name == name).or_else(|| {
            let canonical = canonical.as_deref()?;
            active.iter().find(|tag| tag.name == canonical)
        });
        if let Some(tag) = tag {
            suppress_source(conn, post_id, tag.id)?;
            conn.execute(
                "DELETE FROM post_tags WHERE post_id = ?1 AND tag_id = ?2 AND origin = 'manual'",
                rusqlite::params![post_id, tag.id],
            )?;
        }
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct TagProvenance {
    pub id: i64,
    pub name: String,
    pub source: bool,
    pub manual: bool,
}

pub fn provenance(conn: &Connection, post_id: i64) -> Result<Vec<TagProvenance>> {
    let mut stmt = conn.prepare(
        "SELECT pt.tag_id, t.name, pt.source, pt.manual
         FROM active_post_tags pt JOIN tags t ON t.id = pt.tag_id
         WHERE pt.post_id = ?1 ORDER BY pt.position",
    )?;
    let rows = stmt.query_map([post_id], |row| {
        Ok(TagProvenance {
            id: row.get(0)?,
            name: row.get(1)?,
            source: row.get(2)?,
            manual: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Debug, serde::Serialize)]
pub struct TagRemoval {
    pub id: i64,
    pub name: String,
}

pub fn removals(conn: &Connection, post_id: i64) -> Result<Vec<TagRemoval>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name FROM post_tag_removals r JOIN tags t ON t.id = r.tag_id
         WHERE r.post_id = ?1 ORDER BY t.name",
    )?;
    let rows = stmt.query_map([post_id], |row| {
        Ok(TagRemoval {
            id: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn for_post(conn: &Connection, post_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.name FROM active_post_tags pt JOIN tags t ON t.id = pt.tag_id
         WHERE pt.post_id = ?1 ORDER BY pt.position",
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
        "SELECT t.name, COUNT(*) AS n FROM active_post_tags pt JOIN tags t ON t.id = pt.tag_id
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

    #[test]
    fn source_removal_survives_refresh_and_is_scoped_to_one_post() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.with(|conn| {
            set_alias(conn, "#Fine Art", "illustration")?;
            set_alias(conn, "illustration", "art")?;
            let mut first = crate::source::NormalizedPost::new("pixiv", "42");
            first.tags = vec!["Fine Art".into(), "original".into()];
            let (post_id, _) = super::super::posts::upsert(conn, &first, &[])?;
            let mut second = first.clone();
            second.source_id = "43".into();
            let (other_id, _) = super::super::posts::upsert(conn, &second, &[])?;

            add_manual(conn, post_id, &["Fine Art".into(), "favorite".into()])?;
            let tags = provenance(conn, post_id)?;
            assert_eq!(tags.len(), 3);
            assert_eq!(tags[0].name, "illustration");
            assert!(tags[0].source && tags[0].manual);
            assert!(most_used(conn, 10)?.contains(&("illustration".into(), 2)));
            let tag_id = tags[0].id;

            // Saving the existing manual field does not apply a second alias hop.
            replace_manual(conn, post_id, &["illustration".into(), "favorite".into()])?;
            assert!(provenance(conn, post_id)?[0].manual);
            remove(conn, post_id, &["#fine art".into()])?;
            first.tags.push("new tag".into());
            assert_eq!(
                super::super::posts::upsert(conn, &first, &[])?,
                (post_id, false)
            );
            assert_eq!(
                for_post(conn, post_id)?,
                ["favorite", "original", "new_tag"]
            );
            assert_eq!(for_post(conn, other_id)?, ["illustration", "original"]);
            assert_eq!(removals(conn, post_id)?[0].name, "illustration");
            assert!(most_used(conn, 10)?.contains(&("illustration".into(), 1)));
            let filter = super::super::posts::PostFilter {
                tag: Some("illustration".into()),
                ..Default::default()
            };
            assert_eq!(super::super::posts::count_cards(conn, &filter)?, 1);

            // Undo restores a current source tag, but never resurrects one the source has dropped.
            assert!(restore_source(conn, post_id, tag_id)?);
            assert!(for_post(conn, post_id)?.contains(&"illustration".into()));
            assert!(suppress_source(conn, post_id, tag_id)?);
            first.tags.remove(0);
            super::super::posts::upsert(conn, &first, &[])?;
            assert_eq!(removals(conn, post_id)?.len(), 1);
            assert!(restore_source(conn, post_id, tag_id)?);
            assert!(!for_post(conn, post_id)?.contains(&"illustration".into()));
            first.tags.push("Fine Art".into());
            super::super::posts::upsert(conn, &first, &[])?;
            assert!(for_post(conn, post_id)?.contains(&"illustration".into()));
            Ok(())
        })
        .unwrap();
    }
}
