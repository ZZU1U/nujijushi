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
    pub cfg: Arc<Config>,                        // parsed config.toml + env
    pub db: Db,                                  // Arc<Mutex<Connection>>
    pub blobs: Arc<BlobStore>,                   // blake3 store + thumbnails
    pub renderer: Arc<Renderer>,                 // minijinja for captions
    pub transport: Option<Arc<dyn Transport>>,   // None without a bot token
}
```

`Config` (src/config.rs) is the single source of defaults: `[telegram] [web] [bot]
[access] [import] [limits] [proxy] [storage]`, with env overrides (`NUJIJUSHI_*`,
`TG_BOT_TOKEN`). `AppState::open` creates the home layout, opens/migrates the DB and
builds the blob store and renderer.

## Data model (SQLite, schema v4 via `PRAGMA user_version`)

| migration | tables | purpose |
|---|---|---|
| `0001_init.sql` | `authors`, `posts`, `blobs`, `media`, `tags`, `post_tags`, `tag_aliases`, `tag_rules` | the archive. `posts` is unique on `(source, source_id)`; `media` rows point at a blob hash; tags are normalised and shared |
| `0002_queue.sql` | `channels`, `queue`, `jobs`, `sends`, `settings` | per-channel schedule + the publish queue (`UNIQUE(post_id, channel_id)`), import jobs, the audit log, and key/value state (`bot.offset`, `bot.webhook_secret`) |
| `0003_bot.sql` | `users`, `usage` (+ `channels.alias/discovered`, `posts.submitted_by`, `jobs.submitted_by`) | bot roles, daily quotas, channel handles, who submitted a post |
| `0004_storage.sql` | `mirrors` | per-`(post, sink)` upload state (`pending/done/failed`, external id, bytes, error) |

Blobs are files, not rows of data: `blobs/<aa>/<blake3>.<ext>` with a `thumbs/<aa>/<blake3>.webp`
next to it. Identical files across posts are stored once. `db/posts::upsert` self-heals the
`blobs` catalogue so a media row can never dangle.

## Concurrency model

* **One connection, one mutex.** `Db::call(|conn| …)` runs the closure on `spawn_blocking`
  (async callers), `Db::with` runs it inline (CLI/tests). Closures are `'static`: clone what
  they capture. WAL + `busy_timeout` keep a second process from deadlocking.
* **Long-running loops**: `run` (src/cli.rs) spawns the web server (which spawns the job
  worker), the bot interface, and — when the web UI is off — its own worker. A 15 s tick calls
  `scheduler_pass`; the worker polls every 2 s. Ctrl-C stops the tick loop.
* **Single-flight publishing** comes from the queue lease, not from the loop: two processes
  may run, only one wins `queue::lease::acquire`.

## Flow 1 — import

`jobs::import_target(state, target, submitted_by)` is the entry point for CLI, bot and web:

1. `source::fetch_target` routes the target. A `t.me/…` link → `source::telegram::fetch`
   (Bot API: forward the post into a chat, read file ids, download, delete the copy, rebuild
   album siblings from neighbouring message ids). Anything else → the `Source` registry:
   `gallerydl` for URLs (staged with `--write-metadata`), `localdir` for folders.

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
3. `jobs::import_posts` stores each one (`store_post`): hash + copy into the blob store →
   thumbnail (webp) and, for videos, an ffmpeg poster → `post_db::upsert` (author upsert,
   post upsert, source tags replaced, media rows rewritten) → `submitted_by` →
   hard-blocked tags (`tag_rules` `block`) auto-reject the post.
4. If `[storage] on_import` and a sink is enabled, `storage::enqueue` inserts `pending`
   mirror rows. Uploading happens later in the worker (flow 5).
5. The caller gets an `ImportOutcome` (ids, created/updated counts, skipped reasons). The bot
   turns that into a DM to the submitter.

## Flow 2 — publishing (the important one)

```
scheduler (15 s tick)
  └─ channels: enabled && !paused && is_due && within active hours && under daily cap
       └─ publish_channel(state, channel, dry_run=false, force=false)
            1. queue_db::list(channel, state=queued, limit 60)
            2. per candidate: post_db::get → nsfw::evaluate
                 verdict Reject → queue_db::skip_with_reason (post stays archived)
            3. history: recent authors + recent tags → queue::selection::Selection
                 pinned first, else weighted random with artist cooldown and tag penalty
            4. build_plan: render caption (minijinja + 1024 clipping) and plan messages
                 (media::plan_messages: ≤10 per album, videos isolated, one caption)
            5. execute_plan:
                 dry_run → return the plan (that is `push --dry-run`)
                 lease::acquire  →  send_plan (retry policy: retry-after honoured,
                                    transient backoff, permanent fails fast)
                 success → queue::mark_sent + channels::record_success(next_due) + audit
                 failure → retryable: lease released (stays queued)
                           permanent: state=failed, channel failure counter +1
                                      (3 in a row auto-pauses the channel)
                           partial/ambiguous: state=unknown, never re-sent
```

`publish_item(state, queue_id, dry_run)` is the same tail for one specific queue row; it
ignores the schedule but not the policy.

### The projection (estimated times)

