/* nujijushi clip — service worker.
 *
 * Single writer for the bin: content scripts and the panel read storage directly
 * but send their writes here, so a click during a panel action cannot drop an item.
 * Also owns the toolbar badge, the context menus and the keyboard commands.
 *
 * No network access anywhere in this extension — it only produces a newline list
 * that you paste into nujijushi's /import box.
 */
importScripts("lib/canonical.js", "lib/store.js");

const store = NJS.store;
const { canonicalize, splitUrls } = NJS.canonical;

const MENU = {
  page: "njs-clip-page",
  link: "njs-clip-link",
  selection: "njs-clip-selection",
};

let flashTimer = null;

// --------------------------------------------------------------------- install

chrome.runtime.onInstalled.addListener(() => {
  installMenus();
  enablePanelOnActionClick();
  refreshBadge();
});

chrome.runtime.onStartup.addListener(() => {
  refreshBadge();
});

chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes[store.STORAGE_KEY]) refreshBadge();
});

function installMenus() {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({ id: MENU.page, title: "Clip this page to nujijushi", contexts: ["page"] });
    chrome.contextMenus.create({ id: MENU.link, title: "Clip link to nujijushi", contexts: ["link"] });
    chrome.contextMenus.create({ id: MENU.selection, title: "Clip selected links", contexts: ["selection"] });
  });
}

function enablePanelOnActionClick() {
  try {
    chrome.sidePanel.setPanelBehavior({ openPanelOnActionClick: true }).catch(() => {});
  } catch {
    /* older Chrome: the toolbar button then does nothing, the shortcut still works */
  }
}

// ---------------------------------------------------------------------- badge

async function refreshBadge() {
  const state = await store.read().catch(() => null);
  const total = state ? store.count(state) : 0;
  await chrome.action.setBadgeText({ text: total ? String(total) : "" });
  await chrome.action.setBadgeBackgroundColor({ color: "#2f7d56" });
  await chrome.action.setTitle({ title: total ? `nujijushi clip — ${total} clipped` : "nujijushi clip" });
}

/** Momentary feedback on the badge: "✓" for added, "−" for removed, "×" for a miss. */
function flashBadge(text) {
  chrome.action.setBadgeText({ text });
  if (flashTimer) clearTimeout(flashTimer);
  flashTimer = setTimeout(() => {
    flashTimer = null;
    refreshBadge();
  }, 1200);
}

// --------------------------------------------------------------------- inputs

chrome.contextMenus.onClicked.addListener((info) => {
  if (info.menuItemId === MENU.link) clipOne(info.linkUrl);
  else if (info.menuItemId === MENU.page) clipOne(info.pageUrl);
  else if (info.menuItemId === MENU.selection) clipText(info.selectionText || "");
});

chrome.commands.onCommand.addListener((command) => {
  if (command === "clip-tab") clipActiveTab();
  else if (command === "open-panel") openPanel();
});

chrome.action.onClicked.addListener((tab) => {
  // Reached only if setPanelBehavior failed; harmless otherwise.
  if (tab && tab.id != null) openPanel(tab.id);
});

async function openPanel(tabId) {
  try {
    const id = tabId != null ? tabId : (await chrome.tabs.query({ active: true, currentWindow: true }))[0]?.id;
    if (id != null) await chrome.sidePanel.open({ tabId: id });
  } catch {
    /* no panel available (e.g. chrome:// page) */
  }
}

async function clipActiveTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || tab.id == null) return;
  let href = null;
  try {
    const [result] = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: () => location.href,
    });
    href = result && result.result;
  } catch {
    href = null; // chrome://, the web store, PDF viewer, ...
  }
  if (!href) {
    flashBadge("×");
    return;
  }
  await clipOne(href);
}

async function clipOne(raw) {
  const link = canonicalize(raw);
  if (!link) {
    flashBadge("×");
    return { added: false, reason: "invalid" };
  }
  const result = await store.add(link);
  flashBadge(result.added ? "✓" : "=");
  return result;
}

async function clipText(text) {
  const links = splitUrls(text).map((piece) => canonicalize(piece)).filter(Boolean);
  if (!links.length) {
    flashBadge("×");
    return { added: 0, duplicates: 0 };
  }
  const result = await store.addMany(links);
  flashBadge(result.added.length ? "✓" : "=");
  return { added: result.added.length, duplicates: result.duplicates.length, count: result.count };
}

// -------------------------------------------------------------------- messages

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  handle(message).then(sendResponse, (error) => sendResponse({ error: String((error && error.message) || error) }));
  return true; // async response
});

async function handle(message) {
  switch (message && message.type) {
    case "toggle": {
      const result = await store.toggle(message.link);
      flashBadge(result.added ? "✓" : "−");
      return result;
    }
    case "clip-many": {
      const links = (message.urls || []).map((url) => canonicalize(url)).filter(Boolean);
      const skipped = (message.urls || []).length - links.length;
      const result = await store.addMany(links);
      flashBadge(result.added.length ? "✓" : "=");
      return { added: result.added.length, duplicates: result.duplicates.length, skipped, count: result.count };
    }
    case "remove":
      return store.remove(message.key);
    case "mark-copied":
      return store.markCopied(message.keys || []);
    case "clear-copied":
      return store.clearCopied();
    case "clear-all":
      return store.clearAll();
    case "settings":
      return store.updateSettings(message.patch || {});
    default:
      return { error: "unknown message" };
  }
}
