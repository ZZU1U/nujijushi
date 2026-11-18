/* nujijushi clip — the link bin.
 *
 * One storage row (`njs.bin`) holding a keyed map plus settings:
 *
 *   { v: 1,
 *     items: { "x:123": { url, source, label, page, addedAt, seq, copied } },
 *     settings: { autoClear: false, adapters: { x: true, pixiv: true, generic: true } } }
 *
 * A map (not an array) so dedupe and toggle are O(1); `seq` keeps click order.
 *
 * Reads happen from any context. Writes are serialised by the service worker:
 * content scripts and the panel send messages (see background.js) so two clicks
 * from different contexts cannot lose an update.
 */
(function (root, factory) {
  const api = factory();
  root.NJS = root.NJS || {};
  root.NJS.store = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  const STORAGE_KEY = "njs.bin";
  const DEFAULT_SETTINGS = Object.freeze({
    autoClear: false,
    adapters: Object.freeze({ x: true, pixiv: true, generic: true }),
  });

  function empty() {
    return { v: 1, items: {}, settings: { autoClear: false, adapters: { ...DEFAULT_SETTINGS.adapters } } };
  }

  function normalize(raw) {
    const state = empty();
    if (!raw || typeof raw !== "object") return state;
    if (raw.items && typeof raw.items === "object") state.items = raw.items;
    if (raw.settings && typeof raw.settings === "object") {
      state.settings = { ...state.settings, ...raw.settings };
      state.settings.adapters = { ...empty().settings.adapters, ...(raw.settings.adapters || {}) };
    }
    return state;
  }

  async function read() {
    const got = await chrome.storage.local.get(STORAGE_KEY);
    return normalize(got[STORAGE_KEY]);
  }

  async function write(state) {
    await chrome.storage.local.set({ [STORAGE_KEY]: state });
  }

  function count(state) {
    return Object.keys(state.items).length;
  }

  function list(state) {
    return Object.keys(state.items)
      .map((key) => ({ key, ...state.items[key] }))
      .sort((a, b) => (a.seq || 0) - (b.seq || 0));
  }

  function nextSeq(items) {
    let max = 0;
    for (const item of Object.values(items)) max = Math.max(max, item.seq || 0);
    return max + 1;
  }

  function insert(state, link) {
    state.items[link.key] = {
      url: link.url,
      source: link.source || "web",
      label: link.label || "",
      page: link.page || null,
      addedAt: Date.now(),
      seq: nextSeq(state.items),
      copied: false,
    };
    return state.items[link.key];
  }

  async function add(link) {
    if (!link || !link.key) return { added: false, reason: "invalid", count: 0 };
    const state = await read();
    if (state.items[link.key]) {
      return { added: false, reason: "duplicate", item: state.items[link.key], count: count(state) };
    }
    const item = insert(state, link);
    await write(state);
    return { added: true, item, count: count(state) };
  }

  /** Add many at once: one read, one write. Reports duplicates instead of failing. */
  async function addMany(links) {
    const incoming = (links || []).filter((link) => link && link.key);
    const state = await read();
    const result = { added: [], duplicates: [], count: count(state) };
    for (const link of incoming) {
      if (state.items[link.key]) {
        result.duplicates.push(link.key);
        continue;
      }
      insert(state, link);
      result.added.push(link.key);
    }
    if (result.added.length) await write(state);
    result.count = count(state);
    return result;
  }

  async function remove(key) {
    const state = await read();
    if (!state.items[key]) return { removed: false, count: count(state) };
    delete state.items[key];
    await write(state);
    return { removed: true, count: count(state) };
  }

  async function toggle(link) {
    if (!link || !link.key) return { added: false, reason: "invalid", count: 0 };
    const state = await read();
    if (state.items[link.key]) {
      delete state.items[link.key];
      await write(state);
      return { added: false, removed: true, count: count(state) };
    }
    insert(state, link);
    await write(state);
    return { added: true, count: count(state) };
  }

  async function markCopied(keys) {
    const wanted = new Set(keys || []);
    const state = await read();
    let changed = 0;
    for (const key of wanted) {
      const item = state.items[key];
      if (item && !item.copied) {
        item.copied = true;
        changed += 1;
      }
    }
    if (changed) await write(state);
    return { changed, count: count(state) };
  }

  async function clearCopied() {
    const state = await read();
    let removed = 0;
    for (const [key, item] of Object.entries(state.items)) {
      if (item.copied) {
        delete state.items[key];
        removed += 1;
      }
    }
    if (removed) await write(state);
    return { removed, count: count(state) };
  }

  async function clearAll() {
    const state = await read();
    const removed = count(state);
    state.items = {};
    await write(state);
    return { removed, count: 0 };
  }

  async function updateSettings(patch) {
    const state = await read();
    if (patch && typeof patch === "object") {
      const next = { ...state.settings, ...patch };
      next.adapters = { ...state.settings.adapters, ...(patch.adapters || {}) };
      state.settings = next;
      await write(state);
    }
    return state.settings;
  }

  function urls(items) {
    return (items || []).map((item) => item.url).filter(Boolean);
  }

  return {
    STORAGE_KEY,
    DEFAULT_SETTINGS,
    read,
    write,
    count,
    list,
    add,
    addMany,
    remove,
    toggle,
    markCopied,
    clearCopied,
    clearAll,
    updateSettings,
    urls,
  };
});
