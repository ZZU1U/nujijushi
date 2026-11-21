# Operations guide

[Back to the quickstart](../README.md)

## Configuration

`nujijushi init` creates `config.toml` in `~/.nujijushi`. Set `NUJIJUSHI_HOME` to use a different directory. See [config.example.toml](../config.example.toml) for core settings.

You can override the data directory, Telegram and web settings, gallery-dl path,
and proxy settings with environment variables. `init` writes `config.toml` with mode
`0600`. See [.env.example](../.env.example) for the full list of overrides.

Data layout:

```
$NUJIJUSHI_HOME/
├── config.toml           # 0600
├── nujijushi.sqlite3     # archive metadata and operational state (SQLite WAL)
├── blobs/aa/<hash>.ext   # content-addressed originals and derivatives
├── thumbs/aa/<hash>.webp
├── cookies/             # site cookies in Netscape format
├── staging/             # transient imports and worker lock files
└── export/              # backups
```

## Cookies (only when a site needs them)

Export cookies for the site in Netscape format (e.g. with the "Get cookies.txt"
browser extension) into `$NUJIJUSHI_HOME/cookies/`:

```
cookies/pixiv.txt      cookies/fanbox.txt      cookies/twitter.txt
cookies/danbooru.txt   cookies/gelbooru.txt    cookies/artstation.txt
cookies/deviantart.txt cookies/bluesky.txt     cookies/tumblr.txt
```

For these supported sites, the importer looks up the site filename first and then
falls back to a file named after the exact host, such as `cookies/x.com.txt`.
For `fanbox.cc` and creator subdomains, export the cookies from your signed-in
FANBOX session to `cookies/fanbox.txt`. The export must include `FANBOXSESSID`.

`nujijushi doctor` reports each cookie file's age — refresh anything older than ~30 days.
Public content (many sites) imports fine without cookies.

## Talking to the bot

`nujijushi run` starts the Telegram command interface in polling mode by default.
Admins and authorized guests can use it from a private chat. Use `--no-bot` or
`[bot] enabled = false` to skip starting the bot interface from `run`.

### Queue alerts

Set `queue_alerts = true` under `[bot]` to get DMs about empty queues and unconfirmed sends. The feature defaults to `false` and uses the configured Telegram bot token. Each recipient in `[access] admins` must start a private chat with the bot first. `nujijushi doctor` checks the local prerequisites.

An enabled, unpaused channel gets an empty-queue alert when it has no queued posts or sends in flight. Refilling the queue allows a new alert when it drains again. On first use, the bot can alert about queues that are already empty and sends still awaiting review. Review unconfirmed sends in the admin dashboard. The process logs DM failures and attempts each alert once to avoid duplicates after a restart.

### Bot commands

Telegram's command picker and `/help` show the commands enabled for your role. Start a private chat with the bot to get your menu.

| command | guests | admins | notes |
|---|---|---|---|
| *send a link* / *forward a post* | `allow_import` | ✓ | the normal path, no command needed; several links in one message all import (up to 20) |
| `/import <link>…` | `allow_import` | ✓ | same pipeline as the CLI; one quota unit per link |
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
→ refuse. For everyone but a config admin, `banned` always wins. Guests are quota-limited
(`usage` table, per day); admins are exempt.

**A config admin is always an admin**, whatever the `users` row says — messaging the bot
auto-registers a row as `guest`, so `users ls`, `/users` and the web page print the
*effective* role (`admin … (config admin; db: guest)`) via one helper
(`bot::access::display_role`). Editing `[access] admins` needs a restart (`/grant` is live).
The interface only answers in **private chats**; group chatter is ignored.
Every guest submission is also **DM'd to the admins** (who submitted, how many links, which),
and the post carries an amber `via @handle` badge in the grid — a guest can add to the
archive, but never invisibly.

For Telegram links (`t.me/<name>/<id>` or `t.me/c/<id>/<n>`), the bot forwards the
post into the submitter's private chat, or the first configured admin's chat for
CLI and web imports. It downloads files up to 20 MiB, deletes the temporary
messages, and reconstructs albums from neighbouring message IDs. CLI and web
imports therefore need a bot token and a configured admin who has started a private
chat. If a public post cannot be forwarded, `[import] public_telegram_fallback`
allows the importer to try its `?embed=1` page. This produces one reduced-size image
marked `public embed (degraded)`. Private posts require Bot API access to the source.

