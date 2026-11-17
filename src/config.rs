//! Layered configuration: file at `$NUJIJUSHI_HOME/config.toml`, overridable by env.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub home: PathBuf,
    pub bot_token: Option<String>,
    /// Run the Telegram command interface.
    pub bot_enabled: bool,
    /// Long polling (default) or a webhook served by the admin web server.
    pub bot_mode: BotMode,
    pub bot_poll_timeout_secs: u32,
    /// Public HTTPS base URL for webhook mode (e.g. `https://bot.example.com`).
    pub webhook_url: Option<String>,
    /// Webhook secret; generated and stored in the database when absent.
    pub webhook_secret: Option<String>,
    pub access: AccessConfig,
    pub public_telegram_fallback: bool,
    pub ui_token: Option<String>,
    pub bind: String,
    pub gallery_dl: String,
    pub ffmpeg: String,
    pub import_timeout_secs: u64,
    pub max_concurrent_imports: usize,
    pub caption_limit: usize,
    pub max_upload_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotMode {
    Polling,
    Webhook,
}

impl BotMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BotMode::Polling => "polling",
            BotMode::Webhook => "webhook",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "polling" | "poll" | "getupdates" => Some(BotMode::Polling),
            "webhook" | "push" => Some(BotMode::Webhook),
            _ => None,
        }
    }
}

/// Who may talk to the bot and what they may do.
#[derive(Debug, Clone)]
pub struct AccessConfig {
    pub admins: Vec<i64>,
    pub guests: Vec<i64>,
    /// Unknown users are registered as guests on first contact.
    pub public: bool,
    pub allow_import: bool,
    pub allow_browse: bool,
    pub allow_queue: bool,
    pub allow_publish: bool,
    pub allow_curate: bool,
    /// Let guests see NSFW posts in `/ls`.
    pub allow_nsfw_browse: bool,
    pub import_quota_per_day: u32,
    pub queue_quota_per_day: u32,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            admins: Vec::new(),
            guests: Vec::new(),
            public: false,
            allow_import: true,
            allow_browse: false,
            allow_queue: false,
            allow_publish: false,
            allow_curate: false,
            allow_nsfw_browse: false,
            import_quota_per_day: 20,
            queue_quota_per_day: 20,
        }
    }
}

impl AccessConfig {
    pub fn is_admin(&self, tg_id: i64) -> bool {
        self.admins.contains(&tg_id)
    }

