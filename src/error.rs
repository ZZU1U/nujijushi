//! Error type shared by the whole crate.

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("config error: {0}")]
    Config(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("template error: {0}")]
    Template(#[from] minijinja::Error),

    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("telegram error: {0}")]
    Telegram(String),

    /// Part of an operation completed and must not be retried blindly
    /// (for example, some messages of a backup album were already sent).
    #[error("partial delivery: {0}")]
    Partial(String),

    #[error("{cmd} failed{}: {stderr}", match .code { Some(c) => format!(" (exit {c})"), None => String::new() })]
    Process {
        cmd: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("timed out: {0}")]
    Timeout(String),

    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    pub fn config(msg: impl Into<String>) -> Self {
        Error::Config(msg.into())
    }
}