### Guest reputation

Set `guest_reputation = true` under `[bot]` to start counting new guest suggestions. The default is off. Telegram's `/whoami` shows a signed balance for guests and **∞** for admins. The bot DMs guests about changes, including the reason and resulting balance.

- **Approved suggestion** earns 10 points total by default. An admin can choose a total of 5–20 in the post detail form, with `/approve --points N ID`, or with `nujijushi approve --points N ID`. An admin queueing a new post approves it at the default reward.
- **Rejected suggestion** costs 3 points total. Both decisions split the total among the distinct guests who suggested the post before the first review. A changed verdict adjusts the same shares. Later suggestions leave that split intact.
- **Rediscovery** earns 2 points for the first guest to suggest an unpublished, admin-added post, even if it has already entered the queue.
- **Published duplicate** costs 1 point once per guest and includes a link to the Telegram message. Private post links still require access to the destination. Unconfirmed sends and confirmations without message IDs carry no penalty.

With scoring enabled, guests need admin approval before they can queue a post and admins finalize scored reviews. Disabling the setting pauses scores and DMs and retains balances. Enabling it does not award points for old reviews.

### Polling or webhook

By default, nujijushi uses long polling (`getUpdates`) over an outbound connection. Use webhook mode if your host can receive HTTPS from Telegram on port 443, 80, 88, or 8443. Behind CGNAT or a router that cannot forward those ports, keep `mode = "polling"`. Polling supports `[proxy]`.

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
the `X-Telegram-Bot-Api-Secret-Token` header. Use a TLS-terminating reverse proxy in front of the bind address. The Caddy example forwards the webhook path and returns 404 elsewhere.

```
bot.example.com {
    handle /tg/webhook {
        reverse_proxy 127.0.0.1:8788
    }
    handle {
        respond 404
    }
}
```

The nginx example uses the same routing on port 8443.

```nginx
server {
    listen 8443 ssl;
    server_name bot.example.com;
    ssl_certificate     /etc/nginx/ssl/bot.example.com.crt;
    ssl_certificate_key /etc/nginx/ssl/bot.example.com.key;
    ssl_protocols TLSv1.2 TLSv1.3;

    location = /tg/webhook {                 # exact match: do not rewrite the path
        proxy_pass http://127.0.0.1:8788;    # 127.0.0.1, not localhost
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_buffering off;
    }
    location / { return 404; }
}
```

Include a non-default port in `webhook_url` (for example, `https://bot.example.com:8443`). A `curl -X POST` to `/tg/webhook` without the secret header should receive **403** from nujijushi.

Switching back to `polling` deletes the webhook on the next start (a leftover webhook would
otherwise swallow every update), and `nujijushi doctor` reports a mismatch between the
registered URL and your config. Webhook mode requires the web server — `nujijushi run --no-web`
refuses to start that way.

## Behind a proxy

```toml
[proxy]
url = "socks5h://127.0.0.1:1080"   # or http://10.0.0.5:8080
no_proxy = "localhost,127.0.0.1"   # LAN storage stays direct
```

The proxy applies to outbound Telegram Bot API calls, WebDAV, the `t.me` embed fallback,
and optional Danbooru catalog lookups. `gallery-dl` receives the proxy URL via `--proxy`.
The local FoidFinder connection uses loopback directly.

* Schemes: `http://`, `https://`, `socks5://`, `socks5h://` (`socks5h` resolves DNS
  through the proxy — usually what you want when the resolver is poisoned).
* `NUJIJUSHI_PROXY` / `NUJIJUSHI_NO_PROXY` override the file, and with no `[proxy]` at all
  the HTTP client still honours the usual `HTTPS_PROXY`/`ALL_PROXY` variables.
* For **socks** proxies `gallery-dl` additionally needs PySocks:
  `pipx inject gallery-dl PySocks` (`nujijushi doctor` reminds you).
* `nujijushi doctor` builds the client and reports the proxy, so a typo fails there and
  not at the first publish.

## External storage (mirrors)

Every imported post can be copied to one or more sinks, independently of publishing:

| sink | good for |
|---|---|
| `local` | any mounted/synced folder — Dropbox, iCloud Drive, Google Drive, an external disk |
| `webdav` | Nextcloud, ownCloud, Synology, HiDrive, Box, pCloud, … (plain PUT, basic auth) |
| `telegram` | a private backup channel using albums of up to 10 photos and individual videos |

