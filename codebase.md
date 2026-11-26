# codebase.md — how nujijushi works

How the code flows, from a URL arriving to a post appearing in a Telegram channel.
For "how to change it safely" see [AGENTS.md](AGENTS.md).

```
                    ┌──────────── interfaces ────────────┐
  terminal ──────►  cli.rs  ──┐                       │
  browser  ──────►  web/     ──┼──►  services  ◄──────┘
  telegram ──────►  bot/     ──┘        │
                                        ├─ source/   (gallery-dl, folder, t.me)  → NormalizedPost
                                        ├─ jobs/     (import → blob → db, worker)
                                        ├─ render/   (caption from a template)
                                        ├─ nsfw      (policy decision)
                                        ├─ media/    (album planning, video poster)
                                        ├─ queue/    (schedule, selection, lease)
                                        ├─ tg/       (transport + publish pipeline)
                                        └─ storage/  (mirror to local/webdav/telegram)
                                                │
                          db/ (SQLite) ─────────┴──────── blob.rs (content-addressed files)
```

Everything hangs off one shared `AppState` (`src/state.rs`):

```rust
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub blobs: Arc<BlobStore>,
    pub renderer: Arc<Renderer>,
    pub transport: Option<Arc<dyn Transport>>,
    pub(crate) media_jobs: Arc<tokio::sync::Notify>,
}
```

`Config` (`src/config.rs`) provides defaults for `[telegram]`, `[web]`, `[bot]`,
`[access]`, `[import]`, `[tag_suggestions]`, `[limits]`, `[proxy]`, and `[storage]`.
See [.env.example](.env.example) for environment overrides. `AppState::open` creates
the home layout, opens and migrates the DB, and builds the blob store and renderer.

## Data model (SQLite, schema v24 via `PRAGMA user_version`)

| migration | tables and views | purpose |
|---|---|---|
| `0001_init.sql` | `authors`, `posts`, `blobs`, `media`, `tags`, `post_tags`, `tag_aliases`, `tag_rules` | archive posts unique on `(source, source_id)`, media linked by blob hash, and shared normalised tags |
| `0002_queue.sql` | `channels`, `queue`, `jobs`, `sends`, `settings` | per-channel schedule and publish queue (`UNIQUE(post_id, channel_id)`), background jobs, send audit log, and key/value state |
| `0003_bot.sql` | `users`, `usage` (+ `channels.alias/discovered`, `posts.submitted_by`, `jobs.submitted_by`) | bot roles, daily quotas, channel handles, who submitted a post |
| `0004_storage.sql` | `mirrors` | per-`(post, sink)` upload state (`pending/done/failed`, external id, bytes, error) |
| `0005_media_hygiene.sql`, `0006_media_meta.sql` | `media` | per-item sensitivity and raw source metadata |
| `0007_tag_provenance.sql`, `0014_post_grid_reads.sql` | `post_tags`, `post_tag_removals`, `visible_post_tag_links`, `active_post_tags` | source and manual tag provenance, persistent source-tag removals, and archive grid indexes |
| `0008_queue_alerts.sql` | `channels`, `queue` | whether empty-queue and unconfirmed-send alerts have been handled |
| `0009_watched_sources.sql`, `0011_watch_enabled.sql` | `watched_sources`, `watched_items` | feed checkpoints, pending downloads, and pause/resume state |
| `0010_duplicate_artwork.sql` | `image_fingerprints`, `duplicate_matches`, `duplicate_dismissals` | still-image indexing and duplicate review |
| `0012_guest_reputation.sql` | `reputation_*`, `posts`, `queue` | guest suggestions, score events, publication identities, and delivery destinations |
| `0013_tag_suggestions.sql`, `0017_tagger_character_names.sql` | `post_tag_suggestion*`, `tag_franchise_associations`, `tag_character_names` | tag recommendations, review state, image evidence, and local catalog mappings |
| `0015_media_merges.sql`, `0016_media_compressions.sql`, `0019_gallery_visibility.sql` | `media_merges`, `media_merge_sources`, `media_merge_members`, `media_compressions`, `media_hidden_sources` | original panels, explicit merge membership, JPEG derivatives, and outgoing visibility |
| `0018_post_text_edits.sql` | `posts` | flags that preserve title and description edits across re-imports |
| `0020_gallery_order.sql` | `media_gallery_order` | preferred artwork order keyed by source blob hash and occurrence |
| `0021_duplicate_index_recovery.sql` | `duplicate_fingerprint_failures` | backoff for unreadable artwork and an index for exact duplicate lookup |
| `0022_delivery_links.sql` | `sends` | snapshot Telegram destinations for links to confirmed deliveries |
| `0023_daily_watches.sql` | `watched_sources` | record the start of the last check and backfill older watch activity |
| `0024_pixiv_watch_state.sql` | `watched_pixiv_seen`, `watched_sources`, `watched_items` | native Pixiv watch baseline, seen artwork IDs, and pending artwork absence counts |

