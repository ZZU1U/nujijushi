//! Channel CRUD, aliases, discovery and scheduling bookkeeping.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::error::{Error, Result};
use crate::model::{Attribution, Channel, NsfwPolicy, ParseMode, TagStyle, slugify};

use super::{fmt_ts, get_bool, get_ts, get_ts_required};

const COLUMNS: &str = "
    id, alias, title, tg_id, discovered, enabled, paused, interval_minutes, posts_per_slot,
    active_from, active_to, tz, daily_cap, jitter_secs, artist_cooldown, nsfw_policy, min_media,
    blocked_tags, required_tags, parse_mode, template_name, template, attribution, tag_style,
    max_tags, silent, protect, include_source_tag, last_sent_at, next_due_at,
    consecutive_failures, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Channel> {
    Ok(Channel {
        id: row.get("id")?,
        alias: row.get("alias")?,
        title: row.get("title")?,
        tg_id: row.get("tg_id")?,
        discovered: get_bool(row, "discovered")?,
        enabled: get_bool(row, "enabled")?,
        paused: get_bool(row, "paused")?,
        interval_minutes: row.get("interval_minutes")?,
        posts_per_slot: row.get("posts_per_slot")?,
        active_from: row.get("active_from")?,
        active_to: row.get("active_to")?,
        tz: row.get("tz")?,
        daily_cap: row.get("daily_cap")?,
        jitter_secs: row.get("jitter_secs")?,
        artist_cooldown: row.get("artist_cooldown")?,
        nsfw_policy: NsfwPolicy::parse(&row.get::<_, String>("nsfw_policy")?),
        min_media: row.get("min_media")?,
        blocked_tags: row.get("blocked_tags")?,
        required_tags: row.get("required_tags")?,
        parse_mode: ParseMode::parse(&row.get::<_, String>("parse_mode")?),
        template_name: row.get("template_name")?,
        template: row.get("template")?,
        attribution: Attribution::parse(&row.get::<_, String>("attribution")?),
        tag_style: TagStyle::parse(&row.get::<_, String>("tag_style")?),
        max_tags: row.get("max_tags")?,
        silent: get_bool(row, "silent")?,
        protect: get_bool(row, "protect")?,
        include_source_tag: get_bool(row, "include_source_tag")?,
        last_sent_at: get_ts(row, "last_sent_at")?,
        next_due_at: get_ts(row, "next_due_at")?,
        consecutive_failures: row.get("consecutive_failures")?,
        created_at: get_ts_required(row, "created_at")?,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Channel>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM channels ORDER BY id"))?;
    let rows = stmt.query_map([], from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: i64) -> Result<Channel> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM channels WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("channel {id}")))
}

/// Resolve a channel by alias, `@username`, numeric chat id, database id or title.
pub fn find(conn: &Connection, needle: &str) -> Result<Option<Channel>> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let bare = needle.trim_start_matches('@');
    let lowered = bare.to_ascii_lowercase();
    let numeric = bare.parse::<i64>().ok();
    let with_dash: Option<String> = numeric.filter(|n| *n > 0).map(|n| format!("-{n}"));
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM channels
         WHERE lower(alias) = ?1 OR tg_id = ?2 OR tg_id = ?3 OR id = ?4
            OR lower(title) = ?1 OR tg_id = ?5
         ORDER BY discovered ASC, id ASC LIMIT 1"
    ))?;
    let mut rows = stmt.query_map(params![lowered, bare, with_dash, numeric, needle], from_row)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn by_alias(conn: &Connection, alias: &str) -> Result<Option<Channel>> {
    let alias = alias.trim().trim_start_matches('@').to_ascii_lowercase();
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM channels WHERE lower(alias) = ?1"),
            [alias],
            from_row,
        )
        .optional()?)
}

pub fn by_tg_id(conn: &Connection, tg_id: &str) -> Result<Option<Channel>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM channels WHERE tg_id = ?1"),
            [tg_id.trim()],
            from_row,
        )
        .optional()?)
}

fn alias_taken(conn: &Connection, alias: &str, exclude_id: Option<i64>) -> Result<bool> {
    let taken: i64 = conn.query_row(
        "SELECT COUNT(*) FROM channels WHERE lower(alias) = ?1 AND id IS NOT ?2",
        params![alias.to_ascii_lowercase(), exclude_id],
        |row| row.get(0),
    )?;
    Ok(taken > 0)
}