Configure one or more sinks in `config.toml`. Local and WebDAV sinks write the folder
layout below with a metadata sidecar. The Telegram sink stores media as messages with
an attribution caption and records the message IDs.

```toml
[storage]
on_import = true
root = "nujijushi"

[storage.local]
enabled = true
path = "/path/to/archive"

[storage.webdav]
enabled = false
url = "https://cloud.example.com/remote.php/dav/files/you/art"
username = "you"
password = "app-password"

[storage.telegram]
enabled = false
chat_id = "-1001234567890"
```

```
<payload>/nujijushi/pixiv/nightjar/123456/00.png
                                           01.mp4
                                           post.json   # id, source, url, tags, rating, artist
```

* `[storage] on_import = true` queues a mirror as soon as a post is imported; the
  background worker uploads it (failures are recorded per post/sink and retried).
* `nujijushi storage ls` — sinks, target, and done/pending/failed counts.
* `nujijushi storage test` — reachability + credentials (writable folder, WebDAV
  `PROPFIND`/`MKCOL`, Telegram `getChat`).
* `nujijushi storage sync [--sink NAME] [--post ID] [--limit N] [--dry-run]` backfills
  posts that still need a mirror. The default limit is 500 posts per invocation.
* The dashboard shows mirror counts per sink and has an **upload pending** button.

Mirrors run independently of the publishing queue. New mirror uploads use the
current outgoing gallery. Completed mirrors keep the media from the original upload.

## Troubleshooting

| symptom | cause / fix |
|---|---|
| `gallery-dl produced no media` | wrong URL form, `Cloudflare challenge`, or login required → add `cookies/<site>.txt` and retry |
| `[danbooru][error] ChallengeError` | Cloudflare challenged the request. Try a fresh cookie export for the site |
| `not enough rights to send photos` | bot is not an admin with "post messages" in that channel |
| `chat not found` | wrong id; use the numeric `-100…` id for private channels |
| `curl: tlsv1 unrecognized name` when testing your own domain | a local proxy (Clash/sing-box TUN, fake-IP) is intercepting it: the handshake never reaches your server. Test from the VPS, or add `DOMAIN-SUFFIX,<your-domain>,DIRECT` + a `fake-ip-filter` entry and reload the profile |
| an older X import shows the wrong artist name or handle | re-import the URL with `nujijushi import <url>` to refresh the stored author fields. The importer handles both gallery-dl user metadata formats and can recover the handle from the post URL |
| bot refuses `/publish` | publishing from Telegram is admin-only *and* opt-in: your effective role must be admin and `[access] allow_publish` must be `true`. `/whoami` shows the id and the role the bot actually uses; `nujijushi doctor` prints the effective switches and the config file it read |
| a user listing shows `admin` and `db: guest` | `[access] admins` sets the effective role to admin even when the database row says guest. Config edits take effect on restart. `/grant <id> admin` takes effect immediately |
| a post the platform flagged sensitive is stored as `sfw` | re-import to refresh the source rating, including nested and per-media sensitivity flags. To set a manual override, run `nujijushi nsfw <id> yes` or use the NSFW action in the web grid |
| `webhook registered` but the bot never answers | registration only stores the URL — Telegram only fails *later*, when it POSTs. `nujijushi doctor` prints a `webhook delivery` line with Telegram's last error (e.g. `Wrong response from the webhook: 502 Bad Gateway`); no error but still silence means nothing reaches nginx. Also: commands only work in a **private chat** with the bot, channel posts are ignored by design |
| webhook cannot work at all | the host must accept inbound HTTPS from Telegram. If you are behind CGNAT, cannot port-forward, or your ISP blocks 80/88/443/8443, use `[bot] mode = "polling"` (the default): outbound only, works anywhere, and the next `nujijushi run` deletes the stale webhook |
| a full queue never drains | the dashboard banner says whether the scheduler is alive. `serve` does not schedule — run `nujijushi run`. If it *is* running, the channel's own state (paused, outside active hours, daily cap, `next_due_at`) is on the dashboard and in `nujijushi channel ls` |
| channel paused itself | 3 consecutive send failures — fix the cause, then `nujijushi channel resume` |
| unconfirmed send after a crash | check the channel, then resolve it in the dashboard |