Blob bytes live at `blobs/<aa>/<blake3>.<ext>` and thumbnails at
`thumbs/<aa>/<blake3>.webp`. Identical files across posts are stored once. SQLite
tracks the blobs and their media references. The upsert path ensures each referenced
blob has a catalogue row.

## Concurrency model

* **One connection, one mutex.** `Db::call(|conn| …)` runs the closure on `spawn_blocking`
  (async callers), `Db::with` runs it inline (CLI/tests). Closures are `'static`: clone what
  they capture. SQLite uses WAL and waits up to `busy_timeout` on lock contention.
* **Long-running loops.** `run` (`src/cli.rs`) starts the web server and bot by default.
  The web server owns the job worker. With `--no-web`, `run` starts the worker directly.
  `scheduler_pass` runs every 15 s by default. `serve` runs the web server and worker.
* **Background work.** The worker polls publish and storage jobs, imports and watched
  checks, gallery edits, tag suggestions, and duplicate indexing independently. The
  polling interval is 2 s. New gallery jobs also wake the gallery worker immediately.
  Imports and watched checks share `[import] max_concurrent`. Watches and tag suggestions
  use file locks in `staging/` to coordinate across processes.
* **Single-flight publishing.** The queue lease admits only one sender per item.
  `queue::lease::acquire_for_gallery` checks the outgoing gallery revision in the same
  transaction that acquires the lease.

## Flow 1 — import

The CLI, bot and web use `jobs::import_target` or its progress-reporting variant
`import_target_tracked`. Telegram forwards go directly to `jobs::import_posts`.

1. `source::fetch_target` routes the target. A `t.me/…` link → `source::telegram::fetch`
   (Bot API: forward the post into a chat, read file IDs, download, delete the copy, rebuild
   album siblings from neighbouring message IDs). If forwarding fails for a public
   link, `[import] public_telegram_fallback` permits a single-image embed fallback.
   With `[import] pixiv_backend = "native"`, Pixiv artwork imports and creator watches
   use the Pixiv client in `crates/nujijushi-sources`. The app stages originals and
   maps typed artwork data into `NormalizedPost`. Ugoira routes to gallery-dl.
   With `[import] twitter_backend = "native"` and an X session file, individual
   X status URLs use the same crate's signed-in post client. X profile watches
   use gallery-dl.
   Individual RedNote note links use the native RedNote client. It resolves app
   share links and reads the note page, then stages the complete gallery or
   direct video and archives it under the RedNote note ID.
   Other targets go through the `Source` registry. `gallerydl` stages URLs with
   `--write-metadata` and passes site cookies and the configured proxy. `localdir`
   reads folders.

   What counts as an importable target lives here as well: `source::looks_importable`
   and `source::normalize_target` are the only vocabulary, and the bot/CLI ask them
   instead of re-deriving "looks like a link".
2. Each importer returns `NormalizedPost`s: source, source id, author, title/description,
   tags, rating, `files` staged on disk. `source/normalize.rs` maps the messy per-site JSON
   (pixiv `x_restrict`, twitter `possibly_sensitive`, booru `rating`, …) into `Rating`.
   The author fields are the trap: they are **source-specific and two of them disagree**.
   gallery-dl's `extractor/twitter.py` writes `user.name` = *screen name* and `user.nick` =
   display name (`"name": core["screen_name"], "nick": core["name"]`), while pixiv's
   `user.account` is the handle and `user.name` the display name. And with the *default*
   `metadata-user = false` gallery-dl passes the raw GraphQL user through instead:
   `user.core.screen_name`, `user.core.name`, `user.rest_id`. So `normalize.rs` keeps two
   source-aware candidate lists, accepts a twitter handle only when it passes
   `looks_like_screen_name` (`[A-Za-z0-9_]{1,50}`, optional `@` — a display name like
   `tamanegi@FANBOX` can therefore never become the handle), and falls back to the
   `@handle` in the post URL. A display name that is just the handle is dropped
   (`same_handle`). All of it happens **at import time**: a stored row keeps the author it
   was written with, so a fix needs a re-import.
