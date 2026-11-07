// Start-up: check access, load everything, follow events, mount the panels.

import { api, follow, setToken, token } from "./api.js";
import { refreshAll, apply } from "./store.js";
import * as page from "./page.js";
import * as jobs from "./jobs.js";
import * as streams from "./streams.js";
import * as monitor from "./monitor.js";
import * as media from "./media.js";

const $ = (id) => document.getElementById(id);

function setStatus(s) {
  const el = $("status");
  el.dataset.state = s;
  el.textContent = { live: "Live", offline: "Reconnecting…", connecting: "Connecting…" }[s] ?? s;
}

// Ask for the token until one works.
function askToken(wrong) {
  return new Promise((resolve) => {
    const dialog = $("login");
    $("login-error").hidden = !wrong;
    $("login-token").value = "";
    dialog.showModal();
    $("login-form").onsubmit = (e) => {
      e.preventDefault();
      setToken($("login-token").value.trim());
      dialog.close();
      resolve();
    };
  });
}

async function authenticate() {
  for (;;) {
    try {
      const health = await api("GET", "/health");
      $("version").textContent = `v${health.version}`;
      return;
    } catch (e) {
      if (e.status !== 401) throw e;
      await askToken(token() !== "");
    }
  }
}

async function start() {
  setStatus("connecting");
  try {
    await authenticate();
  } catch (e) {
    setStatus("offline");
    $("fatal").hidden = false;
    $("fatal").textContent = `Can't reach the mcast server: ${e.message}`;
    return;
  }
  $("logout").hidden = token() === "";
  $("logout").onclick = () => { setToken(""); location.reload(); };

  await refreshAll().catch(() => {});
  page.mount($("page"));
  jobs.mount($("jobs"));
  media.mount($("media"));
  streams.mount($("streams"));
  monitor.mount($("monitor"));

  let first = true;
  follow(apply, (s) => {
    setStatus(s);
    // Events may have been missed while disconnected.
    if (s === "live" && !first) refreshAll().catch(() => {});
    if (s === "live") first = false;
  });
}

start();
