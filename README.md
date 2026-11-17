# nujijushi

[![ci](https://github.com/ZZU1U/nujijushi/actions/workflows/ci.yml/badge.svg)](https://github.com/ZZU1U/nujijushi/actions/workflows/ci.yml)

A single Rust binary that keeps a **local artwork archive** and publishes it to your
**Telegram channels on a schedule**, with tidy captions built from the post's artist,
description, tags and NSFW state.

```
URLs / folders ──▶ gallery-dl ──▶ blob store + SQLite ──▶ queue ──▶ caption + album ──▶ Bot API
                                        ▲                              ▲
                                   admin UI (axum)               scheduler (interval, jitter,
                                                                  active hours, daily cap)
```

* **Import** — `gallery-dl` (Pixiv, Twitter/X, Danbooru, ArtStation, …) or a folder on disk.
* **Store** — content-addressed blobs (identical files stored once) + thumbnails, SQLite metadata.
* **Curate** — approve/reject, fix ratings, add or block tags, queue per channel — CLI or web UI.
* **Publish** — per-channel interval, active hours in the channel's timezone, daily cap,
  artist cooldown, spoilers for NSFW, album chunking, retry/backoff, and a lease protocol
  so a restart never double-posts.

---

## Quick start

```bash
# 1. create the data directory (~/.nujijushi, or $NUJIJUSHI_HOME)
nujijushi init
#    → prints the UI token; put your bot token in config.toml or export TG_BOT_TOKEN

# 2. add the bot as an *administrator* of each channel, then register it
#    (`channel add <alias> <@name|id>` — the alias is what the bot and CLI use)
nujijushi channel add night-shift @mychannel \
      --interval 180 --active-from 20:00 --active-to 01:00 --tz Europe/Berlin \
      --nsfw spoil

# 3. import something (URL or a folder with images)
nujijushi import https://www.pixiv.net/artworks/123456
nujijushi import ~/Pictures/dump/artist-set-01

# 4. look at it
nujijushi ls --state new
nujijushi show 1 --channel 1        # renders the exact caption for that channel

# 5. queue + preview + dry run
nujijushi queue add 1 2 3 --channel 1
nujijushi preview 1 --count 10
nujijushi push --channel 1 --dry-run

# 6. run: scheduler + admin UI + Telegram bot
nujijushi run
```

Adding the bot as an admin of a channel **later** makes nujijushi notice it by itself
(`my_chat_member` update) and register it *disabled* under a slugged alias:

```bash
nujijushi channel discover          # → night-shift-2   Night Art   -1001234567890   waiting for enable
nujijushi channel enable night-shift-2
```

`nujijushi run` keeps publishing while it lives. For a permanent setup, start it from
`launchd`/`systemd` or a tmux session. The admin UI is at <http://127.0.0.1:8788/>.

---

## Commands

| command | what it does |
|---|---|
| `nujijushi init` | create home dir, `config.toml`, database |
| `nujijushi doctor [--offline] [--strict]` | check tooling, cookies, token, channel admin rights, disk |
| `nujijushi import <url\|dir>… [--channel C]` | import; optionally queue straight into a channel |
| `nujijushi ls [--state/--source/--tag/--nsfw/--search]` | browse the archive |
| `nujijushi show <id> [--channel C]` | details + rendered caption |
| `nujijushi tag <id> <tag>… [--remove]` · `rate` · `nsfw auto\|yes\|no` | curation |
| `nujijushi approve/reject <id>…` · `rm <id>` | post state |
| `nujijushi channel add <alias> <@name\|id>` | register a channel (validated with `getChat`) |
| `nujijushi channel ls\|edit\|rm\|pause\|resume` | channel + schedule management |
| `nujijushi channel discover\|enable\|disable` | channels the bot was added to, waiting for a name |
| `nujijushi queue add\|ls\|skip\|pin\|reset\|rm` | queue management |
| `nujijushi preview <channel> [--count N]` | what the next N slots would publish |
| `nujijushi push --channel C [--dry-run] [--force]` | publish one post now |
| `nujijushi run [--once] [--tick 15] [--no-web] [--no-bot]` | scheduler + UI + bot |
| `nujijushi serve [--bind ADDR]` | admin UI only |
| `nujijushi bot [--once]` | Telegram command interface only |
| `nujijushi users ls\|grant\|ban` | who may talk to the bot, and their roles |
| `nujijushi rule add <tag> block\|nsfw` · `rule ls` · `rule rm` | content rules |
| `nujijushi alias set <from> <to>` | tag aliasing for future imports |
| `nujijushi log [--limit N]` | send audit log |
| `nujijushi gc [--dry-run]` | delete unreferenced blobs and thumbnails |
| `nujijushi export [--out DIR] [--with-media]` | backup: DB snapshot + JSONL + channels.json (+ blobs) |

Global: `--home DIR` (or `$NUJIJUSHI_HOME`), `-v/-vv` for more logging.

---

## Configuration

`$NUJIJUSHI_HOME/config.toml` (see [config.example.toml](config.example.toml)):

```toml
[telegram]
bot_token = "123456:ABC…"      # or TG_BOT_TOKEN

[web]
bind  = "127.0.0.1:8788"
token = "…"                     # or NUJIJUSHI_UI_TOKEN

[import]
gallery_dl = "gallery-dl"
ffmpeg     = "ffmpeg"
timeout_secs = 1800

[limits]
caption_chars   = 1024
max_upload_bytes = 52428800    # Bot API hard limit is 50 MB

[bot]
enabled = true
poll_timeout_secs = 30

[access]
admins = [123456789]        # or NUJIJUSHI_ADMINS=123,456  (find yours with /whoami)
guests = []
public = false              # true: anyone who writes to the bot becomes a guest
allow_import  = true        # guest switches
allow_browse  = false
allow_queue   = false
allow_publish = false       # off even for admins (Telegram never publishes unless you say so)
allow_curate  = false
allow_nsfw_browse = false
import_quota_per_day = 20
queue_quota_per_day  = 20

[import]
public_telegram_fallback = true   # t.me embed scrape when the bot cannot read a channel
```

Environment overrides `TG_BOT_TOKEN`, `NUJIJUSHI_UI_TOKEN`, `NUJIJUSHI_BIND`,
`NUJIJUSHI_HOME`, `NUJIJUSHI_GALLERY_DL`, `NUJIJUSHI_ADMINS`, `NUJIJUSHI_BOT`, `NUJIJUSHI_BOT_MODE`,
`NUJIJUSHI_WEBHOOK_URL`, `NUJIJUSHI_WEBHOOK_SECRET`. `config.toml` is written with mode `0600`.

Data layout:

```
$NUJIJUSHI_HOME/
├── config.toml          # 0600
├── nujijushi.sqlite3       # metadata (WAL: posts, channels, queue, users, usage)
├── blobs/aa/<hash>.png  # content-addressed media
├── thumbs/aa/<hash>.webp
├── cookies/             # pixiv.txt, twitter.txt (Netscape format)
├── staging/             # transient import dirs
└── export/              # backups
```

---

## Cookies (only when a site needs them)

Export cookies for the site in Netscape format (e.g. with the "Get cookies.txt"
browser extension) into `$NUJIJUSHI_HOME/cookies/`:

```
cookies/pixiv.txt      cookies/twitter.txt     cookies/danbooru.txt
cookies/gelbooru.txt   cookies/artstation.txt  cookies/deviantart.txt
cookies/bluesky.txt    cookies/tumblr.txt      (or <host>.txt)
```

`nujijushi doctor` reports each cookie file's age — refresh anything older than ~30 days.
Public content (many sites) imports fine without cookies.

---

## Talking to the bot

`nujijushi run` also long-polls Telegram, so admins (and, if you allow it, guests) can drive
the archive from a chat. `--no-bot` or `[bot] enabled = false` turns just this off.

| command | guests | admins | notes |
|---|---|---|---|
| *send a link* / *forward a post* | `allow_import` | ✓ | the normal path, no command needed |
| `/import <link>` | `allow_import` | ✓ | same pipeline as the CLI |
| `/ls [n]` · `/show <id>` | `allow_browse` | ✓ | NSFW is hidden from guests unless `allow_nsfw_browse` |
| `/push <alias> [id]` | `allow_queue` | ✓ | **queues only, never publishes** |
| `/queue <alias>` · `/channels` | `allow_browse` | ✓ | depth, next slot, reason |
| `/publish <alias>` | — | `allow_publish` | publishes one now, consumes a slot (off by default) |
| `/approve` `/reject` `/nsfw` `/rate` `/tag` | `allow_curate` | ✓ | curation |
| `/pause` `/resume` `/enable` `/disable` | — | ✓ | schedule control |
| `/discover` | — | ✓ | channels found via `my_chat_member` |
| `/users` · `/grant <id> admin\|guest\|banned` · `/ban <id>` | — | ✓ | runtime roles (config admins stay admins) |
| `/start` `/help` `/whoami` `/status` | ✓ | ✓ | `/help` only lists what you may use |

Roles: config `[access] admins` → database role → config `guests` → `public` (auto-register)
→ refuse. `banned` always wins. Guests are quota-limited (`usage` table, per day);
admins are exempt. The interface only answers in **private chats**; group chatter is ignored.

Importing a **Telegram post** (`t.me/<name>/<id>` or `t.me/c/<id>/<n>`) works when the bot is
a member of that channel: the post is forwarded into your own chat, the files are downloaded,
and the copy is deleted. Albums are reconstructed from the neighbouring message ids.
Constraints are Telegram's: 20 MB per file, and a public channel the bot is not in can only be
scraped from its `?embed=1` page — one image, reduced size, marked
`public embed (degraded)` on the post. Real fidelity for arbitrary channels needs MTProto,
which nujijushi does not do.

### Polling or webhook

By default nujijushi asks Telegram for updates (`getUpdates`, long polling) — no public
address needed. If you would rather have Telegram push to you, switch the mode:

```toml
[bot]
mode = "webhook"
webhook_url = "https://bot.example.com"   # public HTTPS base URL
# webhook_secret = "optional; generated and stored in the database when empty"

[web]
bind = "127.0.0.1:8788"                   # stays loopback: the proxy talks to it
```

`nujijushi run` then registers `https://bot.example.com/tg/webhook` with Telegram and stops
polling; the endpoint lives on the admin web server and authenticates each delivery with
the `X-Telegram-Bot-Api-Secret-Token` header (checked in constant time), so it works without
the UI cookie while the UI itself still requires it. You need a TLS-terminating proxy in
front of the bind address, e.g.

```
bot.example.com {
    reverse_proxy 127.0.0.1:8788    # Caddy: TLS is automatic
}
```

Switching back to `polling` deletes the webhook on the next start (a leftover webhook would
otherwise swallow every update), and `nujijushi doctor` reports a mismatch between the
registered URL and your config. Webhook mode requires the web server — `nujijushi run --no-web`
refuses to start that way.

---

## Scheduling

Each channel has: `interval_minutes`, `posts_per_slot` (1), `active_from`/`active_to`
(in the channel `tz`, may wrap midnight), `daily_cap` (0 = off), `jitter_secs`,
`artist_cooldown`.

Every tick (~15 s) nujijushi looks for channels where *all* of these hold:
enabled, not paused, `next_due_at <= now`, inside the active window, under the daily cap.
It then picks one post:

1. **pinned** items first (highest priority, oldest first),
2. otherwise weighted random, where **priority** raises the weight and posts sharing tags
   with the last few sends are penalised — plus the **artist cooldown**: the last
   *K* sends must not repeat the same artist (relaxed only if that would starve the queue).

After a send the next slot is `now + interval ± jitter`, pushed into the next active
window if it would land outside it. `nujijushi preview` simulates the same selection with a
per-channel seed, so what you see is what will go out.

### Publishing is idempotent

Before touching the API, the queue row is leased (`state='sending'`). Outcomes:

* **success** → `sent`, message ids recorded, `sends` audit row, `next_due_at` advanced.
* **permanent error** (no rights, chat not found, bad entities) → `failed`, channel
  failure counter +1; **3 consecutive failures auto-pause the channel**.
* **transient error** after retries (4 attempts, backoff honouring `retry_after`) →
  back to `queued` for the next slot.
* **partial delivery** (some messages of a multi-album post went out, then a failure) and
  **interrupted mid-send** (lease older than 10 min) → `unknown`. These are **never
  re-sent automatically**; the dashboard shows them and you click *delivered* or
  *not delivered*.

---

## NSFW handling

Three layers, in order:

1. **Hard blocks** (never publish, never even queued): `nujijushi rule add <tag> block`,
   or a blocked author (`authors.blocked`).
2. **Rating** — taken from site metadata (`pixiv x_restrict`, booru `rating`, Twitter
   `possibly_sensitive`), refined by `rule … nsfw` tags and tag heuristics, and
   overridable per post (`nujijushi nsfw <id> yes|no`, `rate <id>`).
3. **Channel policy** — `--nsfw allow` (post normally), `spoil` (Bot API `has_spoiler`
   blur + `#nsfw` tag), `reject` (skipped with a reason).

Channel `blocked_tags`/`required_tags` gate posts at enqueue **and** again at send time,
so policy edits apply to the existing queue. Posts vetoed at send time become `skipped`
with the reason in `last_error`.

---

## Captions

Rendered with minijinja, HTML parse mode, hard-clipped to 1024 characters (tags are
closed if the text is cut). Default layout:

```html
<b>Sample Artist</b> (@sample)

A quiet street at dusk, hand painted.

#original #landscape #pixiv

via <a href="https://www.pixiv.net/artworks/1">Pixiv</a>
```

Per-channel template (UI → channel → *caption template*, or `--template-file`):

```jinja
<b>{{ author.name }}</b>{% if author.handle %} ({{ author.handle }}){% endif %}
{{ post.description | truncate_words(180) }}
{{ hashtags }}
```

Context: `post.{id,source,source_id,url,title,description,rating,date,media_count}`,
`author.{name,handle,url}`, `channel.{id,title,tg_id}`, `tags`, `hashtags`,
`source_label`, `source_link`, `attribution_inline`, `attribution_footer`, `nsfw`, `spoiler`.

Filters: `truncate_words(n)`, `hashtags`, `tag_links(base)`, `one_line`.
Autoescaping is on — `{{ source_link }}` is the only pre-built HTML.

Per-channel options: `--tag-style hashtags|plain|none`, `--max-tags N`,
`--attribution footer|link|none`, `--include-source-tag`, `--silent`, `--protect`.
Multiline text is kept; lines that are nothing but hashtags are stripped at import.

---

## Admin UI

`nujijushi run` (or `nujijushi serve`) serves it on `127.0.0.1:8788`. Auth is a single
token (`[web] token`) exchanged for an HttpOnly cookie; forms carry a CSRF token.
Binding to a non-loopback address is possible but logs a warning.

Pages: dashboard (queue depth, next slot per channel, unconfirmed sends, recent sends,
import jobs) · posts grid with filters + bulk approve/reject/NSFW/queue/delete ·
post detail with caption previews per channel · channel list + create · channel editor
with live next-slot preview · ordered queue with pin/skip/requeue/remove ·
next-slots preview · import box with job progress · **users** (roles, quotas, ban/promote).

**Publishing on demand** is available from the UI, not just the CLI:
* dashboard → **publish next** per channel — sends the next queued post,
* a channel's queue page → **publish** per item — sends exactly that post.

Both bypass the schedule (no waiting for the slot, active hours or daily cap) and ask for
confirmation; the channel's NSFW rules, the media grouping and the lease/idempotency
protections still apply, and a successful manual send advances `next_due_at` exactly like a
scheduled slot. CLI equivalents: `nujijushi push --channel <alias> --force`.

---

## Telegram limits nujijushi respects

* ≤ 10 items per media group; larger posts are split into several albums.
* Videos are never mixed with photos in one album (Telegram limitation) — they go out
  as their own message with an ffmpeg-generated poster.
* Caption ≤ 1024 chars, upload ≤ 50 MB, download ≤ 20 MB.
* `429 retry_after` is honoured; retries use exponential backoff with jitter.

---

## Troubleshooting

| symptom | cause / fix |
|---|---|
| `gallery-dl produced no media` | wrong URL form, `Cloudflare challenge`, or login required → add `cookies/<site>.txt` and retry |
| `[danbooru][error] ChallengeError` | Cloudflare: cookies are mandatory for boorus |
| `not enough rights to send photos` | bot is not an admin with "post messages" in that channel |
| `chat not found` | wrong id; use the numeric `-100…` id for private channels |
| nothing published | `nujijushi channel ls` shows the reason: paused, outside active hours, daily cap, next slot |
| channel paused itself | 3 consecutive send failures — fix the cause, then `nujijushi channel resume` |
| unconfirmed send after a crash | check the channel, then resolve it in the dashboard |

---

## Development

CI runs on every push and pull request (`.github/workflows/ci.yml`: `fmt`, `clippy -D warnings`,
`test` on Linux + macOS); a `v*` tag builds release binaries for linux/macos x86_64+aarch64
and attaches them to the GitHub release (`.github/workflows/release.yml`).

```bash
cargo test                     # 204 tests: units, snapshots, end-to-end flows, bot interface, webhook
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Layout: `blob` (content-addressed store) · `db` (SQLite, migrations in `migrations/`) ·
`source` (gallery-dl / folder importers, metadata normalisation) · `nsfw` (policy engine) ·
`render` (caption templates + truncation) · `media` (album planning, video posters) ·
`queue` (scheduler, selection, leases) · `tg` (transport trait, Bot API impl, mock, publish,
bot-api shapes) · `bot` (access, commands, handlers, replies) · `source/telegram.rs`
(t.me ingest) · `web` (axum UI) · `jobs` (import worker + submitter notifications).

The `Transport` trait keeps publishing testable: `tg::teloxide_impl` talks to Telegram,
`tg::mock` records calls, so the flows (lease/idempotency, spoilers, policy vetoes,
partial delivery) are covered without a network.

## Scope

Deliberately **not** here: MTProto (a user account) for reading arbitrary channels and
subscriber-only guest gating, Telegram's own scheduled messages, video re-encoding,
per-user web accounts, perceptual dedupe.

Respect the artists and the platforms: this tool is for archiving and re-posting content
you have the right to share, and `rule add <tag> block` / blocked authors are there to
keep things out.
