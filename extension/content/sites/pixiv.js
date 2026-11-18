/* pixiv adapter.
 *
 *  - artwork page (/artworks/<id>, /i/<id>, member_illust.php?illust_id=): the shared
 *    floating pill, bottom right.
 *  - user illust/manga lists and search results: a hover toggle in the corner of each
 *    thumbnail card, one click each (no "select all" — that is not the workflow).
 *
 * The click target sits inside the anchor, so the engine stops propagation; the
 * wrapper only gets `position: relative` when it is static, so grids do not reflow.
 */
(function () {
  "use strict";

  const PIXIV_HOSTS = /(^|\.)pixiv\.net$/;

  window.NJS = window.NJS || {};
  window.NJS.register({
    id: "pixiv",
    match: (location) => PIXIV_HOSTS.test(location.hostname),
    scan(root, location) {
      const items = [];
      const here = window.NJS.canonical.canonicalize(location.href);

      if (here && here.source === "pixiv") items.push(window.NJS.floatingClip(here));
      else window.NJS.hideFloatingClip();

      for (const anchor of root.querySelectorAll('a[href*="/artworks/"]')) {
        if (!anchor.querySelector("img")) continue; // text links and the page's own title
        if (anchor.closest(".njs-pill")) continue;
        const found = window.NJS.canonical.canonicalize(anchor.getAttribute("href"));
        if (!found || found.source !== "pixiv") continue;
        markThumb(anchor);
        items.push({ key: found.key, url: found.url, source: "pixiv", label: found.label, el: anchor, slot: anchor });
      }

      return items;
    },
  });

  function markThumb(anchor) {
    anchor.classList.add("njs-thumb");
    if (getComputedStyle(anchor).position === "static") anchor.style.position = "relative";
  }
})();
