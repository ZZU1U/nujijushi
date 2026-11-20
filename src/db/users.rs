//! Bot users and their roles (config admins always win at resolution time).

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::AccessConfig;
use crate::error::Result;

use super::fmt_ts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub tg_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    /// admin | guest | banned
    pub role: String,
    pub created_at: String,
    pub last_seen_at: Option<String>,
}

impl User {
    pub fn label(&self) -> String {
        match (&self.username, &self.first_name) {
            (Some(username), _) => format!("@{username}"),
            (None, Some(name)) => name.clone(),
            _ => self.tg_id.to_string(),
        }
    }
}

const COLUMNS: &str = "tg_id, username, first_name, role, created_at, last_seen_at";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        tg_id: row.get("tg_id")?,
        username: row.get("username")?,
        first_name: row.get("first_name")?,
        role: row.get("role")?,
        created_at: row.get("created_at")?,
        last_seen_at: row.get("last_seen_at")?,
    })
}

pub fn get(conn: &Connection, tg_id: i64) -> Result<Option<User>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM users WHERE tg_id = ?1"),
            [tg_id],
            from_row,
        )
        .optional()?)
}

pub(super) fn is_admin(conn: &Connection, access: &AccessConfig, tg_id: i64) -> Result<bool> {
    Ok(access.is_admin(tg_id)
        || get(conn, tg_id)?.is_some_and(|user| user.role.trim().eq_ignore_ascii_case("admin")))
}

/// Record a sighting without changing the role.
pub fn touch(
    conn: &Connection,
    tg_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
) -> Result<()> {
    let now = fmt_ts(Utc::now());
    conn.execute(
        "INSERT INTO users (tg_id, username, first_name, role, created_at, last_seen_at)
         VALUES (?1, ?2, ?3, 'guest', ?4, ?4)
         ON CONFLICT (tg_id) DO UPDATE SET
            username = COALESCE(excluded.username, users.username),
            first_name = COALESCE(excluded.first_name, users.first_name),
            last_seen_at = excluded.last_seen_at",
        params![tg_id, username, first_name, now],
    )?;
    Ok(())
}

pub fn set_role(conn: &Connection, tg_id: i64, role: &str) -> Result<()> {
    let now = fmt_ts(Utc::now());
    conn.execute(
        "INSERT INTO users (tg_id, role, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT (tg_id) DO UPDATE SET role = excluded.role",
        params![tg_id, role, now],
    )?;
    Ok(())
}

pub fn list(conn: &Connection, limit: i64) -> Result<Vec<User>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM users ORDER BY role, last_seen_at DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map([limit], from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_creates_guest_and_keeps_role() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.with(|conn| touch(conn, 7, Some("someone"), Some("Some")))
            .unwrap();
        let user = db.with(|conn| get(conn, 7)).unwrap().unwrap();
        assert_eq!(user.role, "guest");
        assert_eq!(user.label(), "@someone");

        db.with(|conn| set_role(conn, 7, "admin")).unwrap();
        db.with(|conn| touch(conn, 7, Some("renamed"), None))
            .unwrap();
        let user = db.with(|conn| get(conn, 7)).unwrap().unwrap();
        assert_eq!(user.role, "admin", "a sighting must not demote anyone");
        assert_eq!(user.username.as_deref(), Some("renamed"));
        assert!(user.last_seen_at.is_some());
    }

    #[test]
    fn set_role_upserts_unknown_users() {
        let db = crate::db::Db::open_in_memory().unwrap();
        db.with(|conn| set_role(conn, 99, "banned")).unwrap();
        assert_eq!(
            db.with(|conn| get(conn, 99)).unwrap().unwrap().role,
            "banned"
        );
        assert_eq!(db.with(|conn| count(conn)).unwrap(), 1);
    }
}
