/* The worker is the only context that stores credentials or calls the admin API. */
"use strict";

const CONNECTION_KEY = "njs.connection";

async function connection() {
  const data = await chrome.storage.local.get(CONNECTION_KEY);
  return data[CONNECTION_KEY] || { base: NJS.admin.DEFAULT_SERVER, token: "", direct: false };
}

async function saveConnection(next) {
  await chrome.storage.local.set({ [CONNECTION_KEY]: next });
}

function publicConnection(config) {
  return { base: config.base, hasToken: !!config.token, direct: config.direct };
}

async function connectionFromInput(input) {
  const address = NJS.admin.serverAddress(input.base);
  if (!address) throw new Error("Enter an HTTPS server URL or a local HTTP address.");
  if (!(await chrome.permissions.contains({ origins: [address.permission] }))) {
    throw new Error("Allow access to the server in extension settings.");
  }
  const saved = await connection();
  const token = typeof input.token === "string" ? input.token.trim() : "";
  const selected = token || (address.base === saved.base ? saved.token : "");
  if (!selected || !/^[\x21-\x7e]+$/.test(selected)) {
    throw new Error("Enter the nujijushi admin token.");
  }
  return { base: address.base, token: selected, direct: saved.direct };
}

async function requestAdmin(config, route, body) {
  if (!config.token) throw new Error("Set up the server and admin token in extension settings.");
  let response;
  try {
    response = await fetch(config.base + "/api/v1/" + route, {
      method: body ? "POST" : "GET",
      headers: { Authorization: "Bearer " + config.token, Accept: "application/json" },
      body,
      credentials: "omit",
      redirect: "error",
      cache: "no-store",
      signal: AbortSignal.timeout(20000),
    });
  } catch {
    throw new Error("Could not reach nujijushi. Check the server URL and try again.");
  }
  if (response.status === 401) throw new Error("The server rejected the admin token.");
  let result;
  try {
    result = await response.json();
  } catch {
    throw new Error("Expected a nujijushi API response. Check the server URL.");
  }
  if (!response.ok) {
    throw new Error(
      route !== "import" && response.status === 400 && typeof result?.error === "string"
        ? result.error : `The server could not complete the request (HTTP ${response.status}).`,
    );
  }
  return result;
}

async function adminSession(config) {
  const session = await requestAdmin(config, "session");
  if (!session || typeof session.csrf !== "string" || !session.csrf) {
    throw new Error("Expected a nujijushi session response. Check the server URL.");
  }
  return session;
}

async function adminPost(config, route, fields) {
  const { csrf } = await adminSession(config);
  return requestAdmin(config, route, new URLSearchParams({ ...fields, csrf }));
}
