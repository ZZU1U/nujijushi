//! Reputation decisions, balances, and durable notifications.

mod publications;
mod suggestions;

use rusqlite::{Connection, OptionalExtension, params};

use crate::config::AccessConfig;
use crate::error::{Error, Result};
use crate::model::PostState;

use super::{posts, users};

pub(crate) use publications::parse_message_ids;
pub use publications::{known_message, record_publication};
pub use suggestions::{
    MatchedSuggestion, Suggestion, match_suggestion, reattach_known, register_new,
};

pub const DEFAULT_REWARD: i64 = 10;
pub const MIN_REWARD: i64 = 5;
pub const MAX_REWARD: i64 = 20;
const CENTS: i64 = 100;
const REJECTION: i64 = -300;

/// Use the bot's access precedence for background work which has no incoming update.
pub fn is_guest(conn: &Connection, access: &AccessConfig, user_id: i64) -> Result<bool> {
    if access.is_admin(user_id) {
        return Ok(false);
    }
    Ok(match users::get(conn, user_id)? {
        Some(user) => !matches!(
            user.role.trim().to_ascii_lowercase().as_str(),
            "admin" | "banned" | "ban"
        ),
        None => access.is_listed_guest(user_id) || access.public,
    })
}

/// Post state, its score changes, and any notifications belong to the caller's transaction.
pub fn review(
    conn: &Connection,
    post_id: i64,
    state: PostState,
    enabled: bool,
    admin: bool,
    points: Option<i64>,
) -> Result<()> {
    if let Some(points) = points
        && (!enabled
            || !admin
            || state != PostState::Approved
            || !(MIN_REWARD..=MAX_REWARD).contains(&points))
    {
        return Err(Error::invalid(
            "use 5–20 whole points when approving a guest suggestion",
        ));
    }
    let tracked = conn
        .query_row(
            "SELECT s.id, s.verdict_points FROM reputation_posts p
         JOIN reputation_subjects s ON s.id = p.subject_id
         WHERE p.post_id = ?1 AND s.reviewable",
            [post_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;
    if let Some((subject_id, old)) = tracked
        && matches!(state, PostState::Approved | PostState::Rejected)
    {
        if enabled && !admin {
            return Err(Error::invalid(
                "only an admin can review a guest suggestion",
            ));
        }
        if enabled {
            let total = match (state, points, old) {
                (PostState::Rejected, _, _) => REJECTION,
                (PostState::Approved, None, Some(previous)) if previous > 0 => previous,
                _ => points.unwrap_or(DEFAULT_REWARD) * CENTS,
            };
            settle(conn, subject_id, old, total)?;
        } else if old.is_none() {
            // A decision made while scoring is off closes an unscored review.
            conn.execute(
                "UPDATE reputation_subjects SET reviewable = 0 WHERE id = ?1",
                [subject_id],
            )?;
        }
    } else if points.is_some() {
        return Err(Error::invalid("no guest suggestion to reward on this post"));
    }
    posts::set_state(conn, post_id, state)
}

pub fn reviewable(conn: &Connection, post_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM reputation_posts p
         JOIN reputation_subjects s ON s.id = p.subject_id WHERE p.post_id = ?1 AND s.reviewable)",
        [post_id],
        |row| row.get(0),
    )?)
}

