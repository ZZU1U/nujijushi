//! One HTTP client for everything that speaks to the network directly.

use std::time::Duration;

use crate::config::Config;
use crate::error::{Error, Result};

/// A client honouring `[proxy]`, with rustls and no system openssl.
///
/// Used for storage uploads and the t.me embed fallback; the Telegram client
/// gets the same proxy (see `tg::teloxide_impl`).
pub fn client(cfg: &Config) -> Result<reqwest::Client> {
    client_with_redirects(cfg, reqwest::redirect::Policy::limited(5))
}

pub(crate) fn client_with_redirects(
    cfg: &Config,
    redirects: reqwest::redirect::Policy,
) -> Result<reqwest::Client> {
    let mut builder = client_builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(redirects);

    if let Some(url) = cfg.proxy_url.as_deref() {
        // reqwest silently accepts a bare host, so be explicit about schemes
        const SCHEMES: &[&str] = &["http://", "https://", "socks5://", "socks5h://"];
        if !SCHEMES.iter().any(|scheme| url.starts_with(scheme)) {
            return Err(Error::config(format!(
                "invalid [proxy] url {url:?}: expected http://, https://, socks5:// or socks5h://"
            )));
        }
        let mut proxy = reqwest::Proxy::all(url)
            .map_err(|err| Error::config(format!("invalid [proxy] url {url:?}: {err}")))?;
        if let Some(hosts) = cfg.proxy_no_proxy.as_deref()
            && let Some(no_proxy) = reqwest::NoProxy::from_string(hosts)
        {
            proxy = proxy.no_proxy(Some(no_proxy));
        }
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|err| Error::config(format!("cannot build the http client: {err}")))
}

fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().user_agent(format!("nujijushi/{}", crate::VERSION))
}

/// The tagger address is constructed from a loopback IP and a configured port.
pub(crate) fn loopback_client() -> Result<reqwest::Client> {
    client_builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|err| Error::config(format!("cannot build the local HTTP client: {err}")))
}

/// `--proxy` style arguments for an external tool, or nothing.
pub fn proxy_args(cfg: &Config, flag: &str) -> Vec<String> {
    match cfg.proxy_url.as_deref() {
        Some(url) => vec![flag.to_string(), url.to_string()],
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg_with(proxy: Option<&str>, no_proxy: Option<&str>) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        std::mem::forget(dir);
        cfg.proxy_url = proxy.map(|value| value.to_string());
        cfg.proxy_no_proxy = no_proxy.map(|value| value.to_string());
        cfg
    }

    #[test]
    fn builds_without_a_proxy() {
        let cfg = cfg_with(None, None);
        assert!(client(&cfg).is_ok());
        assert!(proxy_args(&cfg, "--proxy").is_empty());
    }

    #[test]
    fn builds_with_http_and_socks_proxies() {
        for url in [
            "http://127.0.0.1:8080",
            "https://proxy.example.com:8443",
            "socks5://127.0.0.1:1080",
            "socks5h://127.0.0.1:1080",
        ] {
            let cfg = cfg_with(Some(url), Some("localhost,127.0.0.1,.lan"));
            assert!(client(&cfg).is_ok(), "{url} should be accepted");
            assert_eq!(
                proxy_args(&cfg, "--proxy"),
                vec!["--proxy".to_string(), url.to_string()]
            );
        }
    }

    #[test]
    fn rejects_broken_proxy_urls() {
        for broken in [
            "not-a-proxy",
            "socks4://127.0.0.1:1080",
            "ftp://x:21",
            "socks5h:127.0.0.1:1080",
        ] {
            let cfg = cfg_with(Some(broken), None);
            let error = client(&cfg).unwrap_err();
            assert!(
                error.to_string().contains("invalid [proxy] url"),
                "{broken} should be rejected, got: {error}"
            );
        }
    }

    #[test]
    fn proxy_args_use_the_requested_flag() {
        let cfg = cfg_with(Some("socks5://10.0.0.1:9050"), None);
        assert_eq!(
            proxy_args(&cfg, "-x"),
            vec!["-x".to_string(), "socks5://10.0.0.1:9050".to_string()]
        );
        let _ = PathBuf::from("x");
    }
}
