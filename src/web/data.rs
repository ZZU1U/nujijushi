//! Data returned to the React admin. Telegram captions remain Rust-rendered.

use serde::Serialize;

use super::routes::SchedulerView;
use crate::{db::queue as queue_db, model::Channel};

#[derive(Serialize)]
pub(super) struct ChannelChoice {
    id: i64,
    title: String,
}

impl From<&Channel> for ChannelChoice {
    fn from(channel: &Channel) -> Self {
        Self {
            id: channel.id,
            title: channel.title.clone(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct DashboardPage {
    pub stats: DashboardStats,
    pub channels: Vec<DashboardChannel>,
    pub sends: Vec<SendView>,
    pub jobs: Vec<JobView>,
    pub recent_unknown: Vec<queue_db::UnconfirmedDelivery>,
    pub storage: Vec<SinkView>,
    pub scheduler: SchedulerView,
    pub setup: Option<SetupView>,
}

#[derive(Serialize)]
pub(super) struct PostsPage {
    pub cards: Vec<PostCardView>,
    pub total: i64,
    pub page_number: i64,
    pub page_count: i64,
    pub per_page: i64,
    pub next_cursor: Option<String>,
    pub default_channel: Option<i64>,
    pub page_sizes: [i64; 3],
    pub grid_url: String,
    pub previous_page: Option<String>,
    pub next_page: Option<String>,
    pub filters: FiltersView,
    pub sources: Vec<String>,
    pub top_tags: Vec<TagCount>,
    pub clear_tag_url: String,
    pub channels: Vec<ChannelChoice>,
    pub states: Vec<&'static str>,
    pub setup: bool,
}

#[derive(Serialize)]
pub(super) struct PostDetailPage {
    pub open_section: Option<String>,
    pub guest_review: bool,
    pub post: PostView,
    pub submitter: Option<SubmitterView>,
    pub media_total: String,
    pub media: Vec<MediaView>,
    pub can_merge: bool,
    pub has_media_tools: bool,
    pub merge_job: Option<JobView>,
    pub gallery_version: String,
    pub outgoing_count: usize,
    pub deliveries: Vec<crate::db::queue::DeliveryLink>,
    pub tag_review: Option<super::tag_suggestions::ReviewView>,
    pub can_suggest_tags: bool,
    pub captions: Vec<CaptionView>,
    pub default_channel: Option<i64>,
    pub ratings: Vec<&'static str>,
    pub states: Vec<&'static str>,
    pub channels: Vec<ChannelChoice>,
}

#[derive(Serialize)]
pub(super) struct PostDeletePage {
    pub id: i64,
    pub title: Option<String>,
    pub imported_at: String,
    pub sending: bool,
}

#[derive(Serialize)]
pub(super) struct SettingsPage {
    pub channels: Vec<ChannelChoice>,
    pub default_channel: Option<i64>,
    pub cleanup_job: Option<JobView>,
}

#[derive(Serialize)]
pub(super) struct ChannelsPage {
    pub channels: Vec<ChannelSummary>,
    pub timezones: &'static [&'static str],
    pub templates: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct ChannelEditPage {
    pub channel: ChannelSettings,
    pub timezones: &'static [&'static str],
    pub policies: Vec<&'static str>,
    pub tag_styles: Vec<&'static str>,
    pub attributions: Vec<&'static str>,
    pub parse_modes: Vec<&'static str>,
    pub templates: Vec<String>,
    pub default_template: String,
    pub sample: Option<CaptionSample>,
}

#[derive(Serialize)]
pub(super) struct QueuePage {
    pub channel: QueueChannel,
    pub activity: super::queue_status::QueueActivity,
    pub items: Vec<QueueRow>,
    pub eta_hint: Option<&'static str>,
}

#[derive(Serialize)]
pub(super) struct PreviewPage {
    pub channel: ChannelTarget,
    pub held: Option<HeldPreview>,
    pub stopped: bool,
    pub enable_item: Option<PreviewSelection>,
    pub items: Vec<PreviewRow>,
}

#[derive(Serialize)]
pub(super) struct JobsPage {
    pub jobs: Vec<JobView>,
    pub parallel: usize,
    pub waiting: i64,
}

#[derive(Serialize)]
pub(super) struct ImportPage {
    pub jobs: Vec<JobView>,
    pub parallel: usize,
    pub waiting: i64,
    pub cookies: String,
}

#[derive(Serialize)]
pub(super) struct UsersPage {
    pub users: Vec<UserView>,
}

#[derive(Serialize)]
pub(super) struct WatchesPage {
    pub watches: Vec<WatchView>,
}

#[derive(Serialize)]
pub(super) struct DuplicatesPage {
    pub pairs: Vec<DuplicatePair>,
    pub previous: Option<i64>,
    pub next: Option<i64>,
    pub indexing: bool,
    pub pending: i64,
    pub failed: i64,
}

#[derive(Serialize)]
pub(super) struct CardsPage {
    pub cards: Vec<PostCardView>,
    pub next_cursor: Option<String>,
}
#[derive(Serialize)]
pub(super) struct CardBadge {
    pub class: &'static str,
    pub label: &'static str,
    pub channel: String,
}
#[derive(Serialize)]
pub(super) struct CardTag {
    pub name: String,
    pub url: String,
}
#[derive(Serialize)]
pub(super) struct WatchView {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub initial_batch: u16,
    pub enabled: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub job: Option<JobView>,
}

#[derive(Serialize)]
pub(super) struct PostCardView {
    pub id: i64,
    pub source: String,
    pub rating: &'static str,
    pub state: &'static str,
    pub artist: Option<String>,
    pub submitted_by: Option<i64>,
    pub submitter: Option<String>,
    pub submitter_guest: bool,
    pub media_count: i64,
    pub thumb: Option<String>,
    pub nsfw: bool,
    pub snippet: Option<String>,
    pub pushed: i64,
    pub queue_badges: Vec<CardBadge>,
    pub tags: Vec<CardTag>,
    pub imported: String,
}

#[derive(Serialize)]
pub struct JobView {
    pub id: i64,
    pub kind: String,
    pub kind_label: String,
    pub watch_id: Option<i64>,
    pub payload: Option<String>,
    pub state: String,
    pub progress: i64,
    pub total: i64,
    pub percent: i64,
    pub track: bool,
    pub message: Option<String>,
    pub post_id: Option<i64>,
    pub active: bool,
    pub retryable: bool,
    pub age_secs: i64,
    pub age: String,
    pub created: String,
}

#[derive(Serialize)]
pub(super) struct DashboardStats {
    pub posts: i64,
    pub approved: i64,
    pub queued: i64,
    pub sent: i64,
    pub unknown: i64,
}

#[derive(Serialize)]
pub(super) struct DashboardChannel {
    pub publishing: bool,
    pub id: i64,
    pub title: String,
    pub tg_id: String,
    pub queued: i64,
    pub interval: i64,
    pub nsfw_policy: &'static str,
    pub paused: bool,
    pub enabled: bool,
    pub failures: i64,
    pub next: String,
    pub next_at: Option<String>,
    pub reason: String,
}

#[derive(Serialize)]
pub(super) struct SendView {
    pub channel_id: i64,
    pub post_id: Option<i64>,
    pub ok: bool,
    pub at: String,
    pub ids: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct SinkView {
    pub name: String,
    #[serde(rename = "where")]
    pub location: String,
    pub done: i64,
    pub pending: i64,
    pub failed: i64,
    pub blocked: i64,
}

#[derive(Serialize)]
pub(super) struct FiltersView {
    pub state: Option<String>,
    pub source: Option<String>,
    pub tag: Option<String>,
    pub nsfw: Option<String>,
    pub sent: Option<String>,
    pub search: Option<String>,
}

#[derive(Serialize)]
pub(super) struct TagCount {
    pub tag: String,
    pub count: i64,
    pub url: String,
    pub active: bool,
}

#[derive(Serialize)]
pub(super) struct CaptionView {
    pub channel: String,
    pub tg_id: String,
    pub caption: String,
    pub length: usize,
    pub truncated: bool,
    pub messages: usize,
    pub verdict: String,
    pub allowed: bool,
    pub spoiler: bool,
    pub eta: Option<String>,
    pub eta_in: Option<String>,
}

#[derive(Serialize)]
pub(super) struct MediaView {
    pub id: i64,
    pub hash: String,
    pub thumb: String,
    pub ext: String,
    pub is_video: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub kib: f64,
    pub hidden: bool,
    pub can_select: bool,
    pub can_compress: bool,
    pub compressed: bool,
    pub over_photo_limit: bool,
    pub undo_id: Option<i64>,
}

#[derive(Serialize)]
pub(super) struct PostView {
    pub id: i64,
    pub source: String,
    pub source_id: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub artist: Option<String>,
    pub artist_url: Option<String>,
    pub rating: &'static str,
    pub state: &'static str,
    pub nsfw_override: &'static str,
    pub nsfw: bool,
    pub tags: Vec<crate::db::tags::TagProvenance>,
    pub manual_tags: String,
    pub tag_removals: Vec<crate::db::tags::TagRemoval>,
    pub notes: Option<String>,
    pub imported: String,
}

#[derive(Serialize)]
pub(super) struct SubmitterView {
    pub tg_id: i64,
    pub label: String,
    pub admin: bool,
}

#[derive(Serialize)]
pub(super) struct ChannelSummary {
    pub id: i64,
    pub alias: String,
    pub discovered: bool,
    pub title: String,
    pub tg_id: String,
    pub queued: i64,
    pub interval: i64,
    pub nsfw_policy: &'static str,
    pub tag_style: &'static str,
    pub attribution: &'static str,
    pub active: String,
    pub paused: bool,
    pub enabled: bool,
    pub next: String,
}

#[derive(Serialize)]
pub(super) struct ChannelSettings {
    pub id: i64,
    pub alias: String,
    pub discovered: bool,
    pub title: String,
    pub tg_id: String,
    pub enabled: bool,
    pub interval_minutes: i64,
    pub active_from: String,
    pub active_to: String,
    pub tz: String,
    pub daily_cap: i64,
    pub jitter_secs: i64,
    pub artist_cooldown: i64,
    pub nsfw_policy: &'static str,
    pub min_media: i64,
    pub blocked_tags: String,
    pub required_tags: String,
    pub tag_style: &'static str,
    pub max_tags: i64,
    pub attribution: &'static str,
    pub parse_mode: &'static str,
    pub template_name: String,
    pub template: Option<String>,
    pub silent: bool,
    pub protect: bool,
    pub include_source_tag: bool,
    pub paused: bool,
    pub failures: i64,
}

#[derive(Serialize)]
pub(super) struct CaptionSample {
    pub caption: String,
    pub reason: String,
    pub media: usize,
    pub messages: usize,
    pub spoiler: bool,
}

#[derive(Serialize)]
pub(super) struct QueueRow {
    pub id: i64,
    pub post_id: i64,
    pub state: &'static str,
    pub state_label: &'static str,
    /// A `sending` row whose lease is older than the crash cutoff: its delivery
    /// is unknown, so the queue page may offer the normal resolve actions.
    pub stale_sending: bool,
    pub delivery: Option<queue_db::UnconfirmedDelivery>,
    pub priority: i64,
    pub pinned: bool,
    pub attempts: i64,
    pub error: Option<String>,
    pub artist: Option<String>,
    pub snippet: Option<String>,
    pub thumb: Option<String>,
    pub media_count: i64,
    pub nsfw: bool,
    pub added: String,
    pub eta: Option<String>,
    pub eta_in: Option<String>,
}

#[derive(Serialize)]
pub(super) struct QueueChannel {
    pub id: i64,
    pub title: String,
    pub tg_id: String,
    pub enabled: bool,
    pub paused: bool,
}

#[derive(Serialize)]
pub(super) struct ChannelTarget {
    pub id: i64,
    pub title: String,
    pub tg_id: String,
}

#[derive(Serialize)]
pub(super) struct PreviewSelection {
    pub queue_id: i64,
    pub post_id: i64,
}

#[derive(Serialize)]
pub(super) struct PreviewRow {
    pub slot: usize,
    pub queue_id: i64,
    pub post_id: i64,
    pub title: String,
    pub caption: String,
    pub media: usize,
    pub messages: usize,
    pub spoiler: bool,
    pub nsfw: bool,
    pub eta: String,
    pub eta_in: String,
    pub reason: String,
}

#[derive(Serialize)]
pub(super) struct HeldPreview {
    pub queue_id: i64,
    pub post_id: i64,
    pub allowed: bool,
    pub caption: String,
    pub media: usize,
    pub messages: usize,
    pub reason: String,
}

#[derive(Serialize, Clone)]
pub(super) struct SetupStep {
    pub title: &'static str,
    pub detail: &'static str,
    pub done: bool,
    pub href: Option<String>,
    pub action: &'static str,
}

#[derive(Serialize)]
pub(super) struct SetupView {
    pub steps: Vec<SetupStep>,
    pub next: Option<SetupStep>,
    pub connected: bool,
}

#[derive(Serialize)]
pub(super) struct UserView {
    pub tg_id: i64,
    pub label: String,
    pub role: String,
    pub db_role: String,
    pub archived: i64,
    pub pushed: i64,
    pub sent: i64,
    pub config_admin: bool,
    pub last_seen: String,
}

#[derive(Serialize)]
pub(super) struct DuplicatePost {
    pub id: i64,
    pub title: Option<String>,
    pub artist: String,
    pub source: String,
    pub url: Option<String>,
    pub state: &'static str,
    pub queues: Vec<DuplicateQueue>,
}

#[derive(Serialize)]
pub(super) struct DuplicateQueue {
    pub alias: Option<String>,
    pub state: &'static str,
    pub channel_id: i64,
}

#[derive(Serialize)]
pub(super) struct DuplicateImage {
    pub hash: String,
    pub position: i64,
    pub thumb: bool,
}

#[derive(Serialize)]
pub(super) struct DuplicateMatch {
    pub left: DuplicateImage,
    pub right: DuplicateImage,
    pub exact: bool,
}

#[derive(Serialize)]
pub(super) struct DuplicatePair {
    pub left: DuplicatePost,
    pub right: DuplicatePost,
    pub matches: Vec<DuplicateMatch>,
}