fn settle(conn: &Connection, subject: i64, previous: Option<i64>, total: i64) -> Result<()> {
    if previous == Some(total) {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT user_id FROM reputation_suggestions WHERE subject_id = ?1 AND review_member ORDER BY id",
    )?;
    let users: Vec<i64> = stmt
        .query_map([subject], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let count = users.len() as i64;
    let reason = match (previous, total.is_positive()) {
        (None, true) => "Your suggestion was approved.",
        (None, false) => "Your suggestion was rejected.",
        (Some(_), true) => "An admin updated the approval reward for your suggestion.",
        (Some(_), false) => "An admin changed the decision to rejected.",
    };
    for (index, user) in users.into_iter().enumerate() {
        let index = index as i64;
        let delta = share(total, count, index) - share(previous.unwrap_or_default(), count, index);
        record_event(conn, user, delta, reason, None)?;
    }
    conn.execute(
        "UPDATE reputation_subjects SET verdict_points = ?1 WHERE id = ?2",
        params![total, subject],
    )?;
    Ok(())
}

fn share(total: i64, count: i64, index: i64) -> i64 {
    total / count + total.signum() * i64::from(index < (total % count).abs())
}

pub fn balance(conn: &Connection, user: i64) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT balance FROM reputation_events WHERE user_id = ?1 ORDER BY id DESC LIMIT 1",
            [user],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

pub fn format_points(cents: i64) -> String {
    let magnitude = cents.unsigned_abs();
    let sign = if cents < 0 { "−" } else { "" };
    let whole = magnitude / 100;
    let remainder = magnitude % 100;
    if remainder == 0 {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{remainder:02}")
    }
}

fn record_event(
    conn: &Connection,
    user: i64,
    delta: i64,
    reason: &str,
    url: Option<&str>,
) -> Result<()> {
    if delta != 0 {
        let total = balance(conn, user)? + delta;
        conn.execute(
            "INSERT INTO reputation_events (user_id, delta, balance, reason, message_url) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![user, delta, total, reason, url],
        )?;
    }
    Ok(())
}

#[derive(Debug)]
pub struct Notification {
    pub id: i64,
    pub user_id: i64,
    pub delta: i64,
    pub balance: i64,
    pub reason: String,
    pub url: Option<String>,
}

pub fn claim_notification(conn: &Connection) -> Result<Option<Notification>> {
    Ok(conn.query_row(
        "UPDATE reputation_events SET notification_claimed = 1
         WHERE id = (SELECT id FROM reputation_events WHERE notification_claimed = 0 ORDER BY id LIMIT 1)
         RETURNING id, user_id, delta, balance, reason, message_url",
        [], |row| Ok(Notification {
            id: row.get(0)?, user_id: row.get(1)?, delta: row.get(2)?, balance: row.get(3)?,
            reason: row.get(4)?, url: row.get(5)?,
        }),
    ).optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::{Db, channels, queue};
    use crate::model::Channel;
    use crate::source::NormalizedPost;

    fn setup() -> (tempfile::TempDir, Db, Config) {
        let home = tempfile::tempdir().unwrap();
        let mut config = Config::load_from_home(home.path().to_path_buf()).unwrap();
        config.guest_reputation = true;
        config.access.public = true;
        let db = Db::open(config.db_path()).unwrap();
        (home, db, config)
    }

    fn media(hash: &str) -> posts::NewMedia {
        posts::NewMedia {
            blob_hash: hash.to_owned(),
            ext: "png".into(),
            mime: None,
            width: Some(16),
            height: Some(16),
            bytes: 100,
            is_video: false,
            poster_hash: None,
            sensitive: false,
            meta_json: None,
        }
    }

    #[test]
    fn coauthors_split_the_verdict_once_and_corrections_use_the_original_cohort() {
        let (_home, db, cfg) = setup();
        db.with(|conn| {
            let original = NormalizedPost::new("pixiv", "10");
            let files = [media("a"), media("b")];
            let first = posts::store_import(conn, &original, &files, Some(90), false, &cfg)?;
            let post_id = first.post_id.unwrap();
            assert!(first.created);
            let mut alias = NormalizedPost::new("danbooru", "20");
            alias.tags.push("replace-me".into());
            let reordered = [media("b"), media("a")];
            let alias = posts::store_import(conn, &alias, &reordered, Some(30), false, &cfg)?;
            assert_eq!(alias.post_id, Some(post_id));
            assert!(!alias.created);
            assert_eq!(alias.suggestion, Some(Suggestion::Pending));
            assert!(posts::get(conn, post_id)?.tags.is_empty());
            for guest in [90, 10] {
                posts::store_import(conn, &original, &files, Some(guest), false, &cfg)?;
            }

            super::super::transaction(conn, |tx| {
                assert!(review(tx, post_id, PostState::Approved, true, false, None).is_err());
                assert!(review(tx, post_id, PostState::Approved, true, true, Some(21)).is_err());
                review(tx, post_id, PostState::Approved, true, true, None)?;
                assert_eq!(balance(tx, 90)?, 334);
                assert_eq!(balance(tx, 30)?, 333);
                assert_eq!(balance(tx, 10)?, 333);
                Ok(())
            })?;

            posts::store_import(conn, &original, &files, Some(55), false, &cfg)?;
            super::super::transaction(conn, |tx| {
                review(tx, post_id, PostState::Approved, true, true, None)?;
                review(tx, post_id, PostState::Rejected, true, true, None)?;
                for guest in [90, 30, 10] {
                    assert_eq!(balance(tx, guest)?, -100);
                }
                assert_eq!(balance(tx, 55)?, 0);
                review(tx, post_id, PostState::Approved, true, true, Some(20))?;
                review(tx, post_id, PostState::Approved, true, true, None)?;
                assert_eq!(balance(tx, 90)?, 667);
                assert_eq!(balance(tx, 30)?, 667);
                assert_eq!(balance(tx, 10)?, 666);
                Ok(())
            })?;
            let mut count = 0;
            while claim_notification(conn)?.is_some() {
                count += 1;
            }
            assert_eq!(count, 9);

            posts::delete(conn, post_id)?;
            let repeat = posts::store_import(conn, &original, &files, Some(90), false, &cfg)?;
            assert_eq!(repeat.suggestion, Some(Suggestion::AlreadyArchived));
            assert!(!repeat.created);
            assert_eq!(balance(conn, 90)?, 667);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn rediscovery_counts_once_while_queued_and_unconfirmed_is_neutral() {
        let (_home, db, cfg) = setup();
        db.with(|conn| {
            let post = NormalizedPost::new("pixiv", "10");
            let files = [media("a")];
            let post_id = posts::store_import(conn, &post, &files, None, false, &cfg)?
                .post_id
                .unwrap();
            let channel = channels::insert(conn, &Channel::new("Test", "-100123"))?;
            let queue_id = queue::enqueue_new(conn, post_id, channel, 0)?.unwrap();
            let found = posts::store_import(conn, &post, &files, Some(90), false, &cfg)?;
            assert_eq!(found.suggestion, Some(Suggestion::Rediscovered));
            assert_eq!(balance(conn, 90)?, 200);
            let found = posts::store_import(conn, &post, &files, Some(80), false, &cfg)?;
            assert_eq!(found.suggestion, Some(Suggestion::AlreadyArchived));
            assert_eq!(balance(conn, 80)?, 0);
            assert_eq!(posts::diagnostics(conn, post_id)?.1, None);

            queue::acquire_for_target(conn, queue_id, "claim", Some("-100123"))?;
            queue::mark_partial(conn, queue_id, "partial", "42", Some(-100123), None)?;
            let direct = known_message(conn, &["-100123".into()], 42, 70)?.unwrap();
            assert_eq!(direct.status, Suggestion::Unconfirmed);
            let found = posts::store_import(conn, &post, &files, Some(71), false, &cfg)?;
            assert_eq!(found.suggestion, Some(Suggestion::Unconfirmed));
            assert_eq!(balance(conn, 70)?, 0);
            assert_eq!(balance(conn, 71)?, 0);

            // Use the target captured at send time even if an operator edited the channel.
            let mut current = channels::get(conn, channel)?;
            current.tg_id = "-100321".into();
            channels::update(conn, &current)?;
            queue::resolve_unknown(conn, queue_id, true, None)?;
            let target = ["-100123".into()];
            let url = Some("https://t.me/c/123/42".to_string());
            for _ in 0..2 {
                let direct = known_message(conn, &target, 42, 70)?.unwrap();
                assert_eq!(direct.status, Suggestion::Published(url.clone()));
            }
            assert_eq!(balance(conn, 70)?, -100);
            posts::delete(conn, post_id)?;
            let direct = known_message(conn, &target, 42, 60)?.unwrap();
            assert_eq!(direct.status, Suggestion::Published(url));
            assert_eq!(direct.post_id, None);
            assert_eq!(balance(conn, 60)?, -100);
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn concurrent_first_submissions_share_one_archive_post() {
        let (_home, db, cfg) = setup();
        let other = Db::open(cfg.db_path()).unwrap();
        let cfg2 = cfg.clone();
        let left = db.call(move |conn| {
            posts::store_import(
                conn,
                &NormalizedPost::new("pixiv", "7"),
                &[media("equal")],
                Some(9),
                false,
                &cfg,
            )
        });
        let right = other.call(move |conn| {
            posts::store_import(
                conn,
                &NormalizedPost::new("danbooru", "8"),
                &[media("equal")],
                Some(8),
                false,
                &cfg2,
            )
        });
        let (left, right) = tokio::join!(left, right);
        let (left, right) = (left.unwrap(), right.unwrap());
        assert_eq!(left.post_id, right.post_id);
        assert_ne!(left.created, right.created);
        db.with(|conn| {
            super::super::transaction(conn, |tx| {
                review(
                    tx,
                    left.post_id.unwrap(),
                    PostState::Rejected,
                    true,
                    true,
                    None,
                )?;
                assert_eq!(balance(tx, 8)?, -150);
                assert_eq!(balance(tx, 9)?, -150);
                Ok(())
            })
        })
        .unwrap();
    }

    #[test]
    fn opt_out_closes_unscored_reviews_without_backfilling() {
        let (_home, db, mut cfg) = setup();
        db.with(|conn| {
            let post = NormalizedPost::new("pixiv", "10");
            let files = [media("a")];
            let id = posts::store_import(conn, &post, &files, Some(90), false, &cfg)?
                .post_id
                .unwrap();
            cfg.guest_reputation = false;
            super::super::transaction(conn, |tx| {
                review(tx, id, PostState::Approved, false, false, None)
            })?;
            cfg.guest_reputation = true;
            super::super::transaction(conn, |tx| {
                review(tx, id, PostState::Approved, true, true, None)
            })?;
            assert_eq!(balance(conn, 90)?, 0);
            assert!(claim_notification(conn)?.is_none());
            Ok(())
        })
        .unwrap();
    }
}
