//! WebDAV sink: Nextcloud, ownCloud, Synology, HiDrive, Box, pCloud, …
//!
//! Directories are created with MKCOL (405 means "already there"), files with
//! PUT. The same HTTP client (and therefore the same proxy) is used as for
//! Telegram.

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};

use super::{MirrorPost, Sink};
use crate::config::WebdavSinkConfig;
use crate::error::{Error, Result};

pub struct WebdavSink {
    client: Client,
    base_url: String,
    username: String,
    password: String,
    root: String,
}

impl WebdavSink {
    pub fn new(client: Client, cfg: WebdavSinkConfig, root: String) -> Self {
        Self {
            client,
            base_url: cfg.url.trim_end_matches('/').to_string(),
            username: cfg.username,
            password: cfg.password,
            root,
        }
    }

    fn auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.username.is_empty() {
            request
        } else {
            request.basic_auth(&self.username, Some(&self.password))
        }
    }

    fn url(&self, relative: &str) -> String {
        let mut path = String::new();
        if !self.root.is_empty() {
            path.push_str(&self.root);
            path.push('/');
        }
        path.push_str(relative);
        format!("{}/{}", self.base_url, path)
    }

    async fn status(&self, method: Method, url: &str) -> Result<StatusCode> {
        let request = self.auth(self.client.request(method, url));
        let response = request
            .send()
            .await
            .map_err(|err| Error::Other(format!("{url}: {err}")))?;
        Ok(response.status())
    }

    async fn ensure_collections(&self, dir: &str) -> Result<()> {
        let mut prefix = String::new();
        for segment in dir.split('/').filter(|segment| !segment.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            let url = self.url(&prefix);
            let status = self
                .status(Method::from_bytes(b"MKCOL").unwrap(), &url)
                .await?;
            if !(status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED) {
                return Err(Error::Other(format!(
                    "MKCOL {url} returned {status}{}",
                    match status {
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN =>
                            " (check the webdav username/password)",
                        StatusCode::CONFLICT => " (the base url does not exist)",
                        _ => "",
                    }
                )));
            }
        }
        Ok(())
    }

    async fn put_file(&self, relative: &str, body: Vec<u8>, content_type: &str) -> Result<()> {
        let url = self.url(relative);
        let response = self
            .auth(self.client.put(&url))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .map_err(|err| Error::Other(format!("PUT {url}: {err}")))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(Error::Other(format!(
            "PUT {url} returned {status}{}",
            match status {
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN =>
                    " (check the webdav username/password)",
                StatusCode::CONFLICT => " (missing parent collection)",
                _ => "",
            }
        )))
    }
}

