//! Prometheus text exposition (`# HELP` / `# TYPE` plus samples) for the admin
//! `/metrics` endpoint.
//!
//! Read-only: every family is gathered from a typed `crate::db::*` helper, and
//! rendering is a pure function of the gathered snapshot. Samples are sorted by
//! label, so two scrapes of an unchanged archive are byte-identical.
//!
//! The one family without a `db::` helper is `nujijushi_posts_nsfw`; its query
//! satisfies "no nsfw rule of its own" by borrowing the rule from
//! [`crate::model::Rating::nsfw_sql`], exactly like [`crate::db::posts`] does.

use rusqlite::Connection;

use crate::error::Result;
use crate::model::{PostState, Rating};
use crate::state::AppState;

/// Counts gathered in one blocking hop, before anything is formatted.
#[derive(Debug, Default)]
struct Snapshot {
    /// `(post state, count)`
    posts: Vec<(String, i64)>,
    posts_total: i64,
    /// `(yes|no, count)`
    nsfw: Vec<(String, i64)>,
    /// `(yes|no, count)`
    channels: Vec<(String, i64)>,
    /// `(channel alias, queue state, count)`
    queue: Vec<(String, String, i64)>,
    /// `(job state, count)`
    jobs: Vec<(String, i64)>,
    /// `(mirror state, count)`, summed over sinks
    mirrors: Vec<(String, i64)>,
}

/// Render the whole exposition document (always ends with a newline).
///
/// The `# HELP`/`# TYPE` headers are emitted unconditionally, even for a family
/// with no samples yet, so a dashboard can tell "zero" from "not exported".
pub async fn render(state: &AppState) -> Result<String> {
    let snapshot = state.db.call(gather).await?;
    Ok(expose(&snapshot))
}

