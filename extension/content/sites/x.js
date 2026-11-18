/* X / Twitter adapter — the primary one.
 *
 * Every timeline (bookmarks, home, lists, profiles, search) renders posts as
 * `article[data-testid="tweet"]`. The post's own permalink is the anchor that
 * wraps <time>; quoted and retweeted posts also expose /status/ anchors further
 * down, so a plain `querySelector` would clip the wrong post.
 *
 * The toggle goes into the action bar (like/reply/retweet/share row) because that
 * is the one container X reliably keeps per post. When a row is recycled the
 * engine's observer re-injects and repaints from storage, which is why clipped
 * state is keyed by status id and never stored on the DOM.
 */
(function () {
  "use strict";

  const X_HOSTS =
    /(^|\.)(x|twitter)\.com$|(^|\.)(fxtwitter|vxtwitter|fixupx|fixvx)\.com$|(^|\.)nitter\.[a-z.]+$|(^|\.)xcancel\.com$/;
  const STATUS_PATH = /^\/([^/]+)\/status(?:es)?\/(\d+)/;
  const NOT_A_HANDLE = new Set(["i", "web", "intent", "search", "hashtag", "home", "explore", "notifications"]);

  window.NJS = window.NJS || {};
  window.NJS.register({
    id: "x",
    match: (location) => X_HOSTS.test(location.hostname),
    scan(root) {
      const items = [];
      for (const article of root.querySelectorAll('article[data-testid="tweet"]')) {
        const found = permalink(article);
        if (!found) continue;
        items.push({
          key: found.key,
          url: found.url,
          source: "x",
          label: found.label,
          el: article,
          slot: article.querySelector('[role="group"]') || article,
        });
      }
      return items;
    },
  });

  function permalink(article) {
    let fallback = null;
    for (const anchor of article.querySelectorAll('a[href*="/status/"]')) {
      let url;
      try {
        url = new URL(anchor.getAttribute("href"), location.href);
      } catch {
        continue;
      }
      const found = describe(url);
      if (!found) continue;
      if (anchor.querySelector("time")) return found;
      if (!fallback) fallback = found;
    }
    return fallback;
  }

  function describe(url) {
    const match = url.pathname.match(STATUS_PATH);
    if (!match) return null;
    const handle = match[1];
    const id = match[2];
    const named = !NOT_A_HANDLE.has(handle.toLowerCase());
    return {
      key: `x:${id}`,
      url: `https://x.com/${named ? handle : "i"}/status/${id}`,
      label: named ? `@${handle}` : "",
    };
  }
})();
