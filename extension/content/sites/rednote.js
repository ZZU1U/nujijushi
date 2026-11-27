/* RedNote note pages and thumbnail cards in the feed, search and profiles. */
(function () {
  "use strict";

  const { canonicalize, hasRedNoteToken } = window.NJS.canonical;
  const noteHosts = /^(?:www\.)?(?:rednote|xiaohongshu)\.com$/;

  window.NJS.register({
    id: "rednote",
    match: (location) => noteHosts.test(location.hostname),
    scan(root, location) {
      const items = [];
      let here = canonicalize(location.href);
      for (const card of root.querySelectorAll(".note-item")) {
        let note = null;
        for (const anchor of card.querySelectorAll("a[href]")) {
          const link = canonicalize(anchor.href);
          if (!link?.key.startsWith("rednote:")) continue;
          if (!note || (note.key === link.key && hasRedNoteToken(link))) note = link;
        }
        if (!note) {
          if (card.dataset.njsRow?.startsWith("rednote:")) {
            card.querySelector(":scope > .njs-btn")?.remove();
            card.classList.remove("njs-thumb", "njs-rednote-card", "njs-rednote-position");
            delete card.dataset.njsRow;
            delete card.dataset.njsClipped;
          }
          continue;
        }
        if (here?.key === note.key && !hasRedNoteToken(here)) here = note;
        if (getComputedStyle(card).position === "static") card.classList.add("njs-rednote-position");
        card.classList.add("njs-thumb", "njs-rednote-card");
        items.push({ ...note, el: card, slot: card });
      }
      if (here?.source === "rednote") items.push(window.NJS.floatingClip(here));
      else window.NJS.hideFloatingClip();
      return items;
    },
  });
})();
