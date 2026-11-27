/* Admin connection validation and source URL rules shared by the panel and worker. */
(function (root) {
  "use strict";

  const DEFAULT_SERVER = "http://127.0.0.1:8788";
  const { canonicalize } = root.NJS.canonical;
  const postHosts = {
    x: /^(?:[\w-]+\.)*(?:x|twitter)\.com$/,
    pixiv: /^(?:www\.)?pixiv\.net$/,
    rednote: /^(?:www\.)?(?:rednote|xiaohongshu)\.com$/,
    fanbox: /^(?:[a-z0-9_-]+\.)?fanbox\.cc$/,
  };

  function parseUrl(raw) {
    if (typeof raw !== "string" || raw.length > 8192) return null;
    try {
      const url = new URL(raw);
      return /^https?:$/.test(url.protocol) && !url.username && !url.password ? url : null;
    } catch {
      return null;
    }
  }

  function serverAddress(raw) {
    if (typeof raw !== "string") return null;
    const address = raw.trim();
    // Allow the host and path as copied from the browser address bar.
    const bare = /^[a-z0-9-]+(?:\.[a-z0-9-]+)+(?::\d+)?(?:\/|$)/i.test(address);
    const url = parseUrl(bare ? "https://" + address : address);
    if (!url || url.search || url.hash ||
      (url.protocol === "http:" && !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname))) {
      return null;
    }
    return {
      base: url.origin + url.pathname.replace(/\/+$/, ""),
      permission: `${url.protocol}//${url.hostname}/*`,
    };
  }

  function postPage(raw) {
    const url = parseUrl(raw);
    return !!url && !url.port && Object.values(postHosts).some((hosts) => hosts.test(url.hostname));
  }

  function pagePost(raw, page) {
    const url = parseUrl(raw);
    if (!url || url.port || !postPage(page)) return null;
    const link = canonicalize(url.href);
    if (!link || !postHosts[link.source]?.test(url.hostname)
      || !postHosts[link.source].test(new URL(page).hostname)) return null;
    return link;
  }

  function watchCandidate(raw, displayedStatus) {
    const url = parseUrl(raw);
    if (!url || url.port) return null;
    const host = url.hostname;
    const path = url.pathname.replace(/\/+$/, "");
    if (postHosts.x.test(host)) {
      const page = canonicalize(url.href);
      const permalink = canonicalize(displayedStatus);
      const post = page?.source === "x" ? page : null;
      const named = post?.label === "@i" && permalink?.key === post.key ? permalink : post;
      const handle = post ? named.label.replace(/^@/, "") : path.match(/^\/([\w]+)(?:\/media)?$/)?.[1];
      if (!handle || !/^\w+$/.test(handle) || /^(i|web|home|search|explore|settings|intent|notifications|messages|compose|login|account)$/i.test(handle)) {
        return null;
      }
      return { url: `https://x.com/${handle}/media`, name: `@${handle}` };
    }
    if (postHosts.pixiv.test(host)) {
      const artist = path.match(/^\/(?:en\/)?users\/(\d+)(?:\/artworks)?$/)?.[1];
      if (artist) return { url: `https://www.pixiv.net/users/${artist}/artworks`, name: `Pixiv ${artist}` };
      const post = canonicalize(url.href);
      if (post?.source === "pixiv") return { url: post.url, name: "" };
    }
    if (postHosts.fanbox.test(host)) {
      const creator = host === "fanbox.cc" || host === "www.fanbox.cc"
        ? path.match(/^\/@([\w-]+)(?:\/posts(?:\/\d+)?)?$/)?.[1]
        : /^\/(?:posts(?:\/\d+)?)?$|^$/.test(path) ? host.split(".")[0] : null;
      if (creator && creator.toLowerCase() !== "www") {
        return { url: `https://${creator.toLowerCase()}.fanbox.cc/posts`, name: creator };
      }
    }
    if (host === "bsky.app") {
      const actor = path.match(/^\/profile\/([\w.:-]+)(?:\/media|\/post\/[A-Za-z0-9]+)?$/)?.[1];
      if (actor && (actor.includes(".") || actor.startsWith("did:"))) {
        return { url: `https://bsky.app/profile/${actor}/media`, name: actor };
      }
    }
    return null;
  }

  root.NJS.admin = { DEFAULT_SERVER, serverAddress, pagePost, postPage, watchCandidate };
  if (typeof module !== "undefined" && module.exports) module.exports = root.NJS.admin;
})(globalThis);