/// Runs on the blocking pool: every DB read of one scrape, no formatting.
fn gather(conn: &mut Connection) -> Result<Snapshot> {
    let mut snapshot = Snapshot::default();

    for state in PostState::all() {
        let count = crate::db::posts::count(conn, Some(*state))?;
        snapshot.posts.push((state.as_str().to_string(), count));
    }
    snapshot.posts_total = crate::db::posts::count(conn, None)?;

    // Same rule as `Rating::is_nsfw` / `posts::filter_sql`, so the metric can
    // never disagree with what the publisher considers nsfw.
    let sql = format!(
        "SELECT COALESCE(p.nsfw_override, CASE WHEN {} THEN 1 ELSE 0 END) AS nsfw, COUNT(*)
           FROM posts p GROUP BY nsfw",
        Rating::nsfw_sql("p.rating")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
    // both variants are always exported, so "0 nsfw" and "no data" differ
    snapshot.nsfw = vec![("no".to_string(), 0), ("yes".to_string(), 0)];
    for row in rows {
        let (nsfw, count) = row?;
        bump(
            &mut snapshot.nsfw,
            if nsfw != 0 { "yes" } else { "no" },
            count,
        );
    }

    let channels = crate::db::channels::list(conn)?;
    snapshot.channels = vec![("no".to_string(), 0), ("yes".to_string(), 0)];
    for channel in &channels {
        bump(
            &mut snapshot.channels,
            if channel.enabled { "yes" } else { "no" },
            1,
        );
    }

    for channel in &channels {
        let counts = crate::db::queue::counts(conn, channel.id)?;
        // every state gets a sample, zero included: a missing series reads as
        // "no data" in Grafana, an explicit 0 reads as "nothing queued"
        for state in crate::model::QueueState::all() {
            let count = counts
                .iter()
                .find(|(name, _)| name == state.as_str())
                .map(|(_, count)| *count)
                .unwrap_or(0);
            snapshot
                .queue
                .push((channel.alias.clone(), state.as_str().to_string(), count));
        }
    }

    for job in crate::db::jobs::list(conn, 1000)? {
        bump(&mut snapshot.jobs, &job.state, 1);
    }

    // `summary` groups by (sink, state); the metric is per state only, so the
    // per-sink rows are summed. `nujijushi_storage_mirrors` is therefore one
    // sample per state regardless of how many sinks are configured.
    for (_sink, state, count) in crate::db::mirrors::summary(conn)? {
        bump(&mut snapshot.mirrors, &state, count);
    }

    snapshot.posts.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot.nsfw.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot.channels.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot
        .queue
        .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    snapshot.jobs.sort_by(|a, b| a.0.cmp(&b.0));
    snapshot.mirrors.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(snapshot)
}

/// The exposition document for a snapshot.
fn expose(snapshot: &Snapshot) -> String {
    let mut out = String::with_capacity(1024);

    family(
        &mut out,
        "nujijushi_info",
        "Build information about the running nujijushi binary.",
    );
    sample(
        &mut out,
        "nujijushi_info",
        &[("version", env!("CARGO_PKG_VERSION"))],
        1,
    );

    family(&mut out, "nujijushi_posts", "Posts per curator state.");
    for (state, count) in &snapshot.posts {
        sample(&mut out, "nujijushi_posts", &[("state", state)], *count);
    }

    family(
        &mut out,
        "nujijushi_posts_total",
        "Posts in the archive, any state.",
    );
    sample(&mut out, "nujijushi_posts_total", &[], snapshot.posts_total);

    family(
        &mut out,
        "nujijushi_posts_nsfw",
        "Posts by effective nsfw flag (manual override, else the rating rule).",
    );
    for (nsfw, count) in &snapshot.nsfw {
        sample(&mut out, "nujijushi_posts_nsfw", &[("nsfw", nsfw)], *count);
    }

    family(
        &mut out,
        "nujijushi_channels",
        "Channels by the enabled flag.",
    );
    for (enabled, count) in &snapshot.channels {
        sample(
            &mut out,
            "nujijushi_channels",
            &[("enabled", enabled)],
            *count,
        );
    }

    family(
        &mut out,
        "nujijushi_queue",
        "Queue entries per channel and state.",
    );
    for (channel, state, count) in &snapshot.queue {
        sample(
            &mut out,
            "nujijushi_queue",
            &[("channel", channel), ("state", state)],
            *count,
        );
    }

    family(
        &mut out,
        "nujijushi_jobs",
        "Import jobs by state (the newest 1000 rows).",
    );
    for (state, count) in &snapshot.jobs {
        sample(&mut out, "nujijushi_jobs", &[("state", state)], *count);
    }

    family(
        &mut out,
        "nujijushi_storage_mirrors",
        "Mirror rows by state, summed over sinks.",
    );
    for (state, count) in &snapshot.mirrors {
        sample(
            &mut out,
            "nujijushi_storage_mirrors",
            &[("state", state)],
            *count,
        );
    }

    out
}

/// Add `by` to the sample carrying `label`, creating it at zero first.
fn bump(samples: &mut Vec<(String, i64)>, label: &str, by: i64) {
    match samples.iter_mut().find(|(existing, _)| existing == label) {
        Some((_, count)) => *count += by,
        None => samples.push((label.to_string(), by)),
    }
}

/// `# HELP` + `# TYPE` for one gauge family.
fn family(out: &mut String, name: &str, help: &str) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push_str("\n# TYPE ");
    out.push_str(name);
    out.push_str(" gauge\n");
}

/// One sample line: `name{k="v",...} value`.
fn sample(out: &mut String, name: &str, labels: &[(&str, &str)], value: i64) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (index, (key, value)) in labels.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(key);
            out.push_str("=\"");
            out.push_str(&escape_label(value));
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

