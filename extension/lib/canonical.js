/* nujijushi clip — canonical link identity.
 *
 * Loaded as a classic script everywhere: content scripts, the side panel, and the
 * service worker (via importScripts). The UMD tail also exports to `module.exports`
 * so lib/canonical.test.mjs can require() it from node with no build step.
 *
 *   canonicalize(raw) -> { key, url, source, label, page } | null
 *
 *   key     stable identity used for dedupe ("x:123", "pixiv:456", "url:host/path")
 *   url     canonical URL worth handing to gallery-dl / the import box
 *   source  "x" | "pixiv" | "danbooru" | ... | "web" (unknown but http(s))
 *   label   short human label for the panel
 *   page    page hint, deliberately *not* part of the identity (pixiv ?p=)
 *
 * Adding a site: write a matchXxx() below (or a content/sites/*.js adapter if the
 * page needs DOM work) and add cases to lib/canonical.test.mjs.
 */
(function (root, factory) {
  const api = factory();
  root.NJS = root.NJS || {};
  root.NJS.canonical = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  // Identity-neutral query noise. Anything utm_* is dropped by prefix below.
  const TRACKING = new Set([
    "fbclid",
    "gclid",
    "msclkid",
    "yclid",
    "mc_cid",
    "mc_eid",
    "igshid",
    "ref",
    "ref_src",
    "ref_url",
    "si",
    "s",
    "t",
    "mx",
    "share",
    "share_id",
    "vero_id",
    "_branch_match_id",
    "__twitter_impression",
  ]);

  const X_HOSTS =
    /(^|\.)(x|twitter)\.com$|(^|\.)(fxtwitter|vxtwitter|fixupx|fixvx|girlcockx)\.com$|(^|\.)nitter\.[a-z.]+$|(^|\.)xcancel\.com$/;
  const PIXIV_HOSTS = /(^|\.)pixiv\.net$/;
  const BOORU_HOSTS = /(^|\.)(gelbooru\.com|safebooru\.org|rule34\.xxx)$/;
  const TELEGRAM_HOSTS = /(^|\.)(t\.me|telegram\.me|telegram\.dog)$/;

  const RESERVED_X_SEGMENTS = new Set([
    "i",
    "web",
    "intent",
    "search",
    "hashtag",
    "home",
    "explore",
    "notifications",
    "messages",
    "settings",
    "compose",
    "status",
    "statuses",
    "account",
    "login",
  ]);

  function canonicalize(raw) {
    const cleaned = strip(raw);
    if (!cleaned) return null;
    let url;
    try {
      url = new URL(cleaned);
    } catch {
      return null;
    }
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;

    const host = url.hostname.toLowerCase().replace(/^www\./, "");
    const path = url.pathname;
    const query = url.searchParams;

    return (
      matchX(host, path) ||
      matchPixiv(host, path, query) ||
      matchBooru(host, path, query) ||
      matchArt(host, path) ||
      matchTumblr(host, path) ||
      matchBluesky(host, path) ||
      matchMastodon(host, path) ||
      matchTelegram(host, path) ||
      generic(url.protocol, host, path, query)
    );
  }

  /** True when the URL is a single post we can name, not just "some page". */
  function isKnownPost(link) {
    return !!link && link.source !== "web";
  }

  // ------------------------------------------------------------------ helpers

  function strip(raw) {
    if (typeof raw !== "string") return null;
    let s = raw.trim();
    if (!s) return null;
    // wrappers people pick up from chat clients and markdown
    s = s.replace(/^[<("'`\[]+/, "").replace(/[>)"'`\],.]+$/, "");
    if (!s) return null;
    // local folders are valid import targets, just not links we clip
    if (/^[/~.]/.test(s) || /^[A-Za-z]:[\\/]/.test(s)) return null;
    if (/^(javascript|data|file|mailto|about|blob|chrome|chrome-extension|edge|moz-extension|view-source):/i.test(s)) {
      return null;
    }
    if (/\s/.test(s)) return null;
    // a percent-encoded newline/tab is never part of a real link (it means the input
    // was a "https://a%0Ahttps://b" blob that should have been split first)
    if (/%0[adAD]|%09/.test(s)) return null;
    if (s.startsWith("//")) return (s = "https:" + s);
    if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(s)) {
      if (!/^[a-z0-9-]+(\.[a-z0-9-]+)+([/?#]|$)/i.test(s)) return null;
      s = "https://" + s;
    }
    return s;
  }

  function link(source, key, url, label, page) {
    return { key, url, source, label: label || "", page: page || null };
  }

  // -------------------------------------------------------------------- sites

  function matchX(host, path) {
    if (!X_HOSTS.test(host)) return null;
    let handle = null;
    let id = null;
    let m = path.match(/^\/([^/]+)\/status(?:es)?\/(\d+)(?:\/.*)?$/);
    if (m) {
      handle = m[1];
      id = m[2];
    } else if ((m = path.match(/^\/i\/(?:web\/)?status\/(\d+)(?:\/.*)?$/))) {
      id = m[1];
    } else if ((m = path.match(/^\/status(?:es)?\/(\d+)(?:\/.*)?$/))) {
      id = m[1];
    }
    if (!id) return null;
    if (!handle || RESERVED_X_SEGMENTS.has(handle.toLowerCase())) handle = "i";
    return link("x", `x:${id}`, `https://x.com/${handle}/status/${id}`, `@${handle}`);
  }

  function matchPixiv(host, path, query) {
    if (!PIXIV_HOSTS.test(host)) return null;
    let id = null;
    const m =
      path.match(/^\/(?:[a-z]{2}\/)?artworks\/(\d+)/) ||
      path.match(/^\/i\/(\d+)/) ||
      path.match(/^\/member_illust\.php$/);
    if (m && m[1]) {
      id = m[1];
    } else {
      const alt = query.get("illust_id");
      if (alt && /^\d+$/.test(alt)) id = alt;
    }
    if (!id) return null;
    const page = Math.max(0, parseInt(query.get("p") || "", 10) || 0) || null;
    return link("pixiv", `pixiv:${id}`, `https://www.pixiv.net/artworks/${id}`, `artworks/${id}`, page);
  }

  function matchBooru(host, path, query) {
    if (/(^|\.)donmai\.us$/.test(host)) {
      const m = path.match(/^\/posts\/(\d+)/);
      if (m) {
        return link("danbooru", `danbooru:${m[1]}`, `https://${host}/posts/${m[1]}`, `posts/${m[1]}`);
      }
    }
    if (BOORU_HOSTS.test(host)) {
      const id = query.get("id");
      const isPost = query.get("s") === "view" || query.get("page") === "post";
      if (id && /^\d+$/.test(id) && isPost) {
        const source = host.includes("gelbooru") ? "gelbooru" : host.includes("rule34") ? "rule34" : "safebooru";
        return link(source, `${source}:${id}`, `https://${host}/index.php?page=post&s=view&id=${id}`, `post ${id}`);
      }
    }
    return null;
  }

  function matchArt(host, path) {
    if (/(^|\.)artstation\.com$/.test(host)) {
      const m = path.match(/^\/artwork\/([A-Za-z0-9_-]+)/);
      if (m) {
        return link("artstation", `artstation:${m[1]}`, `https://www.artstation.com/artwork/${m[1]}`, m[1]);
      }
    }
    if (/(^|\.)deviantart\.com$/.test(host)) {
      const m = path.match(/\/art\/([^/]+?)-(\d+)\/?$/) || path.match(/\/art\/([A-Za-z0-9._-]+)\/?$/);
      if (m) {
        const slug = m[1];
        const id = m[2] || slug;
        const clean = path.replace(/\/+$/, "");
        return link("deviantart", `deviantart:${id}`, `https://www.deviantart.com${clean}`, slug);
      }
    }
    return null;
  }

  function matchTumblr(host, path) {
    if (!/(^|\.)tumblr\.com$/.test(host)) return null;
    const blog = host === "tumblr.com" ? null : host.replace(/\.tumblr\.com$/, "");
    let owner = blog;
    let m = blog ? path.match(/^\/post\/(\d+)/) : null;
    if (!m) {
      const alt = path.match(/^\/([A-Za-z0-9-]+)\/(\d+)\/?/);
      if (!alt) return null;
      owner = alt[1];
      m = alt;
    }
    const id = m[1] === owner ? m[2] : m[1];
    if (!id) return null;
    return link("tumblr", `tumblr:${owner}:${id}`, `https://${owner}.tumblr.com/post/${id}`, `${owner}/post/${id}`);
  }

  function matchBluesky(host, path) {
    if (!/(^|\.)bsky\.app$/.test(host)) return null;
    const m = path.match(/^\/profile\/([^/]+)\/post\/([A-Za-z0-9]+)/);
    if (!m) return null;
    return link("bluesky", `bluesky:${m[1]}:${m[2]}`, `https://bsky.app/profile/${m[1]}/post/${m[2]}`, `@${m[1]}`);
  }

  function matchMastodon(host, path) {
    const m = path.match(/^\/@([^/]+)\/(\d+)/);
    if (!m) return null;
    return link("mastodon", `mastodon:${host}:${m[2]}`, `https://${host}/@${m[1]}/${m[2]}`, `@${m[1]}`);
  }

  function matchTelegram(host, path) {
    if (!TELEGRAM_HOSTS.test(host)) return null;
    // /c/<chat>/<msg> first: the loose pattern below would read "c" as the channel.
    let m = path.match(/^\/c\/(\d+)\/(\d+)/);
    if (m) {
      return link("telegram", `telegram:c${m[1]}:${m[2]}`, `https://t.me/c/${m[1]}/${m[2]}`, `t.me/c/${m[1]}`);
    }
    m = path.match(/^\/s\/([A-Za-z0-9_]+)\/(\d+)/) || path.match(/^\/([A-Za-z0-9_]+)\/(\d+)/);
    if (m) {
      return link("telegram", `telegram:${m[1]}:${m[2]}`, `https://t.me/${m[1]}/${m[2]}`, `t.me/${m[1]}`);
    }
    return null;
  }

  function generic(protocol, host, path, query) {
    const kept = new URLSearchParams();
    const keys = [...new Set([...query.keys()])].sort();
    for (const key of keys) {
      const lowered = key.toLowerCase();
      if (TRACKING.has(lowered) || lowered.startsWith("utm_")) continue;
      kept.set(key, query.get(key));
    }
    const cleanPath = path.replace(/\/+$/, "") || "/";
    const search = kept.toString();
    return link(
      "web",
      `url:${host}${cleanPath}${search ? "?" + search : ""}`,
      `${protocol}//${host}${cleanPath}${search ? "?" + search : ""}`,
      host,
    );
  }

  /**
   * Split a blob of text into candidate URLs.
   *
   * Tolerates the two ways a newline-separated list survives a round trip through a
   * URL-encoded context (chat clients, query strings, "copy as URL" tools):
   * `%0A`-separated blobs are decoded before splitting. Percent-decoding only happens
   * when the blob actually contains an encoded line break, so ordinary URLs that use
   * %2F/%20/... are left untouched.
   */
  function splitUrls(text) {
    let blob = typeof text === "string" ? text : "";
    if (/%0[adAD]/.test(blob)) {
      try {
        blob = decodeURIComponent(blob);
      } catch {
        /* malformed escape: keep the raw text, the splitter still works on it */
      }
    }
    return blob.split(/[\s<>"']+/).filter(Boolean);
  }

  return { canonicalize, isKnownPost, splitUrls, strip };
});