3. `jobs::store_post` ingests the files, generates WebP thumbnails or ffmpeg video
   posters when available, and calls `db::posts::store_import`. That transaction
   upserts the author and post, refreshes source tags and media, and preserves manual
   edits and matching gallery settings. New operator imports start `approved` and
   guest and watched imports start `new`. A hard-blocked tag sets the state to
   `rejected`. With guest reputation enabled, a recognized suggestion can attach to
   an existing post without replacing its archive data.
4. If `[storage] on_import` and a sink is enabled, `storage::enqueue` inserts `pending`
   mirror rows. Uploading happens later in the worker (flow 5).
5. `ImportOutcome` carries post IDs, created and updated counts, skips, and any guest
   suggestion or gallery reset details. Import jobs submitted through the bot DM their
   result to the submitter.

## Flow 2 — publishing (the important one)

```
scheduler (15 s tick)
  └─ channels: enabled && !paused && is_due && within active hours && under daily cap
       └─ publish_channel(state, channel, dry_run=false, force=false)
            1. queue_db::list(channel, state=queued, page size 60)
            2. load the outgoing gallery and its revision for each candidate
                 rejected post or policy veto → queue_db::skip_with_reason
            3. history: recent authors + recent tags → queue::selection::Selection
                 pinned first, else weighted random with artist cooldown and tag penalty
            4. build_plan: render caption (minijinja + 1024 clipping) and plan messages
                 (media::plan_messages: ≤10 per album, videos separate, caption on the first item)
            5. execute_plan:
                 dry_run → return the plan (that is `push --dry-run`)
                 lease::acquire_for_gallery → send_plan (retry-after honoured,
                                               transient backoff, permanent fails fast)
                 success → queue::mark_sent + channels::record_success(next_due) + audit
                 failure → retryable with no message IDs: lease released (stays queued)
                           permanent with no message IDs: state=failed
                           partial delivery: state=unknown, manual resolution required
                           all failed attempts increment the channel failure counter
```

`publish_item(state, queue_id, dry_run)` applies the same policy and lease to a
specific queued item and bypasses the schedule. Three consecutive failed attempts
pause the channel. On `run` startup, stale `sending` leases older than 600 s become
`unknown`. An operator can confirm delivery or explicitly requeue the whole post.

`flush_channel` uses the same per-item publishing path for a snapshot of current
queued rows. It skips policy vetoes and stops on a delivery error. A staging file
lock excludes other sends for that channel during the flush. Each item still takes
its own gallery lease, and each successful send updates the next scheduled slot.

### The projection (estimated times)

`plan_ahead` (`src/tg/publish.rs`) reads up to 200 queued items, filters their outgoing
galleries through `nsfw::evaluate`, and simulates selection using a stable channel seed.
`scheduler::upcoming_slots` estimates times using the interval, half the configured
jitter, active hours, and the daily cap. `project_queue` returns `{queue_id, post_id, slot}`
and `preview_channel` uses the same projection to render captions. Actual sends select
from the live queue with a fresh random draw, so preview order is an estimate.

Four surfaces read it, all formatting through `publish::slot_label` (local `HH:MM` today,
`Tue HH:MM` within a week, `MM-DD HH:MM` beyond): the queue page's `est.` column, the post
page's per-channel badge (via `db::queue::for_post`), the preview page, and the CLI
(`queue ls`, `preview`). Paused and disabled channels have no scheduled ETA. While
scheduling is off, the web preview shows estimated slots for an enabled schedule
and renders the held first post when present.

`scheduler_pass` (src/cli.rs) also writes `settings['scheduler.tick']` on every pass.
The dashboard and `GET /api/status` expose the heartbeat as
`{last_tick, age_secs, age, stale}`. The dashboard shows a banner when the heartbeat
is missing or older than 120 s. `serve` leaves scheduling off.
The scheduler, CLI `push`, Telegram `/publish`, and the web worker all use `publish_channel`.

`POST /api/v1/publish` queues `publish_channel` or `publish_item` and returns JSON.
The worker executes the send (see Flow 3).

Caption rendering lives in `render/`: `build_context` prepares safe HTML values (author,
description, hashtags, source link, nsfw/spoiler flags), `Renderer::render` applies the
channel's template, `truncate::fit_caption` clips to 1024 characters and closes any tag it
cut through. The bundled Default, Compact, and Detailed presets live in
`templates/captions/`. A channel can also supply its own inline template.

## Flow 3 — the web UI

Axum serves the React admin shell at the browser URLs and JSON at `/api/v1/*`.
The Vite bundle is embedded when built before Cargo. Set `NUJIJUSHI_REQUIRE_WEBUI=1`
to require that bundle at build time. A Rust-only build still starts the HTTP server
and job worker. Known admin pages return 503 with build instructions. Assets,
metrics, and the Telegram webhook have their own routes.

