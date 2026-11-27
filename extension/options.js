(function () {
  "use strict";

  const byId = (id) => document.getElementById(id);
  const form = byId("connection-form");
  const connection = byId("connection");
  const server = byId("server-url");
  const token = byId("admin-token");
  const direct = byId("direct-mode");
  const autoClear = byId("auto-clear");
  const adapters = [...document.querySelectorAll("input[data-adapter]")];
  const status = byId("status");

  function setStatus(message, kind = "") {
    status.textContent = message;
    status.dataset.kind = kind;
  }

  async function request(message) {
    const result = await chrome.runtime.sendMessage(message);
    if (!result || result.error) throw new Error(result?.error || "Could not contact the extension.");
    return result;
  }

  function showConnection(config) {
    server.value = config.base;
    token.placeholder = config.hasToken ? "Saved token" : "";
    direct.checked = config.direct;
  }

  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const address = NJS.admin.serverAddress(server.value);
    if (!address) {
      setStatus("Enter an HTTPS server URL or a local HTTP address.", "error");
      return;
    }
    const testing = event.submitter?.id === "test-connection";
    const secret = token.value;
    connection.disabled = true;
    setStatus(testing ? "Testing connection…" : "Saving connection…");
    try {
      if (!(await chrome.permissions.request({ origins: [address.permission] }))) {
        throw new Error("Server access was not granted.");
      }
      const result = await request({
        type: testing ? "test-connection" : "save-connection",
        base: address.base, token: secret,
      });
      if (!testing) {
        token.value = "";
        showConnection(result);
      }
      setStatus(testing ? "Connected to nujijushi." : "Connection saved.", "ok");
    } catch (error) {
      setStatus(error.message, "error");
    } finally {
      connection.disabled = false;
    }
  });

  function saveCheckbox(input, message) {
    input.addEventListener("change", async () => {
      const checked = input.checked;
      input.disabled = true;
      try {
        await request(message(checked));
        setStatus("Options saved.", "ok");
      } catch (error) {
        input.checked = !checked;
        setStatus(error.message, "error");
      } finally {
        input.disabled = false;
      }
    });
  }

  saveCheckbox(direct, (enabled) => ({ type: "direct-mode", enabled }));
  saveCheckbox(autoClear, (checked) => ({ type: "settings", patch: { autoClear: checked } }));
  for (const input of adapters) {
    saveCheckbox(input, (checked) => ({
      type: "settings", patch: { adapters: { [input.dataset.adapter]: checked } },
    }));
  }

  Promise.all([request({ type: "connection" }), NJS.store.read()])
    .then(([config, { settings }]) => {
      showConnection(config);
      autoClear.checked = settings.autoClear;
      for (const input of adapters) input.checked = settings.adapters[input.dataset.adapter] !== false;
      connection.disabled = false;
      byId("preferences").disabled = false;
      byId("site-buttons").disabled = false;
    })
    .catch((error) => setStatus(error.message, "error"));
})();
