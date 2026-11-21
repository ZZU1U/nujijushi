//! Preview and reclaim storage from confirmed sent posts.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::blob::{BlobStore, Garbage, GcReport};
use crate::db::cleanup::{self as db, Candidate};
use crate::error::{Error, Result};
use crate::state::AppState;

pub const DEFAULT_AGE_DAYS: u32 = 30;

#[derive(Deserialize, Serialize)]
pub struct Confirmation {
    pub cutoff: DateTime<Utc>,
    pub revision: String,
}

#[derive(Serialize)]
pub struct Preview {
    pub confirmation: Confirmation,
    pub posts: usize,
    pub suggestions: i64,
    pub files: usize,
    pub estimated_bytes: u64,
}

impl Preview {
    pub fn describe(&self) -> String {
        format!(
            "{} eligible post(s) · {} suggestion(s) · {} estimated reclaimable",
            self.posts,
            self.suggestions,
            crate::model::format_bytes(self.estimated_bytes),
        )
    }
}

pub struct Report {
    pub preview: Preview,
    pub files: GcReport,
}

impl Report {
    pub fn describe(&self) -> String {
        format!(
            "{} post(s) cleaned · {} suggestion(s) removed · {} freed ({} blob(s), {} thumbnail(s)) · {} file(s) to retry",
            self.preview.posts,
            self.preview.suggestions,
            crate::model::format_bytes(self.files.freed_bytes),
            self.files.removed_blobs,
            self.files.removed_thumbs,
            self.files.failed_files,
        )
    }
}

pub fn cutoff(older_than_days: u32) -> Result<DateTime<Utc>> {
    Utc::now()
        .checked_sub_signed(Duration::days(i64::from(older_than_days)))
        .ok_or_else(|| Error::invalid("This cleanup age is out of range"))
}

fn plan(
    conn: &Connection,
    store: &BlobStore,
    cutoff: DateTime<Utc>,
) -> Result<(Preview, Vec<Candidate>, Garbage)> {
    let posts = db::candidates(conn, cutoff)?;
    let compacted: Vec<_> = posts
        .iter()
        .filter(|post| post.has_edits)
        .map(|post| post.id)
        .collect();
    let garbage = store.garbage(conn, &compacted)?;
    let estimated_bytes = garbage.estimated_bytes();
    let revision = blake3::hash(&serde_json::to_vec(&(
        cutoff,
        &posts,
        &garbage,
        estimated_bytes,
    ))?)
    .to_hex()
    .to_string();
    let preview = Preview {
        confirmation: Confirmation { cutoff, revision },
        posts: posts.len(),
        suggestions: posts.iter().map(|post| post.suggestions).sum(),
        files: garbage.file_count(),
        estimated_bytes,
    };
    Ok((preview, posts, garbage))
}

pub async fn preview(state: &AppState, days: u32) -> Result<Preview> {
    let cutoff = cutoff(days)?;
    let store = state.blobs.clone();
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let (preview, _, _) = plan(&tx, &store, cutoff)?;
            Ok(preview)
        })
        .await
}

pub async fn run(
    state: &AppState,
    cutoff: DateTime<Utc>,
    revision: Option<String>,
    job: Option<i64>,
) -> Result<Report> {
    let lock = state.storage_cleanup().await?;
    let store = state.blobs.clone();
    state
        .db
        .call(move |conn| {
            let _lock = lock;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if let Some(id) = job {
                crate::db::jobs::label_running(&tx, id, "checking cleanup preview")?;
            }
            let (preview, candidates, garbage) = plan(&tx, &store, cutoff)?;
            if revision.is_some_and(|expected| preview.confirmation.revision != expected) {
                return Err(Error::invalid(
                    "The archive changed. Preview cleanup again.",
                ));
            }
            db::apply(&tx, &candidates)?;
            if let Some(id) = job {
                crate::db::jobs::label_running(&tx, id, "reclaiming archive files")?;
            }
            tx.commit()?;
            let files = store.gc_matching(conn, Some(&garbage))?;
            Ok(Report { preview, files })
        })
        .await
}

pub async fn collect_orphans(state: &AppState) -> Result<GcReport> {
    let lock = state.storage_cleanup().await?;
    let store = state.blobs.clone();
    state
        .db
        .call(move |conn| {
            let _lock = lock;
            store.gc(conn)
        })
        .await
}