`[web] base_path` mounts the whole router under one prefix. Axum strips the
mount before dispatch, and the shell passes the prefix to React for browser
URLs. Admin JSON and form paths stay relative to the app root.

The React app loads its CSRF token from `GET /api/v1/session` using the HttpOnly
session cookie. Sign-in uses a short-lived cookie-bound challenge. Protected API
requests with an expired session return 401. Admin writes are URL-encoded,
validate CSRF and return an action message, optional location and any new job IDs.
Media and thumbnails are served from the authenticated blob routes. The post grid
uses `/thumb/{hash}`. Browsers can reuse thumbnails for a day in their private
cache, keyed by URL and session cookie. On revalidation, the ETag identifies the
actual WebP bytes. The post gallery loads full-resolution still images from
`/media/{hash}` and uses thumbnails for video posters.

```
POST /api/v1/publish                     → publish a channel or queue item
POST /api/v1/channels/{id}/flush         → publish the channel's queued posts in succession
POST /api/v1/storage/drain               → upload pending mirrors
POST /api/v1/storage/cleanup             → apply a confirmed cleanup preview
POST /api/v1/import                      → import one job per target
POST /api/v1/jobs/retry                  → retry a failed import in a new job
POST /api/v1/watches/{id}/check          → check a watched source
POST /api/v1/posts/{id}/tag-suggestions  → suggest tags
POST /api/v1/posts/{id}/media            → reorder artwork, merge or compress images
```

Job submissions return 202 with job IDs. Publish, storage drain, and import retry
requests deduplicate matching active jobs. A fresh import submission creates one
job per target. The worker dispatches each job and records its progress. The JSON read handlers
and `GET /api/v1/jobs/{id}/status` share `JobView`. React polls active jobs and
refreshes the current data on completion. While a channel queue is visible,
React polls `GET /api/v1/channels/{id}/queue/status` for a revision so sends from
the scheduler or another tab appear. `GET /api/status` remains session-gated.

The grid (`db::posts::card_select`) is one projection with three shared rules: nsfw
(`Rating::nsfw_sql`), delivered (`db::queue::DELIVERED_STATES`) and how a submitter is named
(`submitter_sql` → `@handle`, first name, else the ID). `/posts` hides sent and rejected
posts by default. Use `?sent=all` to include sent posts, `?sent=yes` for sent posts
only, and `?state=all` to include rejected posts. The CLI lists all states unless
filtered. React appends batches from `/api/v1/posts/cards` with an import-time and
ID cursor. The card badge identifies Telegram submissions from guests.

### Curation and gallery edits

* `web/setup.rs` places the first post in `held` for preview. Confirming the preview
  releases it and enables scheduling. `web/routes.rs` verifies the bot's
  posting rights when creating a channel or changing its Telegram destination.
* `jobs/watch.rs` handles manual and automatic feed checks. The worker queues enabled
  watches on startup and as they become due, 24 hours after the last attempt started.
  Checks record discoveries and the checkpoint before downloading so later checks can
  retry pending items. After its first complete scan, a native Pixiv watch compares the
  creator index with stored artwork IDs and requests summaries only for newly visible
  IDs. Two complete scans that omit a pending ID defer its retries until it reappears.
  `duplicates.rs` indexes still images in batches for the duplicate inbox.
* `jobs/tag_suggestions/` sends still images to FoidFinder on loopback. The optional
  Danbooru fallback resolves character names and franchise metadata. Reviews,
  supporting image positions, and catalog mappings are stored in SQLite.
* `jobs/media_merge.rs` creates a merged image or compressed derivative. The gallery
  edit and job success commit together. `db::media_merges::outgoing_post` applies
  hidden items and active derivatives for publishing, mirrors, and share exports.
  The archive retains source blobs for undo and full backups. Re-imports preserve
  matching edits and report gallery resets when source files change.
* Batch compression uses the existing gallery-edit job kind and stages each JPEG
  before committing the gallery in one transaction. Storage cleanup keeps the
  active gallery, drops undo and unsaved tag-suggestion records for eligible sent
  posts, and rechecks blob references before unlinking files.

The webhook endpoint is separate from the UI: `POST /tg/webhook` checks
`X-Telegram-Bot-Api-Secret-Token` in constant time, builds a `bot::Bot` and hands the body to
`handle_webhook_body`. It returns 200 on success, 400 for a bad update, 500 for an
internal handler error, and 503 when the bot is unavailable. Bad secrets get 403.

## Flow 4 — the Telegram bot