    pub fn is_listed_guest(&self, tg_id: i64) -> bool {
        self.guests.contains(&tg_id)
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    telegram: TelegramSection,
    #[serde(default)]
    web: WebSection,
    #[serde(default)]
    import: ImportSection,
    #[serde(default)]
    limits: LimitsSection,
    #[serde(default)]
    bot: BotSection,
    #[serde(default)]
    access: AccessSection,
}

#[derive(Debug, Default, Deserialize)]
struct TelegramSection {
    bot_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WebSection {
    bind: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ImportSection {
    gallery_dl: Option<String>,
    ffmpeg: Option<String>,
    timeout_secs: Option<u64>,
    max_concurrent: Option<usize>,
    public_telegram_fallback: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct BotSection {
    enabled: Option<bool>,
    mode: Option<String>,
    poll_timeout_secs: Option<u32>,
    webhook_url: Option<String>,
    webhook_secret: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AccessSection {
    admins: Option<Vec<i64>>,
    guests: Option<Vec<i64>>,
    public: Option<bool>,
    allow_import: Option<bool>,
    allow_browse: Option<bool>,
    allow_queue: Option<bool>,
    allow_publish: Option<bool>,
    allow_curate: Option<bool>,
    allow_nsfw_browse: Option<bool>,
    import_quota_per_day: Option<u32>,
    queue_quota_per_day: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct LimitsSection {
    caption_chars: Option<usize>,
    max_upload_bytes: Option<u64>,
}

impl Config {
    /// Resolve the nujijushi home directory without loading the config file.
    pub fn resolve_home(override_home: Option<&Path>) -> Result<PathBuf> {
        if let Some(home) = override_home {
            return Ok(absolutize(home));
        }
        if let Ok(env_home) = std::env::var("NUJIJUSHI_HOME")
            && !env_home.trim().is_empty()
        {
            return Ok(absolutize(Path::new(env_home.trim())));
        }
        let base = directories::BaseDirs::new()
            .ok_or_else(|| Error::config("cannot determine home directory"))?;
        Ok(base.home_dir().join(".nujijushi"))
    }

    pub fn load(override_home: Option<&Path>) -> Result<Self> {
        let home = Self::resolve_home(override_home)?;
        Self::load_from_home(home)
    }

    pub fn load_from_home(home: PathBuf) -> Result<Self> {
        let path = home.join("config.toml");
        let file: FileConfig = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Error::config(format!("cannot read {}: {e}", path.display())))?;
            toml::from_str(&text)
                .map_err(|e| Error::config(format!("cannot parse {}: {e}", path.display())))?
        } else {
            FileConfig::default()
        };

        let env_str = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());

        let bot_token = env_str("TG_BOT_TOKEN").or(file.telegram.bot_token);
        let bot_enabled = match env_str("NUJIJUSHI_BOT").as_deref() {
            Some("0" | "false" | "no" | "off") => false,
            Some("1" | "true" | "yes" | "on") => true,
            _ => file.bot.enabled.unwrap_or(true),
        };
        let mode_raw = env_str("NUJIJUSHI_BOT_MODE").or(file.bot.mode);
        let bot_mode = match mode_raw.as_deref() {
            Some(raw) => BotMode::parse(raw).ok_or_else(|| {
                Error::config(format!(
                    "[bot] mode must be polling or webhook, got {raw:?}"
                ))
            })?,
            None => BotMode::Polling,
        };
        let admins = env_str("NUJIJUSHI_ADMINS")
            .map(|raw| parse_ids(&raw))
            .or(file.access.admins)
            .unwrap_or_default();
        let access = AccessConfig {
            admins,
            guests: file.access.guests.unwrap_or_default(),
            public: file.access.public.unwrap_or(false),
            allow_import: file.access.allow_import.unwrap_or(true),
            allow_browse: file.access.allow_browse.unwrap_or(false),
            allow_queue: file.access.allow_queue.unwrap_or(false),
            allow_publish: file.access.allow_publish.unwrap_or(false),
            allow_curate: file.access.allow_curate.unwrap_or(false),
            allow_nsfw_browse: file.access.allow_nsfw_browse.unwrap_or(false),
            import_quota_per_day: file.access.import_quota_per_day.unwrap_or(20),
            queue_quota_per_day: file.access.queue_quota_per_day.unwrap_or(20),
        };
        let ui_token = env_str("NUJIJUSHI_UI_TOKEN").or(file.web.token);
        let bind = env_str("NUJIJUSHI_BIND")
            .or(file.web.bind)
            .unwrap_or_else(|| "127.0.0.1:8788".to_string());

        Ok(Config {
            home,
            bot_token,
            bot_enabled,
            bot_mode,
            bot_poll_timeout_secs: file.bot.poll_timeout_secs.unwrap_or(30).clamp(1, 120),
            webhook_url: env_str("NUJIJUSHI_WEBHOOK_URL")
                .or(file.bot.webhook_url)
                .map(|url| url.trim().to_string())
                .filter(|url| !url.is_empty()),
            webhook_secret: env_str("NUJIJUSHI_WEBHOOK_SECRET")
                .or(file.bot.webhook_secret)
                .map(|secret| secret.trim().to_string())
                .filter(|secret| !secret.is_empty()),
            access,
            public_telegram_fallback: file.import.public_telegram_fallback.unwrap_or(true),
            ui_token,
            bind,
            gallery_dl: env_str("NUJIJUSHI_GALLERY_DL")
                .or(file.import.gallery_dl)
                .unwrap_or_else(|| "gallery-dl".to_string()),
            ffmpeg: file.import.ffmpeg.unwrap_or_else(|| "ffmpeg".to_string()),
            import_timeout_secs: file.import.timeout_secs.unwrap_or(1800),
            max_concurrent_imports: file.import.max_concurrent.unwrap_or(2).max(1),
            caption_limit: file.limits.caption_chars.unwrap_or(1024),
            max_upload_bytes: file.limits.max_upload_bytes.unwrap_or(50 * 1024 * 1024),
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.home.join("nujijushi.sqlite3")
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.home.join("blobs")
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.home.join("thumbs")
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.home.join("staging")
    }

    pub fn cookies_dir(&self) -> PathBuf {
        self.home.join("cookies")
    }

    pub fn export_dir(&self) -> PathBuf {
        self.home.join("export")
    }

    pub fn require_bot_token(&self) -> Result<&str> {
        self.bot_token
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| {
                Error::config(
                    "no Telegram bot token: set TG_BOT_TOKEN or [telegram] bot_token in config.toml",
                )
            })
    }

    /// Create the home layout (idempotent).
    pub fn ensure_layout(&self) -> Result<()> {
        for dir in [
            self.home.clone(),
            self.blobs_dir(),
            self.thumbs_dir(),
            self.staging_dir(),
            self.cookies_dir(),
            self.export_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| Error::config(format!("cannot create {}: {e}", dir.display())))?;
        }
        Ok(())
    }

    /// Write a starter config.toml if none exists. Returns true when written.
    pub fn write_default_config(&self, bot_token: Option<&str>) -> Result<bool> {
        let path = self.home.join("config.toml");
        if path.exists() {
            return Ok(false);
        }
        let token_line = match bot_token {
            Some(t) => format!("bot_token = \"{t}\""),
            None => "# bot_token = \"123456:ABC-DEF...\"\n# or export TG_BOT_TOKEN".to_string(),
        };
        let body = format!(
            r#"# nujijushi configuration
# Home directory: {home}
# Secrets may also come from the environment: TG_BOT_TOKEN, NUJIJUSHI_UI_TOKEN, NUJIJUSHI_BIND.

[telegram]
{token_line}

[web]
bind = "{bind}"
# Password for the admin UI (also NUJIJUSHI_UI_TOKEN). Generated on `nujijushi init`.
token = "{ui_token}"

[import]
# External helpers. `nujijushi doctor` verifies these.
gallery_dl = "gallery-dl"
ffmpeg = "ffmpeg"
timeout_secs = {timeout}
max_concurrent = 2

[limits]
caption_chars = 1024
max_upload_bytes = 52428800

[bot]
# Telegram command interface (started by `nujijushi run`).
enabled = true
# polling = ask Telegram for updates (works anywhere).
# webhook = Telegram POSTs to this server; needs a public HTTPS URL (reverse proxy).
mode = "polling"
poll_timeout_secs = 30
# webhook_url = "https://bot.example.com"
# webhook_secret = "generated-and-stored-on-first-run-if-empty"

[access]
# Telegram user ids allowed to administer the bot. Find yours with /whoami.
{admins}
# Extra users allowed in (they are auto-registered on first contact anyway).
guests = []
# false: only the ids above may use the bot. true: anyone may, as a guest.
public = {public}
allow_import = true
allow_browse = false
allow_queue = false
allow_publish = false
allow_curate = false
allow_nsfw_browse = false
import_quota_per_day = 20
queue_quota_per_day = 20
"#,
            home = self.home.display(),
            bind = self.bind,
            ui_token = self.ui_token.clone().unwrap_or_else(|| "change-me".into()),
            timeout = self.import_timeout_secs,
            admins = if self.access.admins.is_empty() {
                "admins = []".to_string()
            } else {
                format!(
                    "admins = [{}]",
                    self.access
                        .admins
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            public = self.access.public,
        );
        std::fs::write(&path, body)
            .map_err(|e| Error::config(format!("cannot write {}: {e}", path.display())))?;
        restrict_permissions(&path)?;
        Ok(true)
    }
}

/// `123, 456` -> [123, 456]
pub fn parse_ids(raw: &str) -> Vec<i64> {
    raw.split([',', ' ', ';'])
        .filter_map(|part| part.trim().parse::<i64>().ok())
        .collect()
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(unix)]
pub fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| Error::config(format!("cannot chmod {}: {e}", path.display())))
}

#[cfg(not(unix))]
pub fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn random_token() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32)
        .map(|_| {
            const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
            ALPHABET[rng.random_range(0..ALPHABET.len())] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_file() {
        let cfg = Config::load_from_home(PathBuf::from("/tmp/nujijushi-test-nonexistent")).unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:8788");
        assert_eq!(cfg.caption_limit, 1024);
        assert_eq!(cfg.gallery_dl, "gallery-dl");
        assert!(cfg.bot_token.is_none());
        assert!(cfg.bot_enabled);
        assert_eq!(cfg.bot_mode, BotMode::Polling);
        assert!(cfg.webhook_url.is_none());
        assert!(cfg.access.allow_import);
        assert!(!cfg.access.allow_publish);
        assert_eq!(cfg.access.import_quota_per_day, 20);
    }

    #[test]
    fn writes_default_config_once() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        cfg.ensure_layout().unwrap();
        assert!(cfg.write_default_config(Some("1:abc")).unwrap());
        assert!(!cfg.write_default_config(None).unwrap());
        let reloaded = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.bot_token.as_deref(), Some("1:abc"));
        assert!(dir.path().join("config.toml").exists());
        assert!(cfg.db_path().ends_with("nujijushi.sqlite3"));
    }

