//! Candidates and atomic delivery claims for queue notifications.

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

const EMPTY: &str = "empty_alert_handled = 0 AND enabled = 1 AND paused = 0
    AND NOT EXISTS (SELECT 1 FROM queue q WHERE q.channel_id = channels.id
                    AND q.state IN ('queued', 'sending'))";
const UNCONFIRMED: &str = "state = 'unknown' AND unconfirmed_alert_handled = 0";

#[derive(Debug, Clone, Copy)]
pub enum Candidate {
    Empty(i64),
    Unconfirmed(i64),
}

#[derive(Debug)]
pub struct Alert {
    pub channel_alias: String,
    pub unconfirmed: Option<(i64, i64)>,
}

pub fn pending(conn: &Connection) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT 0, id FROM queue WHERE {UNCONFIRMED}
         UNION ALL SELECT 1, id FROM channels WHERE {EMPTY}
         ORDER BY 1, 2"
    ))?;
    let rows = stmt.query_map([], |row| {
        let id = row.get(1)?;
        Ok(if row.get::<_, i64>(0)? == 0 {
            Candidate::Unconfirmed(id)
        } else {
            Candidate::Empty(id)
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Recheck eligibility while recording the claim. Another process may have
/// handled the alert, or an operator may have resolved it since discovery.
pub fn claim(conn: &Connection, candidate: Candidate) -> Result<Option<Alert>> {
    let alert = match candidate {
        Candidate::Empty(id) => conn.query_row(
            &format!(
                "UPDATE channels SET empty_alert_handled = 1
                 WHERE id = ?1 AND {EMPTY} RETURNING alias"
            ),
            [id],
            |row| Ok(Alert { channel_alias: row.get(0)?, unconfirmed: None }),
        ),
        Candidate::Unconfirmed(id) => conn.query_row(
            &format!(
                "UPDATE queue SET unconfirmed_alert_handled = 1
                 WHERE id = ?1 AND {UNCONFIRMED}
                 RETURNING (SELECT alias FROM channels WHERE channels.id = queue.channel_id), post_id"
            ),
            [id],
            |row| Ok(Alert { channel_alias: row.get(0)?, unconfirmed: Some((id, row.get(1)?)) }),
        ),
    };
    Ok(alert.optional()?)
}