`Bot::run` refreshes private command menus, then either polls `getUpdates` (offset persisted in
`settings['bot.offset']`, a leftover webhook is deleted first) or registers the webhook and
lets the HTTP endpoint feed it. Both paths converge on `Bot::handle`:

```
update → Message?
  ignore bots and non-private chats
  access::resolve(user)      config admins → DB role → config guests → public → refuse
  refresh this chat's command menu from the same rules as /help and dispatch
  forwarded post with media → source::telegram::from_forwarded → jobs::import_posts
  else commands::parse → dispatch checks permission → handler checks any quota → reply
update → my_chat_member (became admin of a channel)
  channels::upsert_discovered (alias slugged, enabled = false) + DM every config admin
```

Replies are HTML, escaped, chunked to Telegram's 4096 limit (`bot/reply.rs`).
Import links become `jobs` rows (`submitted_by` set); the worker DMs the outcome back.
A **guest** batch also DMs the admins (`bot::admin_recipients`: config admins plus users the
bot promoted, deduped, minus the submitter) with the handle, the count and the links — and
the card grid identifies the submitter. With `[bot] guest_reputation` enabled,
review verdicts, rediscoveries, and known publications can create score events.
The notification monitor claims events before sending guest DMs.

## Flow 5 — storage mirrors

`storage::sinks(state)` builds the enabled sinks from config. `enqueue` writes `pending`
rows; `storage::drain` (worker, `storage sync`, dashboard button) takes pending/failed rows,
builds the shared payload with `storage::plan` — `<source>/<artist>/<source_id>/NN.ext` plus
a `post.json` sidecar — and calls `Sink::put`, recording `done` with an external ID or
`failed` with the reason. Local and WebDAV sinks write the layout. The Telegram sink
sends media groups or individual items to a backup channel and records message IDs.
New mirror uploads use the current outgoing gallery, as does `export --share`.
Completed mirrors keep the media from their original upload. `export --with-media`
backs up the database and full blob store, including source images used for undo.

## Flow 6 — configuration and health

`nujijushi init` writes `config.toml` (0600) + opens the DB. `doctor` is the health report:
directory, schema, channels, discovered channels, bot users, external tools, cookies, tokens,
access switches, storage sinks, proxy, then online checks (getMe, per-channel admin rights,
webhook vs polling) and the `--strict` exit code.

## Error model

`crate::error::Error` (`Io`, `Db`, `Json`, `Config`, `NotFound`, `Invalid`, `Template`,
`Image`, `Telegram`, `Process`, `Timeout`, `Join`, `Other`) with a `Result<T>` alias used
everywhere. The Telegram transport uses `TgResult<T>` = `Result<_, TgError>`, where
`TgError` is `RateLimited { retry_after }` / `Transient` / `Permanent`; `TgError::classify`
decides from the Bot API message, `is_retryable` drives the retry policy.

## Traps already paid for

* `Bot::copy_message` returns `MessageId`, not `Message` — use `forward_message` when you
  need the media file ids (we delete the copy right after).
* `InputMedia` is an enum of structs: set `caption`, `parse_mode`, `has_spoiler` on the
  variant, and put the caption on **one** item only or clients break the album layout.
* `Message::photo()` returns every size; the last one is the largest.
* A `webhook` on the token makes `getUpdates` fail (409); two pollers do the same. Polling
  deletes a stale webhook at startup.
* `rand::rng()` is not `Send`: never hold it across `.await` (it broke a spawned publish once).
* `Db::call` closures must be `'static` — clone ids/strings before the `move`.
* SQLite foreign keys are on: media rows require their blob row (upsert inserts it defensively).
* Captions are clipped after rendering, so the clipping code must close open HTML tags.
* Insta snapshots: `INSTA_UPDATE=always cargo test --test captions` to regenerate, then read
  the diff before committing.

## Test map

| file | covers |
|---|---|
| unit tests in each module | parsing, layout, scheduling math, truncation, access matrix, sinks |
| `tests/flow.rs` | import → queue → publish, incl. idempotency, partial delivery, `publish_item` |
| `tests/bot.rs` | roles, commands, discovery, t.me ingest, quotas, forwarding |
| `tests/web.rs` | session/CSRF, `/api/status`, `/api/v1/*` admin actions and jobs, gallery edits, watches, duplicate review |
| `tests/webhook.rs` | Telegram webhook authentication and update delivery |
| `tests/storage.rs` | sinks vs. a real WebDAV server, proxy routing, backfill, failures |
| `tests/captions.rs` + `tests/snapshots/` | the exact caption text per channel configuration |
| `webui/tests/admin.spec.ts` | React workflows, queue updates, gallery edits, full-resolution images and cached grid thumbnails, and responsive layout |
