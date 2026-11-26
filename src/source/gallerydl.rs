//! Import via the external `gallery-dl` helper (Pixiv, Twitter/X, Danbooru, ...).

use std::path::{Path, PathBuf};
use std::process::Output;

use async_trait::async_trait;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::source::pair::{self, Group};
use crate::source::{NormalizedPost, PostFile, Source, fanbox, normalize};

pub(super) const STAGING_PREFIX: &str = "gdl-";

pub struct GalleryDlSource {
    cfg: Config,
}

impl GalleryDlSource {
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Cookie file for a URL: `cookies/<site>.txt` in the nujijushi home.
    pub fn cookie_file(&self, url: &str) -> Option<PathBuf> {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))?;
        let site = cookie_site(&host)?;
        let dir = self.cfg.cookies_dir();
        for candidate in [format!("{site}.txt"), format!("{host}.txt")] {
            let path = dir.join(candidate);
            if path.exists() {
                return Some(path);
            }
        }
        None
    }
}

/// Map a host onto the cookie file base name users actually export.
pub fn cookie_site(host: &str) -> Option<&'static str> {
    let host = host.to_ascii_lowercase();
    let site = if host.ends_with("pixiv.net") || host.ends_with("pximg.net") {
        "pixiv"
    } else if host == "fanbox.cc" || host.ends_with(".fanbox.cc") {
        "fanbox"
    } else if host.ends_with("twitter.com") || host.ends_with("x.com") {
        "twitter"
    } else if host.ends_with("danbooru.donmai.us") {
        "danbooru"
    } else if host.ends_with("gelbooru.com") {
        "gelbooru"
    } else if host.ends_with("artstation.com") {
        "artstation"
    } else if host.ends_with("deviantart.com") {
        "deviantart"
    } else if host.ends_with("bsky.app") {
        "bluesky"
    } else if host.ends_with("tumblr.com") {
        "tumblr"
    } else {
        return None;
    };
    Some(site)
}

pub async fn run_with_timeout(program: &str, args: &[String], timeout_secs: u64) -> Result<Output> {
    let mut command = tokio::process::Command::new(program);
    command.args(args).kill_on_drop(true);
    let child = command.output();
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => Err(Error::Process {
            cmd: program.to_string(),
            code: None,
            stderr: format!("`{program}` not found on PATH (brew install gallery-dl)"),
        }),
        Ok(Err(err)) => Err(Error::Io(err)),
        Err(_) => Err(Error::Timeout(format!(
            "{program} did not finish within {timeout_secs}s"
        ))),
    }
}

/// Command line for one import. Extracted so the proxy/cookie wiring is testable.
pub fn download_args(
    staging: &Path,
    target: &str,
    cookies: Option<&Path>,
    proxy: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-d".into(),
        staging.display().to_string(),
        "--write-metadata".into(),
        "--no-mtime".into(),
    ];
    args.extend(network_args(cookies, proxy));
    args.push(target.to_string());
    args
}

pub(super) fn network_args(cookies: Option<&Path>, proxy: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--retries".into(),
        "3".into(),
        "-o".into(),
        "timeout=30.0".into(),
    ];
    if let Some(cookies) = cookies {
        args.push("--cookies".into());
        args.push(cookies.display().to_string());
    }
    if let Some(proxy) = proxy {
        args.push("--proxy".into());
        args.push(proxy.to_string());
    }
    args
}

fn stderr_tail(output: &Output, lines: usize) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let collected: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    collected
        .iter()
        .skip(collected.len().saturating_sub(lines))
        .copied()
        .collect::<Vec<_>>()
        .join(" | ")
}

fn groups_to_posts(groups: Vec<Group>, url: &str) -> Vec<NormalizedPost> {
    let fallback_source = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .map(|h| normalize::normalize_source_name(&h))
        .unwrap_or_else(|| "unknown".to_string());

    let mut posts = Vec::new();
    for group in groups {
        let files: Vec<PostFile> = group.files.clone();
        match group.meta.as_ref() {
            Some(meta) => {
                if let Some(post) =
                    normalize::from_gallery_metadata(meta, files, &fallback_source, Some(url))
                {
                    posts.push(post);
                }
            }
            None => {
                // no metadata sidecar: keep the media, flag it as untagged
                let mut post = NormalizedPost::new(fallback_source.clone(), group.key.clone());
                post.url = Some(url.to_string());
                post.files = files;
                posts.push(post);
            }
        }
    }
    posts
}

