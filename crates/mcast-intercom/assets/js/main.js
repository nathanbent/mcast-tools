// Start-up: check access, load everything, follow events, mount the tabs.

import { api, follow, setToken, token } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { refreshAll, refreshUpcoming, apply, state, subscribe, onRunEnded } from "./store.js";
import * as panel from "./panel.js";
import * as history from "./history.js";
import * as announcements from "./announcements.js";
import * as zones from "./zones.js";
import * as schedules from "./schedules.js";
import * as settings from "./settings.js";
import * as sounds from "./sounds.js";

const $ = (id) => document.getElementById(id);

function setStatus(s) {
  const el = $("status");
  el.dataset.state = s;
  el.textContent = { live: "Live", offline: "Reconnecting…", connecting: "Connecting…" }[s] ?? s;
}

function showBackend() {
  const b = state.backend;
  const el = $("backend");
  if (!b) return;
  el.dataset.state = b.connected ? "ok" : "bad";
  el.textContent = b.connected ? "mcast server" : "mcast server unreachable";
  el.title = b.connected ? b.url : `${b.url}: ${b.error ?? "not connected"}`;
}

function showPhonebook() {
  replace($("phonebook-groups"),
    state.phonebook.map((e) => h("option", { value: e.name }, `${e.group}${e.description ? ` · ${e.description}` : ""}`)));
}

function tabs() {
  const buttons = [...document.querySelectorAll("nav.tabs button")];
  const show = (name) => {
    if (!buttons.some((b) => b.dataset.tab === name)) name = "panel";
    for (const b of buttons) {
      const on = b.dataset.tab === name;
      b.setAttribute("aria-selected", String(on));
      $(`tab-${b.dataset.tab}`).hidden = !on;
    }
  };
  for (const b of buttons) b.onclick = () => { location.hash = b.dataset.tab; };
  window.addEventListener("hashchange", () => show(location.hash.slice(1)));
  show(location.hash.slice(1));
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
      return await api("GET", "/health");
    } catch (e) {
      if (e.status !== 401) throw e;
      await askToken(token() !== "");
    }
  }
}

async function allStop() {
  try {
    await api("DELETE", "/runs");
    toast("Stopped everything", "ok");
  } catch (e) {
    toast(`All stop failed: ${e.message}`, "error");
  }
}

async function start() {
  setStatus("connecting");
  try {
    await authenticate();
  } catch (e) {
    setStatus("offline");
    $("fatal").hidden = false;
    $("fatal").textContent = `Can't reach the intercom panel: ${e.message}`;
    return;
  }
  $("logout").hidden = token() === "";
  $("logout").onclick = () => { setToken(""); location.reload(); };
  $("all-stop").onclick = allStop;

  await refreshAll().catch(() => {});
  tabs();
  panel.mount($("panel"));
  history.mount($("history"));
  announcements.mount($("announcements"));
  zones.mount($("zones"));
  schedules.mount($("schedules"), $("days-off"));
  settings.mount($("tab-settings"));
  sounds.mount($("sounds"));
  // "in 5 min" moves on, and once-only times drop off the list.
  setInterval(() => refreshUpcoming(), 60000);
  subscribe(["server"], () => { showBackend(); showPhonebook(); });
  showBackend();
  showPhonebook();

  onRunEnded((run) => {
    const played = run.targets.filter((t) => t.played > 0).length;
    if (run.state === "done") toast(`"${run.name}" played to ${played} of ${run.targets.length} groups`, "ok");
    else if (run.state === "failed") toast(`"${run.name}": ${run.error ?? "failed"}`, "error");
    else if (run.state === "missed") toast(`"${run.name}": ${run.error}`, "info");
  });

  let first = true;
  follow(apply, (s) => {
    setStatus(s);
    // Events may have been missed while disconnected.
    if (s === "live" && !first) refreshAll().catch(() => {});
    if (s === "live") first = false;
  });
}

start();