`plan_ahead` (src/tg/publish.rs) is the read-only twin of `publish_channel`: the same
candidate load, the same `nsfw::evaluate` filter, the same seeded `Selection::simulate`
(`channel.id ^ 0x5eed`), so the order it returns is the order the scheduler would take.
`scheduler::upcoming_slots` then gives each item a time: interval (`+ half of jitter`, the
mean of the uniform draw), pushed into the active window, skipping to the next local day
when `daily_cap` is reached. `project_queue` exposes that as `{queue_id, post_id, slot}`;
`preview_channel` builds on the same function (it adds the rendered caption), so a preview
and an ETA cannot disagree.

Four surfaces read it, all formatting through `publish::slot_label` (local `HH:MM` today,
`Tue HH:MM` within a week, `MM-DD HH:MM` beyond): the queue page's `est.` column, the post
page's per-channel badge (via `db::queue::for_post`), the preview page, and the CLI
(`queue ls`, `preview`). A paused or disabled channel projects nothing, which is exactly
what "nothing goes out" means.

`scheduler_pass` (src/cli.rs) also writes `settings['scheduler.tick']` on every pass.
`serve` starts the web UI **and** the job worker, but not this loop, so the timestamp is the
only honest way to tell "nothing is due" from "nothing is running": `queue::scheduler::
tick_age` turns it into a `scheduler` block (`{last_tick, age_secs, age, stale}`) that
`/api/status` returns and the dashboard renders as a banner once it is older than
`STALE_SECS`.
`publish_channel` is used by the scheduler, `push`, `/publish` and the web actions.

Inside the web UI nothing calls either directly: `POST /publish` writes a `publish_channel`
or `publish_item` job row and redirects, and the worker runs it (see Flow 3).

Caption rendering lives in `render/`: `build_context` prepares safe HTML values (author,
description, hashtags, source link, nsfw/spoiler flags), `Renderer::render` applies the
channel's template, `truncate::fit_caption` clips to 1024 characters and closes any tag it
cut through. Templates: `templates/captions/default.html.j2` plus per-channel inline ones.

## Flow 3 — the web UI

Axum serves the React admin shell at the browser URLs and JSON at `/api/v1/*`.
The Vite bundle is embedded when built before Cargo. A Rust-only build still
starts the HTTP server and job worker. Known admin pages return 503 with build
instructions. Assets, metrics and the Telegram webhook have their own routes.

The React app loads its CSRF token from `GET /api/v1/session` using the HttpOnly
session cookie. Sign-in uses a short-lived cookie-bound challenge. Protected API
requests with an expired session return 401. Admin writes are URL-encoded,
validate CSRF and return an action message, optional location and any new job IDs.
Media and thumbnails are served from the authenticated blob routes.

```
POST /api/v1/publish        → enqueue a publish job              → 202
POST /api/v1/storage/drain  → enqueue a storage upload job       → 202
POST /api/v1/import         → enqueue one import job per target  → 202
POST /api/v1/jobs/retry     → enqueue a new failed import retry  → 202
```

`enqueue_unique` refuses a duplicate while the same kind and payload is active.
The worker dispatches the job and records its progress. The JSON read handlers
and per-job status endpoint share `JobView`. React polls active jobs and refreshes
the current data on completion. While a channel queue is open, its status endpoint
also supplies a revision so sends from the scheduler or another tab appear.
The existing `GET /api/status` endpoint remains session-gated.

The grid (`db::posts::card_select`) is one projection with three shared rules: nsfw
(`Rating::nsfw_sql`), delivered (`db::queue::DELIVERED_STATES`) and how a submitter is named
(`submitter_sql` → `@handle`, first name, else the id). `/posts` defaults to
`SentFilter::NotSent` — a curated archive is mostly "what is still to publish" — and
`?sent=all|yes` brings the sent ones back or shows only them; the CLI keeps `Any`. A post a
non-admin submitted gets an amber `via @handle` badge (`submitter_guest`), which is false for
a post with no submitter at all.

The webhook endpoint is separate from the UI: `POST /tg/webhook` checks
`X-Telegram-Bot-Api-Secret-Token` in constant time, builds a `bot::Bot` and hands the body to
`handle_webhook_body`, answering 200 / 400 (bad update) / 500 (internal, Telegram retries).

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
the card grid badges that submitter, because a guest can add to the archive but never
invisibly.

## Flow 5 — storage mirrors

`storage::sinks(state)` builds the enabled sinks from config. `enqueue` writes `pending`
rows; `storage::drain` (worker, `storage sync`, dashboard button) takes pending/failed rows,
builds the shared payload with `storage::plan` — `<source>/<artist>/<source_id>/NN.ext` plus
a `post.json` sidecar — and calls `Sink::put`, recording `done` with an external id or
`failed` with the reason. `Sink` implementations: `local` (copy), `webdav` (MKCOL + PUT via
the shared proxy-aware client), `telegram` (album to a backup channel).

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
| `tests/web.rs` | publish/drain queued as jobs, duplicate refusal, `/api/status`, `/jobs` |
| `tests/storage.rs` | sinks vs. a real WebDAV server, proxy routing, backfill, failures |
| `tests/captions.rs` + `snapshots/` | the exact caption text per channel configuration |