/// First free variant of `base`: base, base-2, base-3, ...
pub fn unique_alias(conn: &Connection, base: &str, exclude_id: Option<i64>) -> Result<String> {
    let base = slugify(base);
    if !alias_taken(conn, &base, exclude_id)? {
        return Ok(base);
    }
    for suffix in 2..500 {
        let candidate = format!("{base}-{suffix}");
        if !alias_taken(conn, &candidate, exclude_id)? {
            return Ok(candidate);
        }
    }
    Err(Error::other(format!("cannot find a free alias for {base}")))
}

pub fn insert(conn: &Connection, channel: &Channel) -> Result<i64> {
    let alias = unique_alias(conn, &channel.alias, None)?;
    let id: i64 = conn.query_row(
        "INSERT INTO channels (alias, title, tg_id, discovered, enabled, paused, interval_minutes,
            posts_per_slot, active_from, active_to, tz, daily_cap, jitter_secs, artist_cooldown,
            nsfw_policy, min_media, blocked_tags, required_tags, parse_mode, template_name,
            template, attribution, tag_style, max_tags, silent, protect, include_source_tag,
            last_sent_at, next_due_at, consecutive_failures, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
            ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31)
         RETURNING id",
        params![
            alias,
            channel.title,
            channel.tg_id,
            channel.discovered as i64,
            channel.enabled as i64,
            channel.paused as i64,
            channel.interval_minutes,
            channel.posts_per_slot,
            channel.active_from,
            channel.active_to,
            channel.tz,
            channel.daily_cap,
            channel.jitter_secs,
            channel.artist_cooldown,
            channel.nsfw_policy.as_str(),
            channel.min_media,
            channel.blocked_tags,
            channel.required_tags,
            channel.parse_mode.as_str(),
            channel.template_name,
            channel.template,
            channel.attribution.as_str(),
            channel.tag_style.as_str(),
            channel.max_tags,
            channel.silent as i64,
            channel.protect as i64,
            channel.include_source_tag as i64,
            channel.last_sent_at.map(fmt_ts),
            channel.next_due_at.map(fmt_ts),
            channel.consecutive_failures,
            fmt_ts(channel.created_at),
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn update(conn: &Connection, channel: &Channel) -> Result<()> {
    let alias = unique_alias(conn, &channel.alias, Some(channel.id))?;
    let changed = conn.execute(
        "UPDATE channels SET alias = ?1, title = ?2, tg_id = ?3, enabled = ?4, paused = ?5,
            interval_minutes = ?6, posts_per_slot = ?7, active_from = ?8, active_to = ?9, tz = ?10,
            daily_cap = ?11, jitter_secs = ?12, artist_cooldown = ?13, nsfw_policy = ?14,
            min_media = ?15, blocked_tags = ?16, required_tags = ?17, parse_mode = ?18,
            template_name = ?19, template = ?20, attribution = ?21, tag_style = ?22,
            max_tags = ?23, silent = ?24, protect = ?25, include_source_tag = ?26
         WHERE id = ?27",
        params![
            alias,
            channel.title,
            channel.tg_id,
            channel.enabled as i64,
            channel.paused as i64,
            channel.interval_minutes,
            channel.posts_per_slot,
            channel.active_from,
            channel.active_to,
            channel.tz,
            channel.daily_cap,
            channel.jitter_secs,
            channel.artist_cooldown,
            channel.nsfw_policy.as_str(),
            channel.min_media,
            channel.blocked_tags,
            channel.required_tags,
            channel.parse_mode.as_str(),
            channel.template_name,
            channel.template,
            channel.attribution.as_str(),
            channel.tag_style.as_str(),
            channel.max_tags,
            channel.silent as i64,
            channel.protect as i64,
            channel.include_source_tag as i64,
            channel.id,
        ],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(format!("channel {}", channel.id)));
    }
    Ok(())
}

/// Register (or refresh) a channel the bot was just added to. Returns (id, created).
pub fn upsert_discovered(
    conn: &Connection,
    tg_id: &str,
    title: &str,
    username: Option<&str>,
) -> Result<(i64, bool)> {
    if let Some(existing) = by_tg_id(conn, tg_id)? {
        conn.execute(
            "UPDATE channels SET discovered = 1,
                title = CASE WHEN title = '' THEN ?1 ELSE title END
             WHERE id = ?2",
            params![title, existing.id],
        )?;
        return Ok((existing.id, false));
    }
    let base = username.map(slugify).unwrap_or_else(|| slugify(title));
    let alias = unique_alias(conn, &base, None)?;
    let mut channel = Channel::new(title, tg_id);
    channel.alias = alias;
    channel.discovered = true;
    // stays out of the scheduler until an admin enables it
    channel.enabled = false;
    let id = insert(conn, &channel)?;
    Ok((id, true))
}

pub fn set_discovered(conn: &Connection, id: i64, discovered: bool) -> Result<()> {
    conn.execute(
        "UPDATE channels SET discovered = ?1 WHERE id = ?2",
        params![discovered as i64, id],
    )?;
    Ok(())
}

pub fn list_discovered(conn: &Connection) -> Result<Vec<Channel>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM channels WHERE discovered = 1 ORDER BY enabled ASC, id ASC"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM channels WHERE id = ?1", [id])? > 0)
}

