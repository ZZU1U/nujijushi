# Usage reference

[Back to the quickstart](../README.md)

## Commands

| command | what it does |
|---|---|
| `nujijushi init` | create home dir, `config.toml`, database |
| `nujijushi doctor [--offline] [--strict]` | check tooling, cookies, token, channel admin rights, disk |
| `nujijushi import <url\|dir>… [--channel C] [--priority N]` | import, with optional queue destination and priority |
| `nujijushi ls [--state/--source/--tag/--nsfw/--search] [--limit N]` | browse the archive |
| `nujijushi show <id> [--channel C] [--raw]` | details + rendered caption; `--raw` dumps the stored source metadata, post-level and per item |
| `nujijushi tag <id> <tag>… [--remove]` · `rate <id> <sfw\|questionable\|explicit>` · `nsfw <id> <auto\|yes\|no>` | curation |
| `nujijushi approve/reject <id>…` · `rm <id>` | post state |
| `nujijushi approve --points N ID` | approve one guest suggestion with a 5–20 point reward |
| `nujijushi channel add [alias] <@name\|id> [--no-verify]` | register a channel, checking `getChat` when a bot token is available |
| `nujijushi channel ls\|edit\|rm\|pause\|resume` | channel + schedule management |
| `nujijushi channel discover\|enable\|disable` | channels the bot was added to, waiting for a name |
| `nujijushi queue add\|ls\|skip\|pin\|reset\|rm` | queue management |
| `nujijushi preview <channel> [--count N]` | what the next N slots would publish |
| `nujijushi push --channel C [--dry-run] [--force]` | publish one post now |
| `nujijushi flush --channel C` | publish the channel’s queued posts in succession, bypassing its schedule |
| `nujijushi run [--once] [--tick 15] [--no-web] [--no-bot] [--bind ADDR]` | scheduler + UI + bot |
| `nujijushi serve [--bind ADDR]` | admin UI and its background jobs |
| `nujijushi bot [--once]` | Telegram command interface only |
| `nujijushi users ls\|grant\|ban` | who may talk to the bot, and their roles |
| `nujijushi rule add <tag> block\|nsfw` · `rule ls` · `rule rm` | content rules |
| `nujijushi alias set <from> <to>` | tag aliasing for future imports and new manual tags |
| `nujijushi tagger map <character-id> <franchise> [--name LABEL]` | set a local override and optionally bind the FoidFinder label for offline lookups |
| `nujijushi tagger unmap <character-id>` | remove your override and use the catalog or cache again |
| `nujijushi tagger mappings` | list effective mappings, IDs, and their origins |
| `nujijushi log [--limit N]` | send audit log |
| `nujijushi storage ls\|test\|sync` | list, check, or backfill [storage mirrors](operations.md#external-storage-mirrors) |
| `nujijushi gc [--dry-run]` | delete unreferenced blobs and thumbnails |
| `nujijushi gc --sent [--older-than-days N] [--yes]` | preview or apply storage cleanup for confirmed sent posts |
| `nujijushi export [--out DIR] [--with-media]` | backup: DB snapshot + JSONL + channels.json (+ blobs) |
| `nujijushi export --share --out DIR` | export outgoing artwork and its `post.json` sidecars |

Global: `--home DIR` (or `$NUJIJUSHI_HOME`), `-v/-vv` for more logging.

When pasting a batch of import links, `%0A` and `%0D%0A` between URLs work as line separators in the admin UI, Telegram bot, and CLI.

New posts imported directly through the CLI, the admin UI, or by a Telegram admin start **Approved**. Guest submissions and watched-source discoveries start **New**. A hard-blocked tag takes priority and sets the post to **Rejected**.

## Channel discovery

When the running bot becomes a channel admin, nujijushi registers the channel as disabled and assigns it an alias. List discovered channels with `nujijushi channel discover`, then enable one with `nujijushi channel enable <alias>`.

For a channel the bot already administers, you can also register it directly with `nujijushi channel add art -1001234567890` or `nujijushi channel add art @yourchannel`.

## Scheduling

Set the interval, time zone, active hours, daily cap, jitter, and artist cooldown per channel. Active hours can span midnight. The scheduler picks pinned posts first and uses priority, recent tags, and artist history to choose among the rest.

Use `nujijushi preview <alias>` or the channel’s **preview** page to see what the scheduler is likely to send. Queue or policy changes can affect the result. `nujijushi run` starts the scheduler. `nujijushi serve` starts the admin UI and its background jobs.

Three consecutive failed publishing attempts pause a channel. A partial delivery is marked unconfirmed immediately. On `nujijushi run` startup, a send left in progress for more than 10 minutes is also marked unconfirmed. Check Telegram, then use **Confirm delivered** or **Requeue entire post** on the dashboard or channel queue. Confirming delivery accepts an optional Telegram message ID. Requeueing sends the entire post on the next attempt.

`push --dry-run` renders the selected payload without sending it. It can still mark policy-vetoed queue items as skipped. Use `preview` to inspect the queue without changing it.

See the [publishing flow](../codebase.md#flow-2--publishing-the-important-one) for selection, leases, and retries.

## NSFW handling

The policy checks these conditions in order.

1. **Hard blocks** (never publish, never even queued): `nujijushi rule add <tag> block`,
   or a blocked author (`authors.blocked`).
2. **Channel gates** check blocked and required tags and the number of outgoing media items.
3. **Rating** — taken from site metadata (`pixiv x_restrict`, booru `rating`, Twitter
   `possibly_sensitive`), refined by `rule … nsfw` tags and tag heuristics, and
   overridable per post (`nujijushi nsfw <id> yes|no`, `rate <id>`).
4. **Channel policy** — `--nsfw allow` (post normally), `spoil` (Bot API `has_spoiler`
   blur + `#nsfw` tag), `reject` (skipped with a reason).

The same policy runs at enqueue and send time. Posts vetoed at send time become
`skipped` with the reason in `last_error`.

## Captions

Rendered with minijinja, HTML parse mode, hard-clipped to 1024 characters (tags are
closed if the text is cut). Default layout:

```html
<b>Sample Artist</b> (sample)

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
`author.{label,given,handle,mention,url}`, `channel.{id,title,tg_id}`, `tags`, `hashtags`,
`source_label`, `source_link`, `attribution_inline`, `attribution_footer`, `nsfw`, `spoiler`.
`author.label` is the name, else the handle, else "unknown artist"; `author.given` is the
real display name only (empty for a handle-only account), and the duplicate `(handle)` is
dropped when the two are the same word. `author.name` is kept as an alias of `label`.
`author.handle` has **no** `@`: in Telegram `@name` is a *mention*, which pings whoever owns
that username (and a twitter/pixiv handle is not a Telegram username at all), so the caption
carries the bare word. `author.mention` is the `@handle` form, for a template that really
wants to ping someone.

Filters: `truncate_words(n)`, `hashtags`, `tag_links(base)`, `one_line`.

Generated Telegram hashtags turn punctuation such as brackets into word boundaries. For example, `character_name_(franchise_name)` becomes `#character_name_franchise_name`. Use the `hashtags` template filter for the same formatting in custom captions.
Text values are HTML-escaped. `{{ source_link }}` supplies the prebuilt attribution
link, and `tag_links(base)` renders escaped HTML links for the supplied tags.

Per-channel options include `--tag-style hashtags|plain|none`, `--max-tags N`,
`--attribution footer|link|none`, `--include-source-tag true`, `--silent true`, and
`--protect true`. Enable the source tag to append tags such as `#twitter` or `#pixiv`.
Multiline text is kept, but `#hashtags` and `@mentions` are stripped at import, runs of blank
lines collapse to one, and a body longer than 1000 characters is cut with `…`.

## Admin UI

`nujijushi run` (or `nujijushi serve`) serves it on `127.0.0.1:8788`. Auth is a single
token (`[web] token`) exchanged for an HttpOnly cookie; forms carry a CSRF token.
Binding to a non-loopback address is possible but logs a warning.

Before upgrading an existing archive, stop older nujijushi workers and take a full backup with `nujijushi export --with-media --out <backup-directory>`. The new binary applies pending SQLite migrations when it opens the archive.

Pages: dashboard (queue depth, next slot per channel, unconfirmed sends, recent sends,
recent jobs) · posts grid with filters + bulk approve/reject/NSFW/queue/delete (sent and
rejected posts are hidden by default; `?sent=all` and `?state=all` bring them back;
state-coloured badges, a guest submitter is named) · post detail with caption previews per
channel and the total size of the media · channel list + create · channel editor
with live next-slot preview · ordered queue with pin/skip/requeue/remove ·
next-slots preview · import box with job progress · **jobs** (every action, live) ·
**users** (roles, quotas, ban/promote).

When you add a channel or change its Telegram destination in the UI, it verifies that the destination is a broadcast channel and the bot has permission to post there. The dashboard's first-run setup can hold the first queued post for caption preview before you start scheduling.

On **Posts**, filters apply as you change them and more cards load as you scroll. **Not queued** shows new and approved artwork that has no queue assignment or recorded delivery. Type in **Search tags** to find matching archive tags, with the most-used matches first. Open **Tags** to browse popular tags and see the active filter. Use **Settings → Default queue channel** to preselect a destination for web queue actions.

Click a post card to select it, or click its post number to open it. **Select all loaded posts** selects the cards currently on screen and in the loaded feed.

The post gallery loads full-resolution images as you scroll. Click an image to open the file on its own in a new tab. To keep full-resolution media unloaded during browsing, enable **Settings → Use thumbnails for media previews**; previews then use the archive thumbnails for images and videos, while clicking still opens the original. This preference is stored in the browser, not in the archive.

The **Post details** panel autosaves edits. Clear the title or description to remove its text. Edits survive re-imports, and future captions use the saved text wherever the channel template includes it.

Use **Add a tag** to pick a matching archive tag or enter a new one. Remove tags with the × on each chip. Hover over a chip to see whether it came from the source, a manual edit, or both. Removing a source tag keeps it hidden after re-import. Restore it under **Removed source tags** if the most recent import still supplied it.

Drag artwork tiles to reorder them, or choose **Move earlier** and **Move later** from a tile's corner menu. The saved order applies to future Telegram sends, mirrors, and share exports. Use the same menu to hide or restore an item in outgoing copies.

Use **Include in merge** in the corner menu to select still images of the same height, then choose **Merge side by side** above the gallery. **Undo merge** restores the source panels. Re-imports preserve the order and visibility of matching images and retain a merge when every source panel is still present.

Channel caption presets include **Default**, **Compact**, and **Detailed**. Compact shows title, credit, tags, and source attribution in fewer lines. Detailed includes an available description and artist handle. Both use the channel’s tag and attribution settings.

Use `nujijushi export --share --out <new-directory>` to copy outgoing images and their `post.json` files. The regular backup retains all original images and the visibility settings.

Open **Compress image** in a tile's corner menu or **Merge output options** when stitching panels. **Use exact JPEG quality** keeps the pixel dimensions and uses the quality you enter. **Fit to Telegram** starts with a quality ceiling of 85 and can lower it or reduce the dimensions to fit the photo limits. It may add white borders to very wide or tall images. JPEG fills transparency with white. **Undo compression** restores the original image or merged PNG.

Use **Compress all images** in the **Artwork** header to apply the same settings to the gallery in one job. It includes hidden still images and merged panels, and skips video and animated images. Each compressed image has its own **Undo compression** action.

`GET /metrics` provides Prometheus text metrics for build information, posts, channels,
queue depth, recent jobs, and storage mirrors. It is accessible without a session.
Bind the UI to loopback or restrict this path at the proxy if the port is reachable from outside.

**Publishing on demand** is available from the dashboard and the queue.

* dashboard → **publish next** per channel — sends the next queued post,
* a channel's queue page → **publish** per item — sends exactly that post.

Both ask for confirmation and bypass the schedule, active hours, and daily cap. The channel’s NSFW rules, media grouping, and lease protections still apply. A successful manual send advances the channel’s next slot. The CLI equivalent of **publish next** is `nujijushi push --channel <alias> --force`.

The request queues a background job. Open **jobs** to see progress or retry a failed import. Imports default to two concurrent jobs, configurable with `[import] max_concurrent`. See the [web UI flow](../codebase.md#flow-3--the-web-ui) for job lifecycle and API details.

### Flush a channel queue

Open a channel’s **queue**, choose **Flush queue**, and confirm the Telegram destination. The queue page shows the background job and its progress. From the CLI, run `nujijushi flush --channel <alias>` and wait for the result.

The flush takes the items already queued when it starts and publishes them one after another. Pinned items go first, then higher priority, then oldest first. The flush bypasses the interval, active hours, daily cap, and the channel’s paused or disabled schedule. Channel content policy still applies. Held previews and unconfirmed deliveries require their usual review.

The flush stops on a send failure after the usual Telegram retries. The queue retains the remaining posts. A partial delivery stays unconfirmed. Successful sends advance the next scheduled slot as usual, and posts queued after the flush starts wait for a regular slot or another manual action. If a publish is already in progress when the flush tries to start, let it finish and try again.

### Storage cleanup

In **Settings**, use **Preview cleanup** to see the number of eligible posts, unsaved tag suggestions, and estimated disk space. The default is 30 days since the latest confirmed send. Cleanup skips posts with outstanding deliveries, mirrors, gallery edits, or tag suggestion jobs.

**Apply cleanup** keeps the gallery you see, including hidden images, and removes the original files and intermediate versions kept only for undo. Saved tags, send history, and files shared with other posts are retained. Re-import from the source to download the originals and replace the compacted gallery. Plain `gc` can retry failed file removals.

The CLI equivalents are `nujijushi gc --sent --older-than-days 30` to preview and `nujijushi gc --sent --older-than-days 30 --yes` to apply. Set the age to `0` to include recently confirmed sends.

### Watched sources and duplicate artwork

Use **Watched sources** to save a Pixiv artist page, an X or Bluesky artist media feed,
or a newest-first Danbooru or Gelbooru tag search. Use **Check now** to start each
background check. The first check uses the configured post limit for that watch. Later checks
look for newly published artwork and retry pending downloads. Use **Pause** to block
new checks and **Resume** to keep checking from the same point. Already queued or
running checks can finish. Artist and search access
uses the same gallery-dl configuration and credentials as regular imports. Review new
posts and queue the ones you want to publish.

Open **Review duplicates** from **Posts** to see exact files shared by distinct posts, including posts from the same source.
Exact matches appear as soon as the files are archived. The worker also checks still JPEG, PNG, and WebP images for
likely resize or recompression matches across sources. It processes the oldest pending images first.
The inbox shows pending and failed visual checks. Failed checks retry automatically, and **Retry failed checks**
makes them eligible immediately. **Dismiss pair** hides a suggestion even after a re-import.

## Telegram media limits

* ≤ 10 items per media group; larger posts are split into several albums.
* The publisher sends videos individually and uses an ffmpeg-generated poster when available. The first item carries the post caption.
* Captions are capped at 1024 characters. Imports warn when a file exceeds `[limits] max_upload_bytes` (50 MiB by default). Telegram imports cap downloads at 20 MiB per file.
* **Fit to Telegram** targets the 10,000,000-byte photo limit, a maximum combined width and height of 10,000 pixels, and a maximum aspect ratio of 20:1.
* `429 retry_after` is honoured; retries use exponential backoff with jitter.
