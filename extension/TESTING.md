# Manual test checklist

The DOM half cannot be unit tested here (no browser in CI, no dependencies), so this
is the checklist that stands in for it. Run it after touching `content/clip.js`, an
adapter, or the manifest. Reload the extension **and** the page between runs —
content scripts are not injected into already-open tabs.

```bash
node --test extension/lib/canonical.test.mjs extension/lib/store.test.mjs
cargo test            # the app must stay untouched
```

## 1. load

1. `chrome://extensions` → *Developer mode* → *Load unpacked* → `extension/`.
2. No red "Errors" button on the card. If there is one: open it, the service worker
   log has the stack (usually a typo in a file listed in `manifest.json`).
3. Open the side panel (toolbar button or `Alt+Shift+A`). It shows `0`, the empty
   hint, and `copy` disabled.

## 2. clip three posts on X

1. Go to `https://x.com/i/bookmarks` and scroll until several posts have rendered.
2. Each post's action row now ends with a round `+`. Click three of them.
3. Expect, per click: button turns green with a check, the post gets a green left
   edge, the panel list grows, the toolbar badge counts up.
4. Click one of the three again → it removes that item (badge `2`, edge gone).

## 3. virtualization (the risky one)

1. Scroll 20+ posts away, then back to the clipped posts.
2. Expect: the check and the green edge are still on the right posts, and on **no**
   other post.
3. Reload the page: same.
4. Reload the extension, reopen the panel: the list is intact (storage survived).

## 4. dedupe

1. In the panel's *paste links* box, paste all of these and press *add*:

```
https://x.com/Artist/status/123
https://twitter.com/Artist/status/123?s=20&t=abc
https://mobile.twitter.com/Artist/status/123/photo/1
/Users/me/Pictures/some-folder
```

2. Expect `1 added · 2 already there · 1 not a link` and exactly one new row showing
   `https://x.com/Artist/status/123`.
3. Add the same blob again → `0 added · 3 already there · 1 not a link`.
4. Paste a percent-encoded list (this is what a `%0A` separator looks like):

```
https://x.com/Artist/status/777%0Ahttps://x.com/Artist/status/888%0A
```

   Expect `2 added`, two rows, and **no** URL containing `%0A`. Pasting the same blob
   again → `0 added · 2 already there`.

## 5. copy

1. Press `copy N`. The status line says `N copied — paste into /import`, and the rows
   are struck through; `copy all N` appears.
2. Paste into a scratch editor: exactly N lines, each `https://x.com/<handle>/status/<id>`,
   no `?s=`, no trailing spaces, in the order you clicked.
3. Clip one more post → the primary button says `copy 1` again (only the new one).
4. Settings → enable *clear the list right after a successful copy*, copy, and confirm
   the list empties itself. Turn it back off.

## 6. the actual goal: into nujijushi

1. `nujijushi run` (or `serve`) is up, `http://127.0.0.1:8788/import` open in a tab.
2. Paste the copied list into the box and click **start import**.
3. Expect one line per URL and one `import` job per link. After each job finishes,
   check for the artwork under **Posts**. To queue artwork, select the imported
   posts there and choose a destination channel.

## 7. other ingest paths

1. Open a post in its own tab → `Alt+Shift+L` → added (badge flashes `✓`).
2. `Alt+Shift+L` on `chrome://extensions` → badge flashes `×`, nothing added.
3. Right-click a status link on any page → *Clip link to nujijushi* → added.
4. Select a paragraph containing two status links → *Clip selected links* → both added.
5. Right-click a danbooru post page → *Clip this page* → added.

## 8. pixiv

1. `/artworks/<id>` → the floating pill appears bottom right, click adds the artwork;
   `?p=2` on the same artwork is still one entry.
2. An artist's illust list → hovering a thumbnail reveals the corner button; clicking
   it clips the artwork and does **not** navigate (check the URL did not change).
3. Grid layout is unchanged (no reflow, no clipped captions).

## 9. settings

1. Turn off *X / Twitter post buttons*, reload an X tab: no buttons on posts, no pill.
2. Turn it back on, reload: buttons return, and previously clipped posts still show
   the check mark immediately (state comes from storage, not from the DOM).

## 10. resilience

1. Open the panel in a second window while the first is open: both update live.
2. Remove the extension and re-install it: the list is gone (it lives in extension
   storage) — acceptable, but note it if you care about long collecting sessions.
3. In a page console, `chrome.runtime.lastError` should never appear after clicking a
   toggle; if it does ("Extension context invalidated"), reload the tab.

## Debugging

| symptom | look at |
|---|---|
| no buttons on posts | page console filter `nujijushi`, and `document.querySelectorAll('article[data-testid="tweet"]').length` |
| buttons appear but nothing is stored | service worker console (`chrome://extensions` → *service worker*), the `toggle` message reply |
| panel list does not update | panel DevTools console; `chrome.storage.onChanged` still fires with the extension page open |
| posts clip but `/import` rejects the line | the URL in the row is the canonical one; `nujijushi import <that url>` from a terminal shows gallery-dl's error |
