//! Domain types shared by every layer.

use std::fmt;

use chrono::{DateTime, Utc};

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? } default $default:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            /// Lenient parse for values read back from the database.
            pub fn parse(s: &str) -> Self {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($text => Self::$variant,)+
                    _ => Self::$default,
                }
            }

            /// Strict parse for user input.
            pub fn try_parse(s: &str) -> Option<Self> {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($text => Some(Self::$variant),)+
                    _ => None,
                }
            }

            pub const fn all() -> &'static [Self] {
                &[$(Self::$variant),+]
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = crate::error::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_parse(s).ok_or_else(|| {
                    crate::error::Error::invalid(format!("unknown {}: {s}", stringify!($name)))
                })
            }
        }
    };
}

string_enum! {
    /// Platform-reported content rating.
    Rating {
        Sfw => "sfw",
        Questionable => "questionable",
        Explicit => "explicit",
    } default Sfw
}

string_enum! {
    /// Curator state of an archive entry.
    PostState {
        New => "new",
        Approved => "approved",
        Rejected => "rejected",
        Archived => "archived",
    } default New
}

string_enum! {
    /// Lifecycle of a queue entry. `unknown` is a reconciled `sending` whose
    /// outcome could not be confirmed (never auto-resent).
    QueueState {
        Queued => "queued",
        Sending => "sending",
        Sent => "sent",
        Failed => "failed",
        Skipped => "skipped",
        Unknown => "unknown",
    } default Queued
}

string_enum! {
    /// How a channel treats NSFW posts.
    NsfwPolicy {
        Allow => "allow",
        Spoil => "spoil",
        Reject => "reject",
    } default Spoil
}

string_enum! {
    /// Attribution placement in the caption.
    Attribution {
        Footer => "footer",
        Link => "link",
        None => "none",
    } default Footer
}

string_enum! {
    /// How tags are rendered.
    TagStyle {
        Hashtags => "hashtags",
        Plain => "plain",
        None => "none",
    } default Hashtags
}

string_enum! {
    /// Caption parse mode.
    ParseMode {
        Html => "html",
        MarkdownV2 => "markdownv2",
        Plain => "plain",
    } default Html
}

string_enum! {
    /// What a tag rule does.
    TagRuleAction {
        Block => "block",
        Nsfw => "nsfw",
    } default Nsfw
}

impl Rating {
    pub fn is_nsfw(self) -> bool {
        matches!(self, Rating::Questionable | Rating::Explicit)
    }

    /// The same rule as [`Rating::is_nsfw`], as SQL, so the database view of
    /// "nsfw" can never drift from the Rust one. `column` is the (aliased)
    /// rating column, e.g. `p.rating`.
    pub fn nsfw_sql(column: &str) -> String {
        let nsfw: Vec<String> = Rating::all()
            .iter()
            .filter(|rating| rating.is_nsfw())
            .map(|rating| format!("'{}'", rating.as_str()))
            .collect();
        format!("{column} IN ({})", nsfw.join(", "))
    }
}

/// The one artist-label rule: name, else handle, else a placeholder.
///
/// Both `Author` (stored) and `source::PostAuthor` (freshly normalized) need it,
/// and they must not disagree: the label lands in captions *and* in the archive
/// path.
pub fn display_author(name: Option<&str>, handle: Option<&str>) -> String {
    name.or(handle)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown artist".to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Author {
    pub id: i64,
    pub source: String,
    pub source_id: String,
    pub name: Option<String>,
    pub handle: Option<String>,
    pub url: Option<String>,
    pub blocked: bool,
}

impl Author {
    pub fn display(&self) -> String {
        display_author(self.name.as_deref(), self.handle.as_deref())
    }
}