pub fn set_paused(conn: &Connection, id: i64, paused: bool) -> Result<()> {
    conn.execute(
        "UPDATE channels SET paused = ?1 WHERE id = ?2",
        params![paused as i64, id],
    )?;
    Ok(())
}

/// Manual resume after a review: clear the failure streak so the circuit breaker
/// starts with a fresh budget instead of re-pausing on the next single failure.
pub fn resume(conn: &Connection, id: i64) -> Result<()> {
    let changed = conn.execute(
        "UPDATE channels SET paused = 0, consecutive_failures = 0 WHERE id = ?1",
        [id],
    )?;
    if changed == 0 {
        return Err(Error::NotFound(format!("channel {id}")));
    }
    Ok(())
}

pub fn set_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE channels SET enabled = ?1 WHERE id = ?2",
        params![enabled as i64, id],
    )?;
    Ok(())
}

pub fn set_next_due(conn: &Connection, id: i64, next_due: Option<DateTime<Utc>>) -> Result<()> {
    conn.execute(
        "UPDATE channels SET next_due_at = ?1 WHERE id = ?2",
        params![next_due.map(fmt_ts), id],
    )?;
    Ok(())
}

/// Successful publish: reset the failure counter, stamp last/next.
pub fn record_success(
    conn: &Connection,
    id: i64,
    sent_at: DateTime<Utc>,
    next_due: DateTime<Utc>,
) -> Result<()> {
    conn.execute(
        "UPDATE channels SET last_sent_at = ?1, next_due_at = ?2, consecutive_failures = 0
         WHERE id = ?3",
        params![fmt_ts(sent_at), fmt_ts(next_due), id],
    )?;
    Ok(())
}