/// Anything with a scheme gallery-dl can fetch. A free function so the "is this
/// importable?" answer is shared instead of re-derived per caller.
pub fn looks_like_url(target: &str) -> bool {
    let lowered = target.trim().to_ascii_lowercase();
    lowered.starts_with("http://") || lowered.starts_with("https://")
}

#[async_trait]
impl Source for GalleryDlSource {
    fn kind(&self) -> &'static str {
        "gallery-dl"
    }

    fn supports(&self, target: &str) -> bool {
        looks_like_url(target)
    }

    async fn fetch(&self, target: &str) -> Result<Vec<NormalizedPost>> {
        self.fetch_with_policy(target, false, None).await
    }
}

impl GalleryDlSource {
    pub async fn fetch_complete(&self, target: &str) -> Result<Vec<NormalizedPost>> {
        self.fetch_with_policy(target, true, None).await
    }

    pub(super) async fn fetch_with_cookie(
        &self,
        target: &str,
        cookie: &Path,
    ) -> Result<Vec<NormalizedPost>> {
        self.fetch_with_policy(target, true, Some(cookie)).await
    }

    async fn fetch_with_policy(
        &self,
        target: &str,
        require_complete: bool,
        cookie: Option<&Path>,
    ) -> Result<Vec<NormalizedPost>> {
        if !self.supports(target) {
            return Err(Error::invalid(format!("not a URL: {target}")));
        }
        std::fs::create_dir_all(self.cfg.staging_dir())?;
        let staging = self
            .cfg
            .staging_dir()
            .join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&staging)?;

        let automatic_cookie = self.cookie_file(target);
        let mut args = download_args(
            &staging,
            target,
            cookie.or(automatic_cookie.as_deref()),
            self.cfg.proxy_url.as_deref(),
        );
        let watched_fanbox = require_complete
            && url::Url::parse(target)
                .ok()
                .and_then(|url| url.host_str().and_then(cookie_site))
                == Some("fanbox");
        if require_complete {
            args.insert(0, "--no-input".into());
            // A user-configured gallery-dl archive must not mark files as handled before the app imports them.
            args.splice(0..0, ["-o".into(), "extractor.archive=null".into()]);
            if watched_fanbox {
                args.splice(0..0, ["-o".into(), fanbox::DISABLE_EMBEDS.into()]);
            }
        }

        tracing::info!(program = %self.cfg.gallery_dl, "running import");
        let output =
            run_with_timeout(&self.cfg.gallery_dl, &args, self.cfg.import_timeout_secs).await;

        if require_complete {
            let failure = match &output {
                Ok(out) if out.status.success() => None,
                Ok(out) => Some(format!("gallery-dl download exited with {}", out.status)),
                Err(err) => Some(err.to_string()),
            };
            if let Some(failure) = failure {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(Error::other(failure));
            }
        }

