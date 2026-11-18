/* nujijushi clip — side panel UI.
 *
 * Reads the bin straight from storage, sends every write through the service
 * worker (single writer), and turns the list into a newline-separated block that
 * the /import textarea eats as-is.
 */
(function () {
  "use strict";

  const store = NJS.store;
  const { splitUrls } = NJS.canonical;

  const els = {
    count: document.getElementById("count"),
    list: document.getElementById("list"),
    empty: document.getElementById("empty"),
    copy: document.getElementById("copy"),
    copyAll: document.getElementById("copy-all"),
    clear: document.getElementById("clear"),
    settings: document.getElementById("settings"),
    settingsToggle: document.getElementById("settings-toggle"),
    autoClear: document.getElementById("auto-clear"),
    adapterBoxes: [...document.querySelectorAll("input[data-adapter]")],
    paste: document.getElementById("paste"),
    pasteAdd: document.getElementById("paste-add"),
    status: document.getElementById("status"),
  };

  let items = [];
  let settings = { autoClear: false, adapters: {} };

  // ----------------------------------------------------------------- plumbing

  function send(message) {
    return new Promise((resolve) => {
      chrome.runtime.sendMessage(message, (reply) => {
        void chrome.runtime.lastError;
        resolve(reply || {});
      });
    });
  }

  function setStatus(text, kind) {
    els.status.textContent = text || "";
    if (kind) els.status.dataset.kind = kind;
    else delete els.status.dataset.kind;
  }

  async function refresh() {
    const state = await store.read();
    items = store.list(state);
    settings = state.settings;
    render();
  }

  // --------------------------------------------------------------------- view

  function render() {
    els.count.textContent = String(items.length);
    els.list.replaceChildren(...items.map(row));
    els.empty.hidden = items.length > 0;

    const fresh = items.filter((item) => !item.copied);
    const target = fresh.length ? fresh : items;
    els.copy.disabled = target.length === 0;
    els.copy.textContent = `copy ${target.length}`;
    els.copy.dataset.mode = fresh.length ? "fresh" : "all";

    els.copyAll.hidden = !(fresh.length && fresh.length < items.length);
    els.copyAll.textContent = `copy all ${items.length}`;

    els.clear.disabled = items.length === 0;

    els.autoClear.checked = !!settings.autoClear;
    for (const box of els.adapterBoxes) {
      box.checked = settings.adapters[box.dataset.adapter] !== false;
    }
  }

  function row(item) {
    const li = document.createElement("li");
    li.dataset.key = item.key;
    if (item.copied) li.dataset.copied = "1";

    const label = document.createElement("div");
    label.className = "label";
    label.textContent = item.label || item.url;
    label.title = item.url;

    const url = document.createElement("div");
    url.className = "url";
    url.textContent = shortUrl(item.url);
    url.title = item.url;

    const chip = document.createElement("div");
    chip.className = "chip";

    const source = document.createElement("span");
    source.className = "source";
    // "x ✕" reads like two close buttons; show the host for X instead.
    const sourceName = item.source === "x" ? "x.com" : item.source || "web";
    source.textContent = item.page && item.page > 1 ? `${sourceName} p${item.page}` : sourceName;

    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "remove";
    remove.textContent = "✕";
    remove.title = "Remove from the list";
    remove.addEventListener("click", () => removeItem(item.key));

    chip.append(source, remove);
    li.append(label, url, chip);
    return li;
  }

  function shortUrl(url) {
    return String(url).replace(/^https?:\/\/(www\.)?/, "");
  }

  // ------------------------------------------------------------------ actions

  async function removeItem(key) {
    await send({ type: "remove", key });
    await refresh();
  }

  async function copyTarget(mode) {
    const chosen = mode === "all" ? items : items.filter((item) => !item.copied);
    const list = chosen.length ? chosen : items;
    if (!list.length) return;

    const text = list.map((item) => item.url).join("\n");
    const ok = await writeClipboard(text);
    if (!ok) {
      setStatus("the clipboard was blocked — the list is still here, try again", "error");
      return;
    }

    await send({ type: "mark-copied", keys: list.map((item) => item.key) });
    if (settings.autoClear) {
      await send({ type: "clear-copied" });
      setStatus(`${list.length} copied, list cleared`, "ok");
    } else {
      setStatus(`${list.length} copied — paste into /import`, "ok");
    }
    await refresh();
  }

  async function writeClipboard(text) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // side panel can be unfocused; fall back to the old hidden-textarea trick
      try {
        const area = document.createElement("textarea");
        area.value = text;
        area.setAttribute("readonly", "");
        area.style.position = "fixed";
        area.style.opacity = "0";
        document.body.appendChild(area);
        area.select();
        const ok = document.execCommand("copy");
        area.remove();
        return ok;
      } catch {
        return false;
      }
    }
  }

  async function clearAll() {
    if (!items.length) return;
    const ok = confirm(`Remove all ${items.length} clipped link(s) from the list?`);
    if (!ok) return;
    await send({ type: "clear-all" });
    setStatus("list cleared");
    await refresh();
  }

  async function addPasted() {
    const raw = els.paste.value.trim();
    if (!raw) {
      setStatus("nothing to add", "error");
      return;
    }
    const urls = splitUrls(raw);
    const result = await send({ type: "clip-many", urls });
    if (result.error) {
      setStatus(result.error, "error");
      return;
    }
    const parts = [`${result.added} added`];
    if (result.duplicates) parts.push(`${result.duplicates} already there`);
    if (result.skipped) parts.push(`${result.skipped} not a link`);
    setStatus(parts.join(" · "), result.added ? "ok" : undefined);
    els.paste.value = "";
    await refresh();
  }

  async function saveSettings() {
    const adapters = {};
    for (const box of els.adapterBoxes) adapters[box.dataset.adapter] = box.checked;
    await send({ type: "settings", patch: { autoClear: els.autoClear.checked, adapters } });
    setStatus("settings saved — reload the tab to apply");
    await refresh();
  }

  // -------------------------------------------------------------------- wire

  els.copy.addEventListener("click", () => copyTarget(els.copy.dataset.mode || "fresh"));
  els.copyAll.addEventListener("click", () => copyTarget("all"));
  els.clear.addEventListener("click", clearAll);
  els.pasteAdd.addEventListener("click", addPasted);
  els.autoClear.addEventListener("change", saveSettings);
  for (const box of els.adapterBoxes) box.addEventListener("change", saveSettings);

  els.settingsToggle.addEventListener("click", () => {
    const open = els.settings.hidden;
    els.settings.hidden = !open;
    els.settingsToggle.setAttribute("aria-expanded", String(open));
  });

  els.paste.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      addPasted();
    }
  });

  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== "local" || !changes[store.STORAGE_KEY]) return;
    refresh();
  });

  refresh();
})();
