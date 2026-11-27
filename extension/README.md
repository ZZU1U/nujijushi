# nujijushi clip

A tiny Chromium extension for the boring half of importing: **clicking through a
bookmarks feed and collecting the post URLs.**

Click a `+` on any post, collect a dozen, press **copy**, paste the list into
nujijushi's `/import` box. One URL per line, no duplicates, no typing.

The default setup works as a standalone clipboard tool:

* clipping and copying links need no server or token;
* page buttons are injected on X, Pixiv, FANBOX, RedNote, and Xiaohongshu. The
  keyboard shortcut uses `activeTab`;
* paste the copied URLs into the admin **Import** page, which accepts one target
  per line and ignores blank lines and `#` comments. For the CLI, pass each URL
  as a separate argument.

## Install

1. Chrome/Edge 116+ (needs the side panel API). `chrome://extensions` →
   *Developer mode* → *Load unpacked* → pick this `extension/` folder.
2. Click the toolbar button (or `Alt+Shift+A`) to open the panel, then pin it so it
   stays docked next to the timeline.
3. `chrome://extensions/shortcuts` if `Alt+Shift+L` / `Alt+Shift+A` collide with
   something else.

Updating: after editing files press *Reload* on the extensions page, then reload the
tab — content scripts are not injected into pages that were already open.

## Connect to nujijushi

Click the gear in the side panel to open the options tab. Enter your server URL
and the same admin token you use to sign in to the WebUI, then save the connection.
**Test connection** checks the URL and token. Use HTTPS for a remote server. A local URL such as
`http://127.0.0.1:8788` also works, including through an SSH tunnel. Include the
path prefix if your server uses one, for example `https://archive.example.org/art`.
The token has full admin access.

Enable **Send directly to nujijushi** to change the button on each post. A click
queues that post and marks its clip as copied after the server accepts the job.
It appears on the server’s **Import** page with the other import jobs.

In **Watch artist**, choose **Use current tab** from an artist page or an open
post. Check or edit the proposed profile URL and name, then select **Add watch**.
You can also paste a supported profile URL. If the extension cannot read the tab,
open the panel from the toolbar button on that tab and try again.

## Workflow

```
x.com/i/bookmarks            side panel                nujijushi /import
┌──────────────────┐   click ┌────────────────┐  copy  ┌──────────────────┐
│ @a · 2h      [+] │ ──────► │ x.com @a/…  ✕  │ ─────► │ https://x.com/…  │
│ text …           │         │ x.com @b/…  ✕  │ paste  │ https://x.com/…  │
│ ♡ 12  ⟲ 3  ↗  [+]│         │ [copy 2]       │        │ [start import]   │
└──────────────────┘         └────────────────┘        └──────────────────┘
```

Clipped posts get a green left edge and their button turns into a check, so you can
see what you already took while scrolling. Clicking again removes it.

## The four ways in

| how | when |
|---|---|
| the `+` / `✓` button on a post | the normal path: bookmarks, home, lists, profiles, search |
| `Alt+Shift+L` | the post is open in its own tab |
| right-click → *Clip link* / *Clip selected links* / *Clip this page* | odd layouts, or a page with several links in the selection |
| *paste links* box in the panel | you already have a blob of URLs somewhere (also handles a `%0A`-joined list) |

Every path canonicalizes first, so the same post pasted in a different shape is the
same entry.

## Copy behaviour

* `copy N` copies the **not yet copied** entries, one per line, in click order.
  Copied rows are struck through; `copy all N` appears when some are already copied.
* `clear` empties the list (asks first). Per-row `✕` removes one.
* Settings → *clear the list right after a successful copy* gives you the one-click
  loop: click, click, copy, paste, done. Off by default, because a blocked clipboard
  must never eat your list.

## What counts as the same post

`lib/canonical.js` maps a URL to a stable identity before anything is stored, so all
of these are one entry:

| site | forms | identity |
|---|---|---|
| X / Twitter | `x.com`, `twitter.com`, `mobile.`, `fxtwitter`, `vxtwitter`, `nitter`, `/i/web/status/<id>`, `/photo/1`, `?s=20&t=…` | `x:<status id>` |
| pixiv | `/artworks/<id>`, `/en/artworks/<id>`, `/i/<id>`, `member_illust.php?illust_id=`, `?p=3` (page kept as a hint only) | `pixiv:<id>` |
| FANBOX | `<creator>.fanbox.cc/posts/<id>` and `www.fanbox.cc/@<creator>/posts/<id>` | `fanbox:<creator>:<id>` |
| RedNote / Xiaohongshu | `/explore/<id>`, `/discovery/item/<id>`, `/user/profile/<author>/<id>`, and their `/en/` variants | `rednote:<id>` |
| RedNote app share links | `xhslink.com` and `xhslink.cn` | short link until the importer resolves it |
| danbooru / gelbooru / safebooru | `/posts/<id>`, `index.php?s=view&id=` | `<source>:<id>` |
| artstation / deviantart / tumblr / bluesky / mastodon / t.me | id- or slug-based | `<source>:<id>` |
| anything else | `utm_*`, `fbclid`, `si`, `s`, `t`, `ref`, fragments and trailing slashes stripped, query sorted | `url:<host><path>[?<query>]` |

