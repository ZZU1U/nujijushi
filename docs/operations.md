# Operations guide

[Back to the quickstart](../README.md)

## Configuration

`nujijushi init` creates `config.toml` in `~/.nujijushi`. Set `NUJIJUSHI_HOME` to use a different directory. See [config.example.toml](../config.example.toml) for core settings.

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

## Talking to the bot

`nujijushi run` also long-polls Telegram, so admins (and, if you allow it, guests) can drive
the archive from a chat. `--no-bot` or `[bot] enabled = false` turns just this off.

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

Importing a **Telegram post** (`t.me/<name>/<id>` or `t.me/c/<id>/<n>`) works when the bot is
a member of that channel: the post is forwarded into your own chat, the files are downloaded,
and the copy is deleted. Albums are reconstructed from the neighbouring message ids.
Constraints are Telegram's: 20 MB per file, and a public channel the bot is not in can only be
scraped from its `?embed=1` page — one image, reduced size, marked
`public embed (degraded)` on the post. Real fidelity for arbitrary channels needs MTProto,
which nujijushi does not do.

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

The proxy applies to outbound Telegram Bot API calls, storage uploads, and the `t.me` embed fallback.
`gallery-dl` receives the proxy via `--proxy`.

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
| `telegram` | a private channel used as an image drive (albums of ≤10, one message group per post) |

Configure one or more sinks in `config.toml`. Each sink gets a portable layout with a metadata sidecar.

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
* `nujijushi storage sync [--post ID] [--limit N] [--dry-run]` — backfill everything that
  is not mirrored yet (this is the "send my whole existing archive" command).
* The dashboard shows mirror counts per sink and has an **upload pending** button.

Mirrors run independently of the publishing queue.

## Troubleshooting

| symptom | cause / fix |
|---|---|
| `gallery-dl produced no media` | wrong URL form, `Cloudflare challenge`, or login required → add `cookies/<site>.txt` and retry |
| `[danbooru][error] ChallengeError` | Cloudflare: cookies are mandatory for boorus |
| `not enough rights to send photos` | bot is not an admin with "post messages" in that channel |
| `chat not found` | wrong id; use the numeric `-100…` id for private channels |
| `curl: tlsv1 unrecognized name` when testing your own domain | a local proxy (Clash/sing-box TUN, fake-IP) is intercepting it: the handshake never reaches your server. Test from the VPS, or add `DOMAIN-SUFFIX,<your-domain>,DIRECT` + a `fake-ip-filter` entry and reload the profile |
| every twitter artist is `unknown` | gallery-dl reports the handle as `username`, which older builds ignored. New imports use it (and `user_id`); already-imported rows keep the author they were stored with, so re-import that post to fix it |
| bot refuses `/publish` | publishing from Telegram is admin-only *and* opt-in: your effective role must be admin and `[access] allow_publish` must be `true`. `/whoami` shows the id and the role the bot actually uses; `nujijushi doctor` prints the effective switches and the config file it read |
| `users ls` says `guest (config admin)` | that is only the stored row: a config admin is admin at runtime (`/whoami` proves it). Since v0.2.x the listings print the effective role — rebuild to get it. Editing `[access] admins` takes effect on restart; `/grant <id> admin` is live |
| a post the platform flagged sensitive is stored as `sfw` | the flag was nested (retweet/quote objects, per-media `sensitive`) where the mapper did not look — fixed, so rebuild and re-import. Ratings are stored at import, so an existing row keeps its value: `nujijushi nsfw <id> yes`, or "mark nsfw" in the web grid |
| `webhook registered` but the bot never answers | registration only stores the URL — Telegram only fails *later*, when it POSTs. `nujijushi doctor` prints a `webhook delivery` line with Telegram's last error (e.g. `Wrong response from the webhook: 502 Bad Gateway`); no error but still silence means nothing reaches nginx. Also: commands only work in a **private chat** with the bot, channel posts are ignored by design |
| webhook cannot work at all | the host must accept inbound HTTPS from Telegram. If you are behind CGNAT, cannot port-forward, or your ISP blocks 80/88/443/8443, use `[bot] mode = "polling"` (the default): outbound only, works anywhere, and the next `nujijushi run` deletes the stale webhook |
| a full queue never drains | the dashboard banner says whether the scheduler is alive. `serve` does not schedule — run `nujijushi run`. If it *is* running, the channel's own state (paused, outside active hours, daily cap, `next_due_at`) is on the dashboard and in `nujijushi channel ls` |
| channel paused itself | 3 consecutive send failures — fix the cause, then `nujijushi channel resume` |
| unconfirmed send after a crash | check the channel, then resolve it in the dashboard |
