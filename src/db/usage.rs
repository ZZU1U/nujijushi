//! Per-user daily quota counters.

use chrono::Utc;
use rusqlite::{Connection, params};

use crate::error::Result;

pub fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

/// Count one action for `tg_id` today. `false` when the quota is already used up.
pub fn bump(conn: &Connection, tg_id: i64, kind: &str, limit: u32) -> Result<bool> {
    let day = today();
    let current: i64 = conn
        .query_row(
            "SELECT count FROM usage WHERE tg_id = ?1 AND day = ?2 AND kind = ?3",
            params![tg_id, day, kind],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if limit > 0 && current as u32 >= limit {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO usage (tg_id, day, kind, count) VALUES (?1, ?2, ?3, 1)
         ON CONFLICT (tg_id, day, kind) DO UPDATE SET count = count + 1",
        params![tg_id, day, kind],
    )?;
    Ok(true)
}

pub fn count(conn: &Connection, tg_id: i64, kind: &str) -> Result<u32> {
    let day = today();
    let current: i64 = conn
        .query_row(
            "SELECT count FROM usage WHERE tg_id = ?1 AND day = ?2 AND kind = ?3",
            params![tg_id, day, kind],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(current as u32)
}

/// Give one unit back when the action it paid for could not start.
pub fn refund(conn: &Connection, tg_id: i64, kind: &str) -> Result<()> {
    let day = today();
    conn.execute(
        "UPDATE usage SET count = MAX(count - 1, 0)
         WHERE tg_id = ?1 AND day = ?2 AND kind = ?3",
        params![tg_id, day, kind],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_counts_up_and_stops() {
        let db = crate::db::Db::open_in_memory().unwrap();
        assert!(db.with(|conn| bump(conn, 1, "import", 2)).unwrap());
        assert!(db.with(|conn| bump(conn, 1, "import", 2)).unwrap());
        assert!(!db.with(|conn| bump(conn, 1, "import", 2)).unwrap());
        assert_eq!(db.with(|conn| count(conn, 1, "import")).unwrap(), 2);
        // other users and other kinds are independent
        assert!(db.with(|conn| bump(conn, 2, "import", 2)).unwrap());
        assert!(db.with(|conn| bump(conn, 1, "queue", 2)).unwrap());
        // limit 0 means unlimited
        for _ in 0..5 {
            assert!(db.with(|conn| bump(conn, 3, "import", 0)).unwrap());
        }
        // a refund hands exactly one unit back
        db.with(|conn| refund(conn, 1, "import")).unwrap();
        assert_eq!(db.with(|conn| count(conn, 1, "import")).unwrap(), 1);
        db.with(|conn| refund(conn, 1, "import")).unwrap();
        db.with(|conn| refund(conn, 1, "import")).unwrap();
        assert_eq!(
            db.with(|conn| count(conn, 1, "import")).unwrap(),
            0,
            "refunds never go below zero"
        );
    }
}