/// Failed publish. Auto-pauses after `pause_after` consecutive failures.
pub fn record_failure(conn: &Connection, id: i64, pause_after: i64) -> Result<i64> {
    conn.execute(
        "UPDATE channels SET consecutive_failures = consecutive_failures + 1 WHERE id = ?1",
        [id],
    )?;
    let failures: i64 = conn.query_row(
        "SELECT consecutive_failures FROM channels WHERE id = ?1",
        [id],
        |row| row.get(0),
    )?;
    if failures >= pause_after {
        conn.execute(
            "UPDATE channels SET paused = 1 WHERE id = ?1 AND paused = 0",
            [id],
        )?;
    }
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> crate::db::Db {
        crate::db::Db::open_in_memory().unwrap()
    }

    #[test]
    fn aliases_are_unique_and_resolvable() {
        let db = db();
        let mut first = Channel::new("Night Shift", "@night_shift");
        first.alias = "night".into();
        let first_id = db.with(|conn| insert(conn, &first)).unwrap();

        let mut second = Channel::new("Other", "-1009988776655");
        second.alias = "night".into(); // collides, must get a suffix
        let second_id = db.with(|conn| insert(conn, &second)).unwrap();

        let (a, b) = db
            .with(|conn| Ok((get(conn, first_id)?, get(conn, second_id)?)))
            .unwrap();
        assert_eq!(a.alias, "night");
        assert_eq!(b.alias, "night-2");

        // every input form resolves: alias, case, @username, numeric id
        for needle in ["night", "NIGHT", "@night_shift", &first_id.to_string()] {
            let found = db.with(|conn| find(conn, needle)).unwrap();
            assert!(found.is_some(), "{needle} should resolve");
        }
        let numeric = db
            .with(|conn| find(conn, "-1009988776655"))
            .unwrap()
            .expect("numeric chat id resolves");
        assert_eq!(numeric.id, second_id);
        let by_alias = db.with(|conn| by_alias(conn, "@night")).unwrap();
        assert_eq!(by_alias.map(|c| c.id), Some(first_id));
    }

    #[test]
    fn backfilled_aliases_exist() {
        let db = db();
        let channel = Channel::new("Legacy", "@legacy");
        let id = db.with(|conn| insert(conn, &channel)).unwrap();
        let stored = db.with(|conn| get(conn, id)).unwrap();
        assert!(!stored.alias.is_empty());
        assert_eq!(stored.alias, "legacy");
        assert!(!stored.discovered);
    }

    #[test]
    fn discovery_registers_disabled_and_is_idempotent() {
        let db = db();
        let (id, created) = db
            .with(|conn| upsert_discovered(conn, "-1001234567890", "Night Art", Some("night_art")))
            .unwrap();
        assert!(created);
        let channel = db.with(|conn| get(conn, id)).unwrap();
        assert_eq!(channel.alias, "night_art");
        assert!(channel.discovered);
        assert!(
            !channel.enabled,
            "discovered channels stay out of the scheduler"
        );

        let (same_id, created_again) = db
            .with(|conn| upsert_discovered(conn, "-1001234567890", "Night Art", Some("night_art")))
            .unwrap();
        assert_eq!(same_id, id);
        assert!(!created_again);
        assert_eq!(db.with(|conn| list(conn)).unwrap().len(), 1);

        db.with(|conn| set_enabled(conn, id, true)).unwrap();
        assert!(db.with(|conn| get(conn, id)).unwrap().enabled);
    }

    #[test]
    fn update_keeps_alias_stable_and_rejects_duplicates() {
        let db = db();
        let mut a = Channel::new("A", "@a_channel");
        a.alias = "alpha".into();
        let a_id = db.with(|conn| insert(conn, &a)).unwrap();
        let mut b = Channel::new("B", "@b_channel");
        b.alias = "beta".into();
        let b_id = db.with(|conn| insert(conn, &b)).unwrap();

        // saving b with a's alias must not steal it
        let mut b_stored = db.with(|conn| get(conn, b_id)).unwrap();
        b_stored.alias = "alpha".into();
        db.with(|conn| update(conn, &b_stored)).unwrap();
        let b_after = db.with(|conn| get(conn, b_id)).unwrap();
        assert_eq!(b_after.alias, "alpha-2");
        assert_eq!(db.with(|conn| get(conn, a_id)).unwrap().alias, "alpha");

        // keeping its own alias is fine
        let mut a_stored = db.with(|conn| get(conn, a_id)).unwrap();
        a_stored.title = "A renamed".into();
        db.with(|conn| update(conn, &a_stored)).unwrap();
        assert_eq!(db.with(|conn| get(conn, a_id)).unwrap().alias, "alpha");
    }

    #[test]
    fn manual_resume_gives_the_circuit_breaker_a_fresh_budget() {
        let db = db();
        let id = db
            .with(|conn| insert(conn, &Channel::new("Test", "@test")))
            .unwrap();
        db.with(|conn| {
            for _ in 0..3 {
                record_failure(conn, id, 3)?;
            }
            Ok(())
        })
        .unwrap();
        let paused = db.with(|conn| get(conn, id)).unwrap();
        assert!(paused.paused);
        assert_eq!(paused.consecutive_failures, 3);

        db.with(|conn| resume(conn, id)).unwrap();
        let resumed = db.with(|conn| get(conn, id)).unwrap();
        assert!(!resumed.paused);
        assert_eq!(resumed.consecutive_failures, 0);

        // One new failure after a manual review must not re-pause immediately.
        db.with(|conn| record_failure(conn, id, 3)).unwrap();
        assert!(!db.with(|conn| get(conn, id)).unwrap().paused);
    }
}