/// Escape a label value the way the exposition format requires.
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::Channel;
    use crate::source::NormalizedPost;

    async fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        // keep the temp dir alive for the whole test process
        std::mem::forget(dir);
        AppState::open(cfg, None).unwrap()
    }

    /// The shared shape check: every non-comment, non-empty line is a sample
    /// (`^[a-zA-Z_:][a-zA-Z0-9_:]*(\{[^}]*\})? -?[0-9.eE+-]+$`) — a hand-written
    /// matcher for that pattern, because the crate has no regex dependency.
    fn assert_exposition(document: &str) {
        assert!(
            document.ends_with('\n'),
            "the document must end with a newline"
        );
        let mut samples = 0;
        for line in document.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            assert!(is_sample_line(line), "not a sample line: {line:?}");
            samples += 1;
        }
        assert!(samples > 0, "the document has no samples:\n{document}");
    }

    fn is_sample_line(line: &str) -> bool {
        fn name_start(ch: char) -> bool {
            ch.is_ascii_alphabetic() || ch == '_' || ch == ':'
        }
        fn name_char(ch: char) -> bool {
            ch.is_ascii_alphanumeric() || ch == '_' || ch == ':'
        }
        fn value_char(ch: char) -> bool {
            ch.is_ascii_digit() || matches!(ch, '.' | 'e' | 'E' | '+' | '-')
        }

        let bytes = line.as_bytes();
        let mut index = 0;
        match bytes.first() {
            Some(&first) if name_start(first as char) => index += 1,
            _ => return false,
        }
        while index < bytes.len() && name_char(bytes[index] as char) {
            index += 1;
        }
        if bytes.get(index) == Some(&b'{') {
            index += 1;
            while index < bytes.len() && bytes[index] != b'}' {
                index += 1;
            }
            if bytes.get(index) != Some(&b'}') {
                return false;
            }
            index += 1;
        }
        if bytes.get(index) != Some(&b' ') {
            return false;
        }
        index += 1;
        !bytes[index..].is_empty() && bytes[index..].iter().all(|b| value_char(*b as char))
    }

    #[tokio::test]
    async fn empty_database_still_exports_every_family() {
        let state = test_state().await;
        let document = render(&state).await.unwrap();

        assert_exposition(&document);
        assert!(
            document.contains(&format!(
                "nujijushi_info{{version=\"{}\"}} 1",
                env!("CARGO_PKG_VERSION")
            )),
            "{document}"
        );
        assert!(document.contains("nujijushi_posts_total 0\n"), "{document}");
        // every family is announced even when it has no rows yet
        for family in [
            "nujijushi_info",
            "nujijushi_posts",
            "nujijushi_posts_total",
            "nujijushi_posts_nsfw",
            "nujijushi_channels",
            "nujijushi_queue",
            "nujijushi_jobs",
            "nujijushi_storage_mirrors",
        ] {
            assert!(
                document.contains(&format!("# TYPE {family} gauge")),
                "{family} is missing from:\n{document}"
            );
        }
        assert!(
            document.contains("nujijushi_posts_nsfw{nsfw=\"no\"} 0"),
            "{document}"
        );
        assert!(
            document.contains("nujijushi_channels{enabled=\"yes\"} 0"),
            "{document}"
        );
    }

    #[tokio::test]
    async fn a_queued_post_shows_up_per_channel_and_as_sfw() {
        let state = test_state().await;

        let channel_id = state
            .db
            .call(|conn| {
                let mut channel = Channel::new("Night Shift", "@night_shift");
                channel.alias = "night".into();
                crate::db::channels::insert(conn, &channel)
            })
            .await
            .unwrap();

        let post_id = state
            .db
            .call(|conn| {
                let post = NormalizedPost::new("pixiv", "42");
                let (id, _) = crate::db::posts::upsert(conn, &post, &[])?;
                Ok(id)
            })
            .await
            .unwrap();

        state
            .db
            .call(move |conn| crate::db::queue::enqueue(conn, post_id, channel_id, 0))
            .await
            .unwrap();

        let document = render(&state).await.unwrap();

        assert_exposition(&document);
        assert!(
            document.contains("nujijushi_queue{channel=\"night\",state=\"queued\"} 1"),
            "{document}"
        );
        assert!(
            document.contains("nujijushi_posts_nsfw{nsfw=\"no\"} 1"),
            "{document}"
        );
        assert!(
            document.contains("nujijushi_posts_nsfw{nsfw=\"yes\"} 0"),
            "{document}"
        );
        assert!(
            document.contains("nujijushi_channels{enabled=\"yes\"} 1"),
            "{document}"
        );
        assert!(document.contains("nujijushi_posts_total 1\n"), "{document}");
    }

    #[tokio::test]
    async fn two_scrapes_of_the_same_archive_are_identical() {
        let state = test_state().await;
        state
            .db
            .call(|conn| {
                // inserted in reverse alphabetical order on purpose
                for alias in ["beta", "alpha"] {
                    let mut channel = Channel::new(alias, format!("@{alias}"));
                    channel.alias = alias.into();
                    crate::db::channels::insert(conn, &channel)?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let first = render(&state).await.unwrap();
        let second = render(&state).await.unwrap();
        assert_eq!(first, second);

        // samples are sorted by label, not by insertion order
        let alpha = first.find("channel=\"alpha\"").expect("alpha sample");
        let beta = first.find("channel=\"beta\"").expect("beta sample");
        assert!(alpha < beta, "{first}");
    }

    #[test]
    fn escape_label_quotes_backslashes_and_newlines() {
        assert_eq!(escape_label("plain-1.x"), "plain-1.x");
        assert_eq!(escape_label("a\"b"), "a\\\"b");
        assert_eq!(escape_label("a\\b"), "a\\\\b");
        assert_eq!(escape_label("line\nbreak"), "line\\nbreak");
        assert_eq!(escape_label("\"\\\n"), "\\\"\\\\\\n");
    }
}
