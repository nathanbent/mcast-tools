// Start-up: check access, load everything, follow events, mount the panels.

import { api, follow, setToken, token } from "./api.js";
import { refreshAll, apply, state, subscribe } from "./store.js";
import * as page from "./page.js";
import * as jobs from "./jobs.js";
import * as streams from "./streams.js";
import * as monitor from "./monitor.js";
import * as media from "./media.js";
import * as text from "./text.js";
import * as files from "./files.js";
import * as phonebook from "./phonebook.js";
import * as probes from "./probes.js";

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

// Two views: the console (#) and the probes (#probes).
function route() {
  const view = location.hash === "#probes" ? "probes" : "console";
  $("view-console").hidden = view !== "console";
  $("view-probes").hidden = view !== "probes";
  for (const a of document.querySelectorAll("nav a")) {
    if (a.dataset.view === view) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  document.title = view === "probes" ? "Probes · mcast" : "mcast";
}

// Active alerts, on the nav tab.
function probeBadge() {
  const n = state.alerts.active.length;
  $("probe-alerts").hidden = n === 0;
  $("probe-alerts").textContent = n;
  $("probe-alerts").title = `${n} active alert${n === 1 ? "" : "s"}`;
}

async function start() {
  route();
  addEventListener("hashchange", route);
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
  text.mount($("text"));
  files.mount($("files"));
  streams.mount($("streams"));
  monitor.mount($("monitor"));
  phonebook.mount($("phonebook"));
  probes.mount($("probes"));
  probeBadge();
  subscribe((what) => what === "alerts" && probeBadge());

  let first = true;
  follow(apply, (s) => {
    setStatus(s);
    // Events may have been missed while disconnected.
    if (s === "live" && !first) refreshAll().catch(() => {});
    if (s === "live") first = false;
  });
}

start();
