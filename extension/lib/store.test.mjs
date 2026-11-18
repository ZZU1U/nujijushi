/* Bin behaviour without a browser: dedupe, click order, copy marking.
 *
 *   node --test extension/lib/store.test.mjs
 *
 * lib/store.js only touches chrome.storage.local, so a tiny in-memory stand-in is
 * enough to prove the claims the panel makes ("no duplicates", "click order").
 */
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);

function fakeChrome() {
  const data = {};
  return {
    data,
    storage: {
      local: {
        async get(key) {
          return key in data ? { [key]: structuredClone(data[key]) } : {};
        },
        async set(patch) {
          for (const [key, value] of Object.entries(patch)) data[key] = structuredClone(value);
        },
      },
    },
  };
}

const chrome = fakeChrome();
globalThis.chrome = chrome;

const store = require("./store.js");
const { canonicalize } = require("./canonical.js");

function reset() {
  for (const key of Object.keys(chrome.data)) delete chrome.data[key];
}

test("the same post in different URL shapes is stored once", async () => {
  reset();
  const shapes = [
    "https://x.com/Artist/status/1",
    "https://twitter.com/Artist/status/1?s=20",
    "https://mobile.twitter.com/Artist/status/1/photo/1",
  ];
  const results = [];
  for (const shape of shapes) results.push(await store.add(canonicalize(shape)));

  assert.equal(results[0].added, true);
  assert.equal(results[1].added, false);
  assert.equal(results[1].reason, "duplicate");
  assert.equal(results[2].added, false);

  const state = await store.read();
  assert.equal(store.count(state), 1);
  assert.deepEqual(
    store.list(state).map((item) => item.key),
    ["x:1"],
  );
});

test("click order is preserved and unpicked items keep their place", async () => {
  reset();
  for (const url of ["https://x.com/a/status/3", "https://www.pixiv.net/artworks/1", "https://x.com/a/status/2"]) {
    await store.add(canonicalize(url));
  }
  const state = await store.read();
  assert.deepEqual(
    store.list(state).map((item) => item.url),
    ["https://x.com/a/status/3", "https://www.pixiv.net/artworks/1", "https://x.com/a/status/2"],
  );
});

test("toggle adds then removes the same key", async () => {
  reset();
  const link = canonicalize("https://x.com/a/status/9");
  assert.equal((await store.toggle(link)).added, true);
  assert.equal((await store.toggle(link)).removed, true);
  assert.equal(store.count(await store.read()), 0);
});

test("addMany reports duplicates instead of failing the batch", async () => {
  reset();
  await store.add(canonicalize("https://x.com/a/status/1"));
  const result = await store.addMany([
    canonicalize("https://x.com/a/status/1"),
    canonicalize("https://x.com/a/status/2"),
    canonicalize("https://twitter.com/a/status/2/photo/1"),
    null,
  ]);
  assert.deepEqual(result.added, ["x:2"]);
  assert.deepEqual(result.duplicates, ["x:1", "x:2"]);
  assert.equal(result.count, 2);
});

test("marking copied then clearing only drops the copied ones", async () => {
  reset();
  await store.add(canonicalize("https://x.com/a/status/1"));
  await store.add(canonicalize("https://x.com/a/status/2"));

  await store.markCopied(["x:1"]);
  let state = await store.read();
  assert.equal(store.list(state).find((item) => item.key === "x:1").copied, true);
  assert.equal(store.list(state).find((item) => item.key === "x:2").copied, false);

  const cleared = await store.clearCopied();
  assert.equal(cleared.removed, 1);
  state = await store.read();
  assert.deepEqual(
    store.list(state).map((item) => item.key),
    ["x:2"],
  );
});

test("settings merge instead of overwriting the adapter map", async () => {
  reset();
  await store.updateSettings({ autoClear: true });
  await store.updateSettings({ adapters: { pixiv: false } });
  const state = await store.read();
  assert.equal(state.settings.autoClear, true);
  assert.deepEqual(state.settings.adapters, { x: true, pixiv: false, generic: true });
});

test("clearAll empties the list but keeps settings", async () => {
  reset();
  await store.add(canonicalize("https://x.com/a/status/1"));
  await store.updateSettings({ autoClear: true });
  assert.equal((await store.clearAll()).removed, 1);
  const state = await store.read();
  assert.equal(store.count(state), 0);
  assert.equal(state.settings.autoClear, true);
});
