use std::path::{Path, PathBuf};

use reqwest::header::{CONTENT_TYPE, REFERER};
use tokio::io::AsyncWriteExt;

use crate::config::Config;
use crate::error::{Error, Result};

/// Discard incomplete downloads, including when the enclosing job times out.
pub(super) struct StagingDir {
    path: PathBuf,
    complete: bool,
}

impl StagingDir {
    pub(super) fn new(cfg: &Config, prefix: &str) -> Result<Self> {
        let parent = cfg.staging_dir();
        std::fs::create_dir_all(&parent)?;
        let path = parent.join(format!("{prefix}{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self {
            path,
            complete: false,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn finish(mut self) {
        self.complete = true;
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.complete {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

pub(super) async fn download(
    http: &reqwest::Client,
    url: url::Url,
    referer: &str,
    path: &Path,
    label: &str,
    content_prefix: &str,
) -> Result<()> {
    let failure = || Error::other(format!("{label} download failed"));
    let mut response = http
        .get(url)
        .header(REFERER, referer)
        .send()
        .await
        .map_err(|_| failure())?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::other(format!(
            "{label} download failed (HTTP {})",
            response.status()
        )));
    }
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            !value.starts_with(content_prefix) && value != "application/octet-stream"
        })
    {
        return Err(Error::other(format!(
            "{label} has an unexpected content type"
        )));
    }
    let mut file = tokio::fs::File::create(path).await?;
    let mut wrote = false;
    while let Some(chunk) = response.chunk().await.map_err(|_| failure())? {
        file.write_all(&chunk).await?;
        wrote |= !chunk.is_empty();
    }
    file.flush().await?;
    if !wrote {
        return Err(Error::other(format!("{label} is empty")));
    }
    Ok(())
}
