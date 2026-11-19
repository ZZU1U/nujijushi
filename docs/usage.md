# Usage reference

[Back to the quickstart](../README.md)

## Commands

| command | what it does |
|---|---|
| `nujijushi init` | create home dir, `config.toml`, database |
| `nujijushi doctor [--offline] [--strict]` | check tooling, cookies, token, channel admin rights, disk |
| `nujijushi import <url\|dir>… [--channel C]` | import; optionally queue straight into a channel |
| `nujijushi ls [--state/--source/--tag/--nsfw/--search]` | browse the archive |
| `nujijushi show <id> [--channel C] [--raw]` | details + rendered caption; `--raw` dumps the stored source metadata, post-level and per item |
| `nujijushi tag <id> <tag>… [--remove]` · `rate` · `nsfw auto\|yes\|no` | curation |
| `nujijushi approve/reject <id>…` · `rm <id>` | post state |
| `nujijushi channel add <alias> <@name\|id>` | register a channel (validated with `getChat`) |
| `nujijushi channel ls\|edit\|rm\|pause\|resume` | channel + schedule management |
| `nujijushi channel discover\|enable\|disable` | channels the bot was added to, waiting for a name |
| `nujijushi queue add\|ls\|skip\|pin\|reset\|rm` | queue management |
| `nujijushi preview <channel> [--count N]` | what the next N slots would publish |
| `nujijushi push --channel C [--dry-run] [--force]` | publish one post now |
| `nujijushi run [--once] [--tick 15] [--no-web] [--no-bot]` | scheduler + UI + bot |
| `nujijushi serve [--bind ADDR]` | admin UI and its background jobs |
| `nujijushi bot [--once]` | Telegram command interface only |
| `nujijushi users ls\|grant\|ban` | who may talk to the bot, and their roles |
| `nujijushi rule add <tag> block\|nsfw` · `rule ls` · `rule rm` | content rules |
| `nujijushi alias set <from> <to>` | tag aliasing for future imports |
| `nujijushi log [--limit N]` | send audit log |
| `nujijushi gc [--dry-run]` | delete unreferenced blobs and thumbnails |
| `nujijushi export [--out DIR] [--with-media]` | backup: DB snapshot + JSONL + channels.json (+ blobs) |

Global: `--home DIR` (or `$NUJIJUSHI_HOME`), `-v/-vv` for more logging.

## Channel discovery

When the running bot becomes a channel admin, nujijushi registers the channel as disabled and assigns it an alias. List discovered channels with `nujijushi channel discover`, then enable one with `nujijushi channel enable <alias>`.

## Scheduling

Set the interval, time zone, active hours, daily cap, jitter, and artist cooldown per channel. Active hours can span midnight. The scheduler picks pinned posts first and uses priority, recent tags, and artist history to choose among the rest.

Use `nujijushi preview <alias>` or the channel’s **preview** page to see what the scheduler is likely to send. Queue or policy changes can affect the result. `nujijushi run` starts the scheduler. `nujijushi serve` starts the admin UI and its background jobs.

Three consecutive permanent send failures pause a channel. If a send is interrupted or partially delivered, the dashboard marks it unconfirmed. Check the channel before resolving the item in the dashboard. Unconfirmed items require manual resolution.

See the [publishing flow](../codebase.md#flow-2--publishing-the-important-one) for selection, leases, and retries.

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

## Captions

Rendered with minijinja, HTML parse mode, hard-clipped to 1024 characters (tags are
closed if the text is cut). Default layout:

```html
<b>Sample Artist</b> (@sample)

A quiet street at dusk, hand painted.

#original #landscape

via <a href="https://www.pixiv.net/artworks/1">Pixiv</a>
```

Per-channel template (UI → channel → *caption template*, or `--template-file`):

```jinja
<b>{{ author.label }}</b>{% if author.given and author.handle %} ({{ author.handle }}){% endif %}
{{ post.description | truncate_words(180) }}
{{ hashtags }}
```

Context: `post.{id,source,source_id,url,title,description,rating,date,media_count}`,
`author.{label,given,handle,url}`, `channel.{id,title,tg_id}`, `tags`, `hashtags`,
`source_label`, `source_link`, `attribution_inline`, `attribution_footer`, `nsfw`, `spoiler`.
`author.label` is the name, else the `@handle`, else "unknown artist"; `author.given` is the
real display name only (empty for a handle-only account), and the duplicate `(@handle)` is
dropped when the two are the same word. `author.name` is kept as an alias of `label`.

Filters: `truncate_words(n)`, `hashtags`, `tag_links(base)`, `one_line`.
Autoescaping is on — `{{ source_link }}` is the only pre-built HTML.

Per-channel options: `--tag-style hashtags|plain|none`, `--max-tags N`,
`--attribution footer|link|none`, `--include-source-tag`, `--silent`, `--protect`.
The source tag is **off by default** (no `#twitter`/`#pixiv` appended); pass
`--include-source-tag` to bring it back per channel.
Multiline text is kept, but `#hashtags` and `@mentions` are stripped at import, runs of blank
lines collapse to one, and a body longer than 1000 characters is cut with `…`.

## Admin UI

`nujijushi run` (or `nujijushi serve`) serves it on `127.0.0.1:8788`. Auth is a single
token (`[web] token`) exchanged for an HttpOnly cookie; forms carry a CSRF token.
Binding to a non-loopback address is possible but logs a warning.

Pages: dashboard (queue depth, next slot per channel, unconfirmed sends, recent sends,
recent jobs) · posts grid with filters + bulk approve/reject/NSFW/queue/delete (sent and
rejected posts are hidden by default; `?sent=all` and `?state=all` bring them back;
state-coloured badges, a guest submitter is named) · post detail with caption previews per
channel and the total size of the media · channel list + create · channel editor
with live next-slot preview · ordered queue with pin/skip/requeue/remove ·
next-slots preview · import box with job progress · **jobs** (every action, live) ·
**users** (roles, quotas, ban/promote).

`GET /metrics` answers in the Prometheus text format (posts by state and nsfw, channels by
enabled, queue depth per channel/state, import jobs, storage mirrors). It sits outside the
cookie gate because a scraper has no session and the document carries counts only — bind the
UI to loopback, or block the path in your proxy if the port is reachable from outside.

**Publishing on demand** is available from the dashboard and the queue.

* dashboard → **publish next** per channel — sends the next queued post,
* a channel's queue page → **publish** per item — sends exactly that post.

Both ask for confirmation and bypass the schedule, active hours, and daily cap. The channel’s NSFW rules, media grouping, and lease protections still apply. A successful manual send advances the channel’s next slot. The CLI equivalent of **publish next** is `nujijushi push --channel <alias> --force`.

The request queues a background job. Open **jobs** to see progress or retry a failed import. Imports default to two concurrent jobs, configurable with `[import] max_concurrent`. See the [web UI flow](../codebase.md#flow-3--the-web-ui) for job lifecycle and API details.

## Telegram limits nujijushi respects

* ≤ 10 items per media group; larger posts are split into several albums.
* The publisher sends videos separately from photo albums and uses an ffmpeg-generated poster when available.
* Caption ≤ 1024 chars, upload ≤ 50 MB, download ≤ 20 MB.
* `429 retry_after` is honoured; retries use exponential backoff with jitter.
