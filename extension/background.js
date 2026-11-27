/* nujijushi clip — service worker.
 *
 * Owns the bin, authenticated admin requests, toolbar badge and shortcuts.
 */
importScripts("lib/canonical.js", "lib/store.js", "lib/admin.js", "server.js");

const store = NJS.store;
const { canonicalize, splitUrls } = NJS.canonical;
const admin = NJS.admin;
const trustedStorage = chrome.storage.local.setAccessLevel({ accessLevel: "TRUSTED_CONTEXTS" });
const pendingImports = new Map();
let writes = Promise.resolve();

function serialize(write) {
  const result = writes.then(write);
  writes = result.catch(() => {});
  return result;
}

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
  if (area !== "local") return;
  if (changes[store.STORAGE_KEY]) refreshBadge();
  if (changes[store.STORAGE_KEY] || changes[CONNECTION_KEY]) {
    chrome.tabs.query({}).then((tabs) => {
      for (const tab of tabs) {
        if (tab.id != null) chrome.tabs.sendMessage(tab.id, { type: "refresh-bin" }).catch(() => {});
      }
    }).catch(() => {});
  }
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
  const result = await serialize(() => store.add(link));
  flashBadge(result.added || result.updated ? "✓" : "=");
  return result;
}

async function clipText(text) {
  const links = splitUrls(text).map((piece) => canonicalize(piece)).filter(Boolean);
  if (!links.length) {
    flashBadge("×");
    return { added: 0, duplicates: 0 };
  }
  const result = await serialize(() => store.addMany(links));
  flashBadge(result.added.length || result.updated.length ? "✓" : "=");
  return { added: result.added.length, updated: result.updated.length, duplicates: result.duplicates.length, count: result.count };
}

// -------------------------------------------------------------------- messages

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  handle(message, sender).then(sendResponse, (error) => sendResponse({ error: String((error && error.message) || error) }));
  return true; // async response
});

async function clipPost(raw, page) {
  const link = admin.pagePost(raw, page);
  if (!link) throw new Error("Could not recognize this post URL.");
  const config = await connection();
  if (!config.direct) {
    const result = await serialize(() => store.toggle(link));
    flashBadge(result.added ? "✓" : "−");
    return result;
  }
  const existing = pendingImports.get(link.url);
  if (existing) return existing;
  const submit = (async () => {
    const result = await adminPost(config, "import", { targets: link.url, reuse_active: "true" });
    if (result?.job_ids?.length !== 1 || !Number.isSafeInteger(result.job_ids[0])) {
      throw new Error("The server did not acknowledge the import. Try again.");
    }
    await serialize(() => store.recordImport(link));
    flashBadge("✓");
    return { added: true, jobId: result.job_ids[0] };
  })();
  pendingImports.set(link.url, submit);
  try {
    return await submit;
  } finally {
    pendingImports.delete(link.url);
  }
}

async function activeWatch() {
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  let page;
  if (tab?.id != null) {
    page = await chrome.tabs.sendMessage(tab.id, { type: "watch-page" }, { frameId: 0 }).catch(() => null);
  }
  const candidate = admin.watchCandidate(page?.url || tab?.url, page?.permalink);
  if (!candidate) throw new Error("Open a supported artist page or post, or paste the profile URL.");
  const result = await adminPost(await connection(), "watches/resolve", { url: candidate.url });
  return { url: result.url, name: result.name || candidate.name };
}

async function handleOptions(message) {
  switch (message?.type) {
    case "connection":
      return publicConnection(await connection());
    case "save-connection":
      return serialize(async () => {
        const config = await connectionFromInput(message);
        await saveConnection(config);
        return publicConnection(config);
      });
    case "test-connection":
      await adminSession(await connectionFromInput(message));
      return { connected: true };
    case "direct-mode":
      return serialize(async () => {
        const config = await connection();
        if (!config.token && message.enabled) throw new Error("Save a server and admin token first.");
        config.direct = message.enabled === true;
        await saveConnection(config);
        return publicConnection(config);
      });
    case "settings":
      return serialize(() => store.updateSettings(message.patch || {}));
    default:
      return { error: "unknown message" };
  }
}

async function handle(message, sender) {
  await trustedStorage;
  const type = message?.type;
  if (sender.id !== chrome.runtime.id) throw new Error("Unrecognized extension sender.");
  if (sender.tab && sender.frameId === 0 && admin.postPage(sender.url)) {
    if (type === "clip-post") return clipPost(message.url, sender.url);
    if (type === "clip-state") {
      const [state, config] = await Promise.all([store.read(), connection()]);
      return { keys: Object.keys(state.items), adapters: state.settings.adapters, direct: config.direct };
    }
  }
  if (sender.url === chrome.runtime.getURL("options.html")) return handleOptions(message);
  if (sender.url !== chrome.runtime.getURL("panel.html")) throw new Error("Unsupported request.");
  switch (type) {
    case "active-watch":
      return activeWatch();
    case "add-watch": {
      const result = await adminPost(await connection(), "watches", {
        url: message.url,
        name: message.name,
        initial_batch: message.initialBatch,
      });
      return { message: result.message };
    }
    case "clip-many": {
      const links = (message.urls || []).map((url) => canonicalize(url)).filter(Boolean);
      const skipped = (message.urls || []).length - links.length;
      const result = await serialize(() => store.addMany(links));
      flashBadge(result.added.length || result.updated.length ? "✓" : "=");
      return {
        added: result.added.length,
        updated: result.updated.length,
        duplicates: result.duplicates.length,
        skipped,
        count: result.count,
      };
    }
    case "remove":
      return serialize(() => store.remove(message.key));
    case "mark-copied":
      return serialize(() => store.markCopied(message.keys || []));
    case "clear-copied":
      return serialize(() => store.clearCopied());
    case "clear-all":
      return serialize(() => store.clearAll());
    default:
      return { error: "unknown message" };
  }
}
