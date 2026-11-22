export interface Session { csrf: string }
export interface ActionResult { location?: string; message: string; job_ids: number[] }
export type ActionFields = Record<string, string | number | boolean | null | undefined>
export { action, api, expired, friendly, RequestError } from './api/client'

export interface Job {
  id: number
  kind: string
  kind_label: string
  state: string
  message: string | null
  payload: string | null
  progress: number
  total: number
  percent: number
  age: string
  created: string
  post_id: number | null
  active: boolean
  retryable: boolean
  track: boolean
}

export interface ChannelChoice { id: number; title: string }
export interface CardTag { name: string; url: string }
export interface Card {
  id: number; source: string; rating: string; state: string; artist: string | null
  submitter: string | null; submitter_guest: boolean; media_count: number
  thumb: string | null; nsfw: boolean; snippet: string | null; pushed: number
  queue_badges: { class: string; label: string; channel: string }[]
  tags: CardTag[]; imported: string
}
export interface CardsPage { cards: Card[]; next_cursor: string | null }
export interface PostFilters {
  state: string | null; source: string | null; tag: string | null; nsfw: string | null
  sent: string | null; search: string | null
}
export interface PostsPage extends CardsPage {
  total: number; page_number: number; page_count: number; per_page: number; page_sizes: number[]
  default_channel: number | null; grid_url: string; previous_page: string | null; next_page: string | null
  filters: PostFilters; sources: string[]; top_tags: { tag: string; count: number; url: string; active: boolean }[]
  clear_tag_url: string; channels: ChannelChoice[]; states: string[]; setup: boolean
}
export interface Tag { id: number; name: string }
export interface TagMatch { name: string; count: number }
export interface Post {
  id: number; source: string; source_id: string; url: string | null; title: string | null
  description: string | null; artist: string | null; artist_url: string | null
  rating: string; state: string; nsfw_override: string; nsfw: boolean
  tags: (Tag & { source: boolean; manual: boolean })[]; manual_tags: string; tag_removals: Tag[]
  notes: string | null; imported: string
}
export interface Media {
  id: number; hash: string; thumb: string; ext: string; is_video: boolean; width: number | null
  height: number | null; kib: number; hidden: boolean; can_select: boolean; can_compress: boolean
  compressed: boolean; over_photo_limit: boolean; undo_id: number | null
}
export interface TagSuggestion {
  id: number; kind: string; tag: string; wiki_url: string | null; evidence: string
  character_tag_id: number | null; images: number[]
}
export interface TagSuggestions { job: Job; stale: boolean; suggestions: TagSuggestion[] }
export interface PostCaption {
  channel: string; tg_id: string; allowed: boolean; spoiler: boolean; eta?: string; eta_in?: string
  caption: string; length: number; messages: number; verdict: string
}
export interface PostDetailPage {
  open_section: string | null; guest_review: boolean; post: Post
  submitter: { tg_id: number; label: string; admin: boolean } | null
  media_total: string; media: Media[]; can_merge: boolean; has_media_tools: boolean
  merge_job: Job | null; gallery_version: string; outgoing_count: number
  deliveries: { channel_id: number; channel: string; url: string }[]
  tag_review: TagSuggestions | null; can_suggest_tags: boolean; captions: PostCaption[]
  default_channel: number | null; ratings: string[]; states: string[]; channels: ChannelChoice[]
}
export interface PostDeletePage { id: number; title: string | null; imported_at: string; sending: boolean }
export interface SettingsPage {
  channels: ChannelChoice[]; default_channel: number | null; cleanup_job: Job | null
}
export interface CleanupPreview {
  confirmation: { cutoff: string; revision: string }
  posts: number; suggestions: number; files: number; estimated_bytes: number
}
export interface UnknownDelivery {
  id: number; channel_id: number; post_id: number; error: string | null; channel: string
  messages: string[]; public_channel: string | null
}
export interface ChannelSummary extends ChannelChoice {
  tg_id: string; queued: number; interval: number; nsfw_policy: string; paused: boolean; enabled: boolean; next: string
}
export interface DashboardChannel extends ChannelSummary { publishing: boolean; failures: number; next_at: string | null; reason: string | null }
export interface Scheduler { last_tick: string | null; age_secs: number | null; age: string; stale: boolean }
export interface SetupStep { title: string; detail: string; done: boolean; href: string | null; action: string }
export interface DashboardPage {
  stats: { posts: number; approved: number; queued: number; sent: number; unknown: number }
  channels: DashboardChannel[]; sends: { channel_id: number; post_id: number; ok: boolean; at: string; ids: string | null; error: string | null }[]
  jobs: Job[]; recent_unknown: UnknownDelivery[]
  storage: { name: string; where: string; done: number; pending: number; failed: number; blocked: number }[]
  scheduler: Scheduler; setup: { connected: boolean; next: SetupStep | null; steps: SetupStep[] } | null
}
export interface ChannelSummaryRow extends ChannelSummary {
  alias: string; discovered: boolean; tag_style: string; attribution: string; active: string
}
export interface ChannelsPage { channels: ChannelSummaryRow[]; timezones: string[]; templates: string[] }
export interface ChannelTarget extends ChannelChoice { tg_id: string }
export interface ChannelConfig extends ChannelTarget {
  alias: string; discovered: boolean; enabled: boolean; interval_minutes: number; active_from: string
  active_to: string; tz: string; daily_cap: number; jitter_secs: number | null; artist_cooldown: number | null
  nsfw_policy: string; min_media: number; blocked_tags: string; required_tags: string; tag_style: string
  max_tags: number; attribution: string; parse_mode: string; template_name: string; template: string | null
  silent: boolean; protect: boolean; include_source_tag: boolean; paused: boolean; failures: number
}
export interface CaptionSample { caption: string; reason: string; media: number; messages: number; spoiler: boolean }
export interface ChannelEditPage {
  channel: ChannelConfig; timezones: string[]; policies: string[]; tag_styles: string[]
  attributions: string[]; parse_modes: string[]; templates: string[]; default_template: string; sample: CaptionSample | null
}
export interface QueueStatus {
  revision: string
  queued: number
  jobs: {
    id: number; kind: 'publish_channel' | 'publish_item' | 'flush_channel'; queue_id: number | null
    label: string; state: string; message: string | null; progress: number; total: number
  }[]
  latest_attempt: { id: number; post_id: number | null; state: string; error: string | null } | null
}
export interface QueueRow {
  id: number; post_id: number; state: string; state_label: string; delivery: UnknownDelivery | null
  priority: number; pinned: boolean; attempts: number; error: string | null; artist: string | null
  snippet: string | null; thumb: string | null; media_count: number; nsfw: boolean; added: string
  eta: string | null; eta_in: string | null
}
export interface QueuePage {
  channel: ChannelTarget & { enabled: boolean; paused: boolean }
  activity: QueueStatus; items: QueueRow[]; eta_hint: string | null
}
export interface PreviewItem {
  slot: number; queue_id: number; post_id: number; title: string | null; caption: string
  media: number; messages: number; spoiler: boolean; nsfw: boolean; eta: string; eta_in: string; reason?: string
}
export interface PreviewPage {
  channel: ChannelTarget
  held: (Pick<PreviewItem, 'queue_id' | 'post_id' | 'caption' | 'media' | 'messages'> & { allowed: boolean; reason: string }) | null
  stopped: boolean; enable_item: { queue_id: number; post_id: number } | null; items: PreviewItem[]
}
export interface JobsPage { jobs: Job[]; parallel: number; waiting: number }
export interface ImportPage extends JobsPage { cookies: string }
export interface WatchesPage { watches: { id: number; name: string; url: string; initial_batch: number; enabled: boolean; job: Job | null }[] }
export interface DuplicatePost {
  id: number; title: string | null; artist: string; source: string; url: string | null; state: string
  queues: { alias: string | null; state: string; channel_id: number }[]
}
export interface DuplicateImage { hash: string; position: number; thumb: boolean }
export interface DuplicatesPage {
  pairs: { left: DuplicatePost; right: DuplicatePost; matches: { left: DuplicateImage; right: DuplicateImage; exact: boolean }[] }[]
  previous: number | null; next: number | null; indexing: boolean; pending: number; failed: number
}
export interface UsersPage {
  users: { tg_id: number; label: string; role: string; db_role: string; archived: number; pushed: number; sent: number; config_admin: boolean; last_seen: string }[]
}