Non-links (folder paths, `javascript:`, `chrome://`, loose words) are refused rather
than stored as junk. A list that arrives percent-encoded (`...status/1%0A...status/2`)
is split like real newlines, and a single "link" that still contains `%0A` is refused
instead of being stored as one broken URL.

URLs are also normalised before copying. An X post is written as
`https://x.com/<handle>/status/<id>`, with `i` in place of the handle on handle-free links.

RedNote links keep `xsec_token` and `xsec_source` in the copied URL. Paste a fresh
share URL for an already clipped note to refresh its link.

## Adding a site

Two parts, and most sites only need the first.

1. **Identity** — add a `match<Site>()` to `lib/canonical.js` and cases to
   `lib/canonical.test.mjs`. That alone makes the paste box, the shortcut and the
   context menus use that identity. The fallback pill uses it on pages matched by
   `content_scripts` in `manifest.json`.
2. **Per-post buttons on a timeline** — add `content/sites/<site>.js`:

```js
window.NJS = window.NJS || {};
window.NJS.register({
  id: "danbooru",
  // only run on pages this adapter understands
  match: (location) => /(^|\.)donmai\.us$/.test(location.hostname),
  // map the current DOM to posts; return [] when nothing matches
  scan(root, location) {
    return [...root.querySelectorAll("article.post")].map((article) => {
      const link = window.NJS.canonical.canonicalize(article.querySelector("a.post-link").href);
      return { key: link.key, url: link.url, source: "danbooru", label: link.label, el: article, slot: article };
    });
  },
});
```

Then add the file to `content_scripts[0].js` in `manifest.json` after the `lib/`
scripts, and add the site's URLs to `matches`. Set `fallback: true` if it
should only run when no real adapter matched (that is how the page pill works).

**The engine owns the rest** (`content/clip.js`):

* it rescans on DOM mutations (X recycles its rows — a click must survive scrolling),
* it injects exactly one `.njs-btn` per `slot` and repaints it from storage,
* it stops `mousedown`/`click` propagation so the site does not treat the click as a
  navigation,
* it toggles through the service worker, which is the only writer.

An adapter must therefore be pure DOM → array. No listeners, no storage, no state.

## Files

| file | role |
|---|---|
| `manifest.json` | MV3: side panel, one content-script bundle, two shortcuts |
| `background.js` | single writer for the bin, toolbar badge, context menus, commands |
| `lib/canonical.js` | URL → `{key, url, source, label, page}` (pure, shared by every context) |
| `lib/store.js` | the bin: `chrome.storage.local` map + settings |
| `content/clip.js` | the engine (registry, observer, injection, painting) — loaded **first** |
| `content/sites/*.js` | adapters: `x`, `pixiv`, `rednote`, `generic` |
| `content/clip.css` | injected styling, namespaced `.njs-*` |
| `panel.html/.css/.js` | the side panel |
| `options.html/.css/.js` | connection, clipping preferences, and on-page button settings |

Storage shape (`chrome.storage.local`, key `njs.bin`):

```js
{ v: 1,
  items: { "x:123": { url, source, label, page, addedAt, seq, copied } },
  settings: { autoClear: false, adapters: { x: true, pixiv: true, rednote: true, generic: true } } }
```

A keyed map, so dedupe and toggle are O(1); `seq` keeps click order.

## Tests

```bash
node --test extension/lib/*.test.mjs
```

Node-only, no dependencies, no browser. `store.test.mjs` stubs `chrome.storage.local`,
so dedupe, click order and copy marking are covered without Chrome. The DOM side has
no harness — use `TESTING.md`.

## Known limits

* X's markup is not an API. If they rename `article[data-testid="tweet"]` or the
  `[role="group"]` action bar, the `x` adapter needs a one-line fix; everything else
  (canonicalizing, clipboard, panel) keeps working.
* Only the URLs matter, so a post whose link you clipped but which gallery-dl cannot
  fetch will fail at import time in nujijushi, not here.
* No icons shipped (Chrome shows the default puzzle piece). Add `icons/` and an
  `icons` block in the manifest if you want to publish it.
* No "select all"/"harvest page" on purpose: a bookmarks feed contains mostly noise,
  and a bulk button makes it too easy to queue 200 posts you did not look at.
