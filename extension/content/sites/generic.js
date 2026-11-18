/* Generic adapter — a "clip this page" pill on any single-post page whose URL the
 * canonicalizer understands (danbooru post, DA artwork, artstation, t.me, ...).
 *
 * The engine only runs fallback adapters when no site adapter matched, so this
 * never stacks a second button onto the X or pixiv pages.
 */
(function () {
  "use strict";

  window.NJS = window.NJS || {};
  window.NJS.register({
    id: "generic",
    fallback: true,
    match: (location) => location.protocol === "http:" || location.protocol === "https:",
    scan(root, location) {
      const found = window.NJS.canonical.canonicalize(location.href);
      // "web" means we could not name the post — a pill there would be noise.
      if (!found || found.source === "web") {
        window.NJS.hideFloatingClip();
        return [];
      }
      return [window.NJS.floatingClip(found)];
    },
  });
})();