        let groups = pair::group(pair::collect(&staging)?);
        if watched_fanbox
            && groups
                .iter()
                .any(|group| group.meta.as_ref().is_some_and(fanbox::restricted))
        {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::other(
                "The configured FANBOX session cannot access this post",
            ));
        }
        let posts = groups_to_posts(groups, target);

        match output {
            Ok(out) => {
                if posts.is_empty() {
                    let _ = std::fs::remove_dir_all(&staging);
                    let tail = if require_complete {
                        String::new()
                    } else {
                        stderr_tail(&out, 6)
                    };
                    return Err(Error::other(format!(
                        "gallery-dl produced no media for {target}{}{}",
                        if out.status.success() {
                            ""
                        } else {
                            " (exit non-zero)"
                        },
                        if tail.is_empty() {
                            String::new()
                        } else {
                            format!(": {tail}")
                        }
                    )));
                }
                if !out.status.success() {
                    tracing::warn!(
                        target,
                        "gallery-dl exited non-zero but files were downloaded: {}",
                        stderr_tail(&out, 3)
                    );
                }
                Ok(posts)
            }
            Err(err) => {
                if !posts.is_empty() {
                    tracing::warn!(target, "import reported {err} but files exist");
                    return Ok(posts);
                }
                let _ = std::fs::remove_dir_all(&staging);
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_names() {
        assert_eq!(cookie_site("www.pixiv.net"), Some("pixiv"));
        assert_eq!(cookie_site("fanbox.cc"), Some("fanbox"));
        assert_eq!(cookie_site("notfanbox.cc"), None);
        assert_eq!(cookie_site("x.com"), Some("twitter"));
        assert_eq!(cookie_site("twitter.com"), Some("twitter"));
        assert_eq!(cookie_site("danbooru.donmai.us"), Some("danbooru"));
        assert_eq!(cookie_site("example.com"), None);
    }

    #[test]
    fn creator_posts_use_fanbox_cookies() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_home(home.path().to_path_buf()).unwrap();
        let cookies = cfg.cookies_dir();
        std::fs::create_dir(&cookies).unwrap();
        std::fs::write(cookies.join("pixiv.txt"), "").unwrap();
        let source = GalleryDlSource::new(cfg);
        let target = "https://creator.fanbox.cc/posts/123";
        assert!(source.cookie_file(target).is_none());

        let host_file = cookies.join("creator.fanbox.cc.txt");
        std::fs::write(&host_file, "").unwrap();
        assert_eq!(source.cookie_file(target), Some(host_file));

        let fanbox = cookies.join("fanbox.txt");
        std::fs::write(&fanbox, "").unwrap();
        assert_eq!(source.cookie_file(target), Some(fanbox));
    }

    #[test]
    fn args_carry_cookies_and_proxy() {
        let staging = Path::new("/tmp/stage");
        let cookies = Path::new("/home/me/cookies/pixiv.txt");
        let args = download_args(
            staging,
            "https://www.pixiv.net/artworks/1",
            Some(cookies),
            Some("socks5h://127.0.0.1:1080"),
        );
        assert_eq!(args[0], "-d");
        assert!(args.contains(&"--write-metadata".to_string()));
        let cookies_at = args.iter().position(|a| a == "--cookies").unwrap();
        assert_eq!(args[cookies_at + 1], "/home/me/cookies/pixiv.txt");
        let proxy_at = args.iter().position(|a| a == "--proxy").unwrap();
        assert_eq!(args[proxy_at + 1], "socks5h://127.0.0.1:1080");
        assert_eq!(args.last().unwrap(), "https://www.pixiv.net/artworks/1");

        let plain = download_args(staging, "https://x.com/a/status/1", None, None);
        assert!(!plain.iter().any(|a| a == "--cookies"));
        assert!(!plain.iter().any(|a| a == "--proxy"));
    }

    #[test]
    fn supports_only_urls() {
        let cfg = Config::load_from_home(PathBuf::from("/tmp/nujijushi-test")).unwrap();
        let source = GalleryDlSource::new(cfg);
        assert!(source.supports("https://www.pixiv.net/artworks/1"));
        assert!(source.supports("http://x.com/user/status/1"));
        assert!(!source.supports("/tmp/some/dir"));
        assert!(!source.supports("pixiv.net/artworks/1"));
    }

    #[tokio::test]
    async fn missing_binary_is_reported_clearly() {
        let err = run_with_timeout("definitely-not-a-real-binary-xyz", &[], 5)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[tokio::test]
    async fn timeout_is_enforced() {
        let err = run_with_timeout("/bin/sleep", &["5".to_string()], 1)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Timeout(_)), "got: {err}");
    }

    #[test]
    fn meta_less_groups_still_yield_posts() {
        let groups = vec![Group {
            key: "abc".into(),
            meta: None,
            files: vec![PostFile {
                path: PathBuf::from("/tmp/a.png"),
                is_video: false,
                sensitive: false,
                meta: None,
            }],
        }];
        let posts = groups_to_posts(groups, "https://x.com/u/status/1");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].source_id, "abc");
        assert_eq!(posts[0].source, "twitter");
    }
}
