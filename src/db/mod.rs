//! SQLite access: one connection behind a mutex, sync helpers plus an async wrapper.

pub mod channels;
pub mod jobs;
pub mod migrate;
pub mod mirrors;
pub mod posts;
pub mod queue;
pub mod tags;
pub mod usage;
pub mod users;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
    path: Arc<PathBuf>,
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        Self::configure(&conn)?;
        migrate::migrate(&mut conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            path: Arc::new(path),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        migrate::migrate(&mut conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            path: Arc::new(PathBuf::from(":memory:")),
        })
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    /// Synchronous access. Never held across an await point.
    pub fn with<T>(&self, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| Error::other("db mutex poisoned"))?;
        f(&mut guard)
    }

    /// Blocking access from async code (runs on the blocking pool).
    pub async fn call<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .lock()
                .map_err(|_| Error::other("db mutex poisoned"))?;
            f(&mut guard)
        })
        .await?
    }

    pub fn schema_version(&self) -> Result<i64> {
        self.with(|conn| migrate::schema_version(conn))
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.with(|conn| {
            Ok(conn
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    row.get(0)
                })
                .ok())
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
    }
}

// --- shared row helpers (timestamps are stored as RFC3339 TEXT) ---

use chrono::{DateTime, Utc};
use rusqlite::Row;

pub fn fmt_ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

pub fn fmt_ts_opt(dt: Option<DateTime<Utc>>) -> Option<String> {
    dt.map(fmt_ts)
}

pub fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

pub fn get_ts(row: &Row<'_>, col: &str) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let raw: Option<String> = row.get(col)?;
    Ok(raw.as_deref().and_then(parse_ts))
}

pub fn get_ts_required(row: &Row<'_>, col: &str) -> rusqlite::Result<DateTime<Utc>> {
    let raw: String = row.get(col)?;
    parse_ts(&raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!(
                "bad timestamp in {col}: {raw}"
            ))),
        )
    })
}

pub fn get_bool(row: &Row<'_>, col: &str) -> rusqlite::Result<bool> {
    let raw: Option<i64> = row.get(col)?;
    Ok(raw.unwrap_or(0) != 0)
}