#[async_trait]
impl Sink for WebdavSink {
    fn name(&self) -> &'static str {
        "webdav"
    }

    fn describe(&self) -> String {
        let prefix = if self.root.is_empty() {
            String::new()
        } else {
            format!("{}/", self.root)
        };
        format!("upload to {}/{prefix}", self.base_url)
    }

    async fn put(&self, item: &MirrorPost) -> Result<String> {
        self.ensure_collections(&item.dir).await?;
        for file in &item.files {
            let bytes = tokio::fs::read(&file.path)
                .await
                .map_err(|err| Error::Other(format!("reading {}: {err}", file.path.display())))?;
            let ext = file.relative.rsplit('.').next().unwrap_or("");
            let content_type = crate::blob::mime_for(ext).unwrap_or("application/octet-stream");
            self.put_file(&file.relative, bytes, content_type).await?;
        }
        self.put_file(
            &item.meta_name(),
            item.meta.clone().into_bytes(),
            "application/json",
        )
        .await?;
        Ok(self.url(&item.dir))
    }

    async fn check(&self) -> Result<String> {
        let url = self.url("");
        let status = self
            .status(Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .await?;
        if status.is_success()
            || status == StatusCode::MULTI_STATUS
            || status == StatusCode::MOVED_PERMANENTLY
        {
            Ok(format!("{url} is reachable"))
        } else {
            Err(Error::Other(format!(
                "PROPFIND {url} returned {status}{}",
                match status {
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN =>
                        " (check the webdav username/password)",
                    StatusCode::NOT_FOUND =>
                        " (the base url does not exist - create the folder first)",
                    _ => "",
                }
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests_support::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::{HeaderMap, Method, StatusCode};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorded {
        methods: Vec<(String, String, usize, Option<String>)>,
    }

    async fn handler(
        State(record): State<Arc<Mutex<Recorded>>>,
        method: Method,
        uri: axum::http::Uri,
        headers: HeaderMap,
        body: Bytes,
    ) -> StatusCode {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        record.lock().unwrap().methods.push((
            method.to_string(),
            uri.path().to_string(),
            body.len(),
            auth,
        ));
        match method.as_str() {
            "MKCOL" => StatusCode::CREATED,
            "PUT" => StatusCode::CREATED,
            "PROPFIND" => StatusCode::MULTI_STATUS,
            _ => StatusCode::OK,
        }
    }

    async fn spawn() -> (String, Arc<Mutex<Recorded>>) {
        let record = Arc::new(Mutex::new(Recorded::default()));
        let app: Router = Router::new().fallback(handler).with_state(record.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/dav/art"), record)
    }

    fn sink(url: &str) -> WebdavSink {
        WebdavSink::new(
            crate::http::client(&crate::config::Config::load_from_home("/tmp/x".into()).unwrap())
                .unwrap(),
            WebdavSinkConfig {
                url: url.to_string(),
                username: "me".into(),
                password: "secret".into(),
            },
            "nujijushi".into(),
        )
    }

    #[tokio::test]
    async fn creates_collections_and_uploads_files() {
        let (url, record) = spawn().await;
        let dir = tempfile::tempdir().unwrap();
        let item = sample_mirror(&dir);

        let external = sink(&url).put(&item).await.unwrap();
        assert_eq!(external, format!("{url}/nujijushi/pixiv/nightjar/123456"));

        let calls = record.lock().unwrap().methods.clone();
        let mkcol: Vec<&String> = calls
            .iter()
            .filter(|(method, ..)| method == "MKCOL")
            .map(|(_, path, ..)| path)
            .collect();
        // three directories: source, artist, post
        assert_eq!(mkcol.len(), 3, "got {mkcol:?}");
        assert!(mkcol[0].ends_with("/nujijushi/pixiv"));
        assert!(mkcol[2].ends_with("/nujijushi/pixiv/nightjar/123456"));

        let puts: Vec<&(String, String, usize, Option<String>)> = calls
            .iter()
            .filter(|(method, ..)| method == "PUT")
            .collect();
        assert_eq!(puts.len(), 3, "two media files + post.json");
        assert!(puts.iter().any(|(_, path, ..)| path.ends_with("/00.png")));
        assert!(puts.iter().any(|(_, path, ..)| path.ends_with("/01.mp4")));
        assert!(
            puts.iter()
                .any(|(_, path, ..)| path.ends_with("/post.json"))
        );
        // basic auth is sent on every call
        assert!(calls.iter().all(|(_, _, _, auth)| {
            auth.as_deref()
                .map(|value| value.starts_with("Basic "))
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn reports_bad_credentials() {
        let record = Arc::new(Mutex::new(Recorded::default()));
        let app: Router = Router::new()
            .fallback(|State(_r): State<Arc<Mutex<Recorded>>>| async { StatusCode::UNAUTHORIZED })
            .with_state(record);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let item = sample_mirror(&dir);
        let error = sink(&format!("http://{addr}/dav"))
            .put(&item)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("username/password"), "got: {error}");
    }

    #[tokio::test]
    async fn check_props_the_base_url() {
        let (url, _record) = spawn().await;
        let report = sink(&url).check().await.unwrap();
        assert!(report.contains("reachable"), "got: {report}");
    }
}