    #[test]
    fn access_section_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[access]\nadmins = [1, 2]\npublic = true\nallow_publish = true\nimport_quota_per_day = 5\n[bot]\nenabled = false\n",
        )
        .unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        assert_eq!(cfg.access.admins, vec![1, 2]);
        assert!(cfg.access.public);
        assert!(cfg.access.allow_publish);
        assert_eq!(cfg.access.import_quota_per_day, 5);
        assert!(!cfg.bot_enabled);
    }

    #[test]
    fn webhook_mode_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[bot]\nmode = \"webhook\"\nwebhook_url = \"https://bot.example.com/\"\nwebhook_secret = \"abc123\"\n",
        )
        .unwrap();
        let cfg = Config::load_from_home(dir.path().to_path_buf()).unwrap();
        assert_eq!(cfg.bot_mode, BotMode::Webhook);
        assert_eq!(cfg.webhook_url.as_deref(), Some("https://bot.example.com/"));
        assert_eq!(cfg.webhook_secret.as_deref(), Some("abc123"));

        // a typo must not silently fall back to polling
        std::fs::write(dir.path().join("config.toml"), "[bot]\nmode = \"webhok\"\n").unwrap();
        assert!(Config::load_from_home(dir.path().to_path_buf()).is_err());
    }

    #[test]
    fn parses_id_lists() {
        assert_eq!(parse_ids("1, 2;3 x 4"), vec![1, 2, 3, 4]);
        assert!(parse_ids("").is_empty());
    }

    #[test]
    fn home_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = Config::resolve_home(Some(dir.path())).unwrap();
        assert_eq!(resolved, dir.path().to_path_buf());
    }
}