/// The `display_author` rule as SQL over an `authors` alias (`a`).
pub fn author_label_sql(alias: &str) -> String {
    format!("COALESCE({alias}.name, {alias}.handle)")
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaItem {
    pub id: i64,
    pub post_id: i64,
    pub ordinal: i64,
    pub blob_hash: String,
    pub ext: String,
    pub mime: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: i64,
    pub is_video: bool,
    pub poster_hash: Option<String>,
    /// The source flagged *this item* as sensitive (twitter does it per image).
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub id: i64,
    pub source: String,
    pub source_id: String,
    pub url: Option<String>,
    pub author: Option<Author>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub rating: Rating,
    pub posted_at: Option<DateTime<Utc>>,
    pub imported_at: DateTime<Utc>,
    pub nsfw_override: Option<bool>,
    pub state: PostState,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub media: Vec<MediaItem>,
}

impl Post {
    /// Effective NSFW flag: manual override wins, else rating, else a sensitive
    /// media item, else the channel's nsfw tag rules.
    pub fn is_nsfw(&self, nsfw_tag_hit: bool) -> bool {
        if let Some(flag) = self.nsfw_override {
            return flag;
        }
        self.rating.is_nsfw() || nsfw_tag_hit || self.media.iter().any(|item| item.sensitive)
    }

    pub fn author_label(&self) -> String {
        match &self.author {
            Some(a) => a.display(),
            None => "unknown artist".to_string(),
        }
    }

    pub fn first_media(&self) -> Option<&MediaItem> {
        self.media.first()
    }

    pub fn thumb_hash(&self) -> Option<&str> {
        let first = self.media.first()?;
        Some(first.poster_hash.as_deref().unwrap_or(&first.blob_hash))
    }
}

/// Light-weight projection used by the post grid.
#[derive(Debug, Clone, PartialEq)]
pub struct PostCard {
    pub id: i64,
    pub source: String,
    pub source_id: String,
    pub rating: Rating,
    pub state: PostState,
    pub author_name: Option<String>,
    pub imported_at: DateTime<Utc>,
    pub media_count: i64,
    pub thumb_hash: Option<String>,
    pub snippet: Option<String>,
    /// How many channels this post was pushed to.
    pub pushed: i64,
    /// How many of those pushes already reached Telegram.
    pub delivered: i64,
    /// Channel aliases it was pushed to, comma separated (`night-shift,main`).
    pub channels: Option<String>,
    /// Whether this post is nsfw *as the UI must show it*: manual override is
    /// honoured, otherwise the rating decides (`Rating::nsfw_sql`). The queue and
    /// the grid both read this, so they cannot disagree.
    pub nsfw: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub id: i64,
    /// Short human handle used by the bot and the CLI (`night-shift`).
    pub alias: String,
    pub title: String,
    pub tg_id: String,
    /// True when the bot found this channel through a `my_chat_member` update.
    pub discovered: bool,
    pub enabled: bool,
    pub paused: bool,
    pub interval_minutes: i64,
    pub posts_per_slot: i64,
    pub active_from: String,
    pub active_to: String,
    pub tz: String,
    pub daily_cap: i64,
    pub jitter_secs: i64,
    pub artist_cooldown: i64,
    pub nsfw_policy: NsfwPolicy,
    pub min_media: i64,
    pub blocked_tags: String,
    pub required_tags: String,
    pub parse_mode: ParseMode,
    pub template_name: String,
    pub template: Option<String>,
    pub attribution: Attribution,
    pub tag_style: TagStyle,
    pub max_tags: i64,
    pub silent: bool,
    pub protect: bool,
    pub include_source_tag: bool,
    pub last_sent_at: Option<DateTime<Utc>>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub consecutive_failures: i64,
    pub created_at: DateTime<Utc>,
}

impl Channel {
    pub fn blocked_tags_list(&self) -> Vec<String> {
        split_csv(&self.blocked_tags)
    }

    pub fn required_tags_list(&self) -> Vec<String> {
        split_csv(&self.required_tags)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub id: i64,
    pub post_id: i64,
    pub channel_id: i64,
    pub state: QueueState,
    pub priority: i64,
    pub pinned: bool,
    pub lease_token: Option<String>,
    pub lease_at: Option<DateTime<Utc>>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub sent_at: Option<DateTime<Utc>>,
    pub tg_message_ids: Option<String>,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendLog {
    pub id: i64,
    pub channel_id: i64,
    pub post_id: Option<i64>,
    pub sent_at: DateTime<Utc>,
    pub ok: bool,
    pub tg_message_ids: Option<String>,
    pub caption: Option<String>,
    pub error: Option<String>,
}

pub fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn join_csv<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let t = item.as_ref().trim();
        if !t.is_empty() && !out.iter().any(|existing| existing == t) {
            out.push(t.to_string());
        }
    }
    out.join(", ")
}

impl Channel {
    /// Sensible defaults for a brand new channel.
    pub fn new(title: impl Into<String>, tg_id: impl Into<String>) -> Self {
        let tg_id = tg_id.into();
        Self {
            id: 0,
            alias: slugify(&tg_id),
            title: title.into(),
            tg_id,
            discovered: false,
            enabled: true,
            paused: false,
            interval_minutes: 180,
            posts_per_slot: 1,
            active_from: "09:00".to_string(),
            active_to: "23:00".to_string(),
            tz: "UTC".to_string(),
            daily_cap: 0,
            jitter_secs: 300,
            artist_cooldown: 3,
            nsfw_policy: NsfwPolicy::Spoil,
            min_media: 1,
            blocked_tags: String::new(),
            required_tags: String::new(),
            parse_mode: ParseMode::Html,
            template_name: "default".to_string(),
            template: None,
            attribution: Attribution::Footer,
            tag_style: TagStyle::Hashtags,
            max_tags: 10,
            silent: false,
            protect: false,
            // sources are not tags: a caption does not need #twitter appended to it
            include_source_tag: false,
            last_sent_at: None,
            next_due_at: None,
            consecutive_failures: 0,
            created_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_author_prefers_name_then_handle() {
        assert_eq!(
            display_author(Some("Nightjar"), Some("nightjar_")),
            "Nightjar"
        );
        assert_eq!(display_author(None, Some("asobinin")), "asobinin");
        assert_eq!(display_author(None, None), "unknown artist");
        // the SQL mirror names the same two columns, in the same order
        assert_eq!(author_label_sql("a"), "COALESCE(a.name, a.handle)");
    }

    #[test]
    fn nsfw_sql_lists_exactly_the_nsfw_ratings() {
        let sql = Rating::nsfw_sql("p.rating");
        assert_eq!(sql, "p.rating IN ('questionable', 'explicit')");
        assert!(!sql.contains("'sfw'"), "sfw must never be in the nsfw set");
    }
    #[test]
    fn string_enum_roundtrip() {
        assert_eq!(Rating::parse("EXPLICIT"), Rating::Explicit);
        assert_eq!(Rating::Explicit.as_str(), "explicit");
        assert_eq!(NsfwPolicy::try_parse("spoil"), Some(NsfwPolicy::Spoil));
        assert_eq!(NsfwPolicy::try_parse("nope"), None);
        assert!("reject".parse::<NsfwPolicy>().is_ok());
        assert!(Rating::Questionable.is_nsfw());
        assert!(!Rating::Sfw.is_nsfw());
    }

    #[test]
    fn csv_helpers() {
        assert_eq!(split_csv("a, b ,,c"), vec!["a", "b", "c"]);
        assert_eq!(join_csv(["a", "b", "a", " "]), "a, b");
    }

    #[test]
    fn override_beats_rating() {
        let mut post = Post {
            id: 1,
            source: "test".into(),
            source_id: "1".into(),
            url: None,
            author: None,
            title: None,
            description: None,
            rating: Rating::Explicit,
            posted_at: None,
            imported_at: Utc::now(),
            nsfw_override: Some(false),
            state: PostState::New,
            notes: None,
            tags: vec![],
            media: vec![],
        };
        assert!(!post.is_nsfw(false));
        post.nsfw_override = None;
        assert!(post.is_nsfw(false));
        assert!(post.is_nsfw(true) || post.rating.is_nsfw());
    }
}

/// Lowercase handle: `@My Channel` -> `my-channel`, `-1001234567890` -> `ch-1001234567890`.
pub fn slugify(raw: &str) -> String {
    let lowered = raw.trim().trim_start_matches('@').to_ascii_lowercase();
    let numeric = !lowered.is_empty()
        && (lowered.starts_with('-') || lowered.chars().all(|c| c.is_ascii_digit()));
    let source = if numeric {
        format!("ch{}", lowered.trim_start_matches('-'))
    } else {
        lowered
    };
    let mut out = String::with_capacity(source.len());
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let capped: String = trimmed.chars().take(32).collect();
    let trimmed = capped.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "channel".to_string()
    } else {
        trimmed
    }
}

/// A path component: lowercase, digits kept as-is (unlike [`slugify`], which
/// prefixes numeric chat ids), anything unsafe collapsed into `-`.
pub fn path_slug(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    let capped: String = trimmed.chars().take(48).collect();
    let trimmed = capped.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod slug_tests {
    use super::slugify;

    #[test]
    fn path_slugs_keep_numbers() {
        assert_eq!(super::path_slug("123456"), "123456");
        assert_eq!(super::path_slug("Nightjar"), "nightjar");
        assert_eq!(super::path_slug("a b/c"), "a-b-c");
        assert_eq!(super::path_slug("  "), "unknown");
        assert_eq!(super::path_slug("x".repeat(80).as_str()).len(), 48);
    }

    #[test]
    fn slugs_are_stable_and_safe() {
        assert_eq!(slugify("@My Channel"), "my-channel");
        assert_eq!(slugify("night_shift"), "night_shift");
        assert_eq!(slugify("-1001234567890"), "ch1001234567890");
        assert_eq!(slugify("   "), "channel");
        assert_eq!(slugify("a  --  b"), "a-b");
        assert_eq!(slugify(&"x".repeat(60)).len(), 32);
    }
}
