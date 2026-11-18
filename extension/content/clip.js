/* nujijushi clip — the in-page engine.
 *
 * Loaded FIRST in the content_scripts list so adapters can call NJS.register()
 * from scripts that run later in the same injection batch. Everything is started
 * from a zero-delay timer for exactly that reason: by then every adapter exists.
 *
 * The engine owns the hard parts:
 *   - observing the DOM (X virtualises its timeline: rows unmount and come back)
 *   - injecting one idempotent toggle per post, repainting it from stored state
 *   - toggling through the service worker so there is a single writer
 *
 * An adapter only maps the current DOM to { url, el, slot } — see
 * content/sites/*.js and README.md for the contract.
 */
(function () {
  "use strict";

  const adapters = [];
  const clipped = new Set(); // canonical keys currently in the bin
  const warned = new Set();
  let settings = { adapters: {}, autoClear: false };
  let started = false;
  let observing = false;
  let scanning = false;


  const root = (window.NJS = window.NJS || {});

  root.register = function register(adapter) {
    if (!adapter || !adapter.id || typeof adapter.scan !== "function") return;
    if (adapters.some((other) => other.id === adapter.id)) return;
    adapters.push(adapter);
  };

  // ------------------------------------------------------------------ messages

  function send(message) {
    return new Promise((resolve) => {
      try {
        chrome.runtime.sendMessage(message, (reply) => {
          void chrome.runtime.lastError;
          resolve(reply || {});
        });
      } catch {
        resolve({}); // extension reloaded under us
      }
    });
  }

  async function loadState() {
    if (!root.store) return;
    const state = await root.store.read();
    settings = state.settings || settings;
    clipped.clear();
    for (const key of Object.keys(state.items)) clipped.add(key);
  }

  // ------------------------------------------------------------------- buttons

  const ICON =
    '<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" focusable="false">' +
    '<path class="njs-i-plus" fill="currentColor" d="M11 5h2v6h6v2h-6v6h-2v-6H5v-2h6z"/>' +
    '<path class="njs-i-check" fill="currentColor" d="M9.55 17.6 4 12.05l1.4-1.4 4.15 4.15 8.9-8.9L19.9 7.3z"/>' +
    "</svg>";

  function createButton() {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "njs-btn";
    button.dataset.njsState = "off";
    button.innerHTML = ICON;
    button.setAttribute("aria-pressed", "false");
    button.title = "Clip to nujijushi";
    // X reacts to mousedown/click on the whole post; stop it touching ours.
    for (const type of ["mousedown", "mouseup", "pointerdown", "pointerup"]) {
      button.addEventListener(type, (event) => event.stopPropagation(), true);
    }
    button.addEventListener("click", onToggle, true);
    return button;
  }

  async function onToggle(event) {
    const button = event.currentTarget;
    event.preventDefault();
    event.stopPropagation();
    if (button.dataset.njsBusy === "1") return;
    button.dataset.njsBusy = "1";
    try {
      const reply = await send({
        type: "toggle",
        link: {
          key: button.dataset.njsKey,
          url: button.dataset.njsUrl,
          source: button.dataset.njsSource,
          label: button.dataset.njsLabel,
          page: Number(button.dataset.njsPage) || null,
        },
      });
      if (reply.error) throw new Error(reply.error);
      if (reply.added) clipped.add(button.dataset.njsKey);
      else clipped.delete(button.dataset.njsKey);
      paint();
      flash(button, reply.added ? "added" : "removed");
    } catch (error) {
      flash(button, "failed");
    } finally {
      delete button.dataset.njsBusy;
    }
  }

  function flash(button, kind) {
    button.dataset.njsFlash = kind;
    setTimeout(() => {
      if (button.dataset.njsFlash === kind) delete button.dataset.njsFlash;
    }, 900);
  }

  function paintOne(button, row) {
    const on = clipped.has(button.dataset.njsKey);
    button.dataset.njsState = on ? "on" : "off";
    button.setAttribute("aria-pressed", on ? "true" : "false");
    button.title = on ? "Clipped to nujijushi — click to remove" : "Clip to nujijushi";
    if (row) {
      if (on) row.dataset.njsClipped = "1";
      else delete row.dataset.njsClipped;
    }
  }

  function paint() {
    for (const button of document.querySelectorAll(".njs-btn[data-njs-key]")) {
      paintOne(button, button.closest("[data-njs-row]"));
    }
  }

  /** Idempotently wire one adapter item: create the toggle, remember its link. */
  function ensure(item) {
    if (!item || !item.url) return;
    const link = item.key ? item : root.canonical && root.canonical.canonicalize(item.url);
    if (!link) return;

    let button = item.button || null;
    if (!button) {
      const slot = item.slot || item.el;
      if (!slot) return;
      button = slot.querySelector(":scope > .njs-btn");
      if (!button) {
        button = createButton();
        slot.appendChild(button);
      }
    }

    button.dataset.njsKey = link.key;
    button.dataset.njsUrl = link.url;
    button.dataset.njsSource = link.source || "web";
    button.dataset.njsLabel = link.label || "";
    if (link.page) button.dataset.njsPage = String(link.page);
    else delete button.dataset.njsPage;

    const row = item.el || button.parentElement;
    if (row) row.dataset.njsRow = link.key;
    paintOne(button, row);
  }

  // --------------------------------------------------------------- scan & watch

  function adapterEnabled(adapter) {
    const flags = (settings && settings.adapters) || {};
    if (adapter.fallback) return flags.generic !== false;
    return flags[adapter.id] !== false;
  }

  function matches(adapter, location) {
    if (typeof adapter.match !== "function") return true;
    try {
      return !!adapter.match(location);
    } catch {
      return false;
    }
  }

  function scanAll() {
    if (scanning || !root.canonical) return;
    scanning = true;
    try {

      const here = new URL(location.href);
      const live = adapters.filter((adapter) => adapterEnabled(adapter) && matches(adapter, here));
      const primary = live.filter((adapter) => !adapter.fallback);
      // A page-level "clip this page" pill only when no site adapter understood the page.
      const chosen = primary.length ? primary : live.filter((adapter) => adapter.fallback);

      for (const adapter of chosen) {
        let items = [];
        try {
          items = adapter.scan(document, here) || [];
        } catch (error) {
          if (!warned.has(adapter.id)) {
            warned.add(adapter.id);
            console.warn("[nujijushi clip] adapter failed:", adapter.id, error);
          }
          continue;
        }
        for (const item of items) ensure(item);
      }
      paint();
    } finally {
      scanning = false;
    }
  }

  let timer = null;

  function scheduleScan() {
    if (timer) return;
    timer = setTimeout(() => {
      timer = null;
      // X and pixiv swap content without a reload, so every mutation rescans (debounced).
      scanAll();
    }, 150);
  }


  function observe() {
    if (observing) return;
    observing = true;
    const observer = new MutationObserver(scheduleScan);
    observer.observe(document.documentElement, { childList: true, subtree: true });
    window.addEventListener("popstate", scanAll);
    window.addEventListener("pageshow", scanAll);
  }

  // A single shared floating pill for the "clip this page" adapters.
  let pill = null;
  let pillButton = null;

  root.floatingClip = function floatingClip(link) {
    if (!pill) {
      pill = document.createElement("div");
      pill.className = "njs-pill";
      pillButton = createButton();
      pillButton.classList.add("njs-pill-btn");
      pill.appendChild(pillButton);
      (document.body || document.documentElement).appendChild(pill);
    }
    pill.hidden = false;
    return {
      key: link.key,
      url: link.url,
      source: link.source,
      label: link.label,
      page: link.page,
      button: pillButton,
      el: pill,
    };
  };

  root.hideFloatingClip = function hideFloatingClip() {
    if (pill) pill.hidden = true;
  };

  // ---------------------------------------------------------------------- boot

  async function boot() {
    if (started) return;
    started = true;
    try {
      await loadState();
    } catch (error) {
      console.warn("[nujijushi clip] could not read the bin:", error);
    }
    scanAll();
    observe();
  }

  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== "local" || !root.store || !changes[root.store.STORAGE_KEY]) return;
    loadState().then(paint, () => {});
  });

  setTimeout(boot, 0);
})();
