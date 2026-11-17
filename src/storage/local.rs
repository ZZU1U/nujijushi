//! Local folder sink: any mounted or synced directory (Dropbox, iCloud Drive,
//! an external disk, …).

use std::path::PathBuf;

use async_trait::async_trait;

use super::{MirrorPost, Sink};
use crate::error::{Error, Result};

pub struct LocalSink {
    root: PathBuf,
    base: String,
}

impl LocalSink {
    pub fn new(root: PathBuf, base: String) -> Self {
        Self { root, base }
    }

    fn target(&self, relative: &str) -> PathBuf {
        if self.base.is_empty() {
            self.root.join(relative)
        } else {
            self.root.join(&self.base).join(relative)
        }
    }

    fn external(&self, dir: &str) -> String {
        if self.base.is_empty() {
            dir.to_string()
        } else {
            format!("{}/{}", self.base, dir)
        }
    }
}

#[async_trait]
impl Sink for LocalSink {
    fn name(&self) -> &'static str {
        "local"
    }

    fn describe(&self) -> String {
        format!("copy to {}", self.root.display())
    }

    async fn put(&self, item: &MirrorPost) -> Result<String> {
        for file in &item.files {
            let dest = self.target(&file.relative);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::copy(&file.path, &dest).await.map_err(|err| {
                Error::other(format!(
                    "copy {} -> {}: {err}",
                    file.path.display(),
                    dest.display()
                ))
            })?;
        }
        let meta_path = self.target(&item.meta_name());
        if let Some(parent) = meta_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&meta_path, &item.meta).await?;
        Ok(self.external(&item.dir))
    }

    async fn check(&self) -> Result<String> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|err| Error::other(format!("cannot create {}: {err}", self.root.display())))?;
        let probe = self.root.join(".nujijushi-write-test");
        tokio::fs::write(&probe, b"ok").await.map_err(|err| {
            Error::other(format!("cannot write to {}: {err}", self.root.display()))
        })?;
        let _ = tokio::fs::remove_file(&probe).await;
        Ok(format!("{} is writable", self.root.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests_support::*;

    #[tokio::test]
    async fn mirrors_files_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LocalSink::new(dir.path().to_path_buf(), "nujijushi".into());
        let item = sample_mirror(&dir);

        let external = sink.put(&item).await.unwrap();
        assert_eq!(external, "nujijushi/pixiv/nightjar/123456");

        let base = dir.path().join("nujijushi/pixiv/nightjar/123456");
        assert!(base.join("00.png").exists());
        assert!(base.join("01.mp4").exists());
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(base.join("post.json")).unwrap())
                .unwrap();
        assert_eq!(meta["source_id"], "123456");
        // the copied media is byte-identical to the blob
        assert_eq!(
            std::fs::read(base.join("00.png")).unwrap(),
            std::fs::read(&item.files[0].path).unwrap()
        );
    }

    #[tokio::test]
    async fn overwrites_without_complaining() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LocalSink::new(dir.path().to_path_buf(), String::new());
        let item = sample_mirror(&dir);
        sink.put(&item).await.unwrap();
        sink.put(&item).await.unwrap();
        assert!(dir.path().join("pixiv/nightjar/123456/00.png").exists());
    }

    #[tokio::test]
    async fn check_reports_a_writable_folder() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LocalSink::new(dir.path().join("nested"), String::new());
        let report = sink.check().await.unwrap();
        assert!(report.contains("writable"));
        assert!(dir.path().join("nested").is_dir());
    }

    #[tokio::test]
    async fn check_fails_on_an_unwritable_path() {
        let sink = LocalSink::new(PathBuf::from("/proc/nope/nope"), String::new());
        assert!(sink.check().await.is_err());
    }
}
