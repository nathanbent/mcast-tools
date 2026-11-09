// Start-up: check access, load everything, follow events, mount the tabs.

import { api, follow, setToken, token } from "./api.js";
import { themeButton } from "./themes.js";
import { h, replace, toast } from "./dom.js";
import { refreshAll, refreshUpcoming, apply, state, subscribe, onRunEnded, onRunSettled, onSpeakersChanged, can, TABS } from "./store.js";
import * as panel from "./panel.js";
import * as history from "./history.js";
import * as monitor from "./monitor.js";
import * as announcements from "./announcements.js";
import * as zones from "./zones.js";
import * as schedules from "./schedules.js";
import * as settings from "./settings.js";
import * as sounds from "./sounds.js";
import * as playlists from "./playlists.js";
import * as display from "./display.js";

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

// Kiosk: only the Panel, nothing to wander off to, and the screen kept on.
function kiosk() {
  document.body.classList.add("kiosk");
  let lock = null;
  const keepAwake = async () => {
    try { if (document.visibilityState === "visible") lock = await navigator.wakeLock?.request("screen"); } catch {}
  };
  keepAwake();
  document.addEventListener("visibilitychange", keepAwake);
  const full = $("fullscreen");
  if (document.documentElement.requestFullscreen) {
    full.hidden = false;
    full.onclick = () => (document.fullscreenElement ? document.exitFullscreen() : document.documentElement.requestFullscreen()).catch(() => {});
  }
  void lock;
}

// The setup tabs, in a "More" menu when there are many tabs. On a phone,
// only the everyday tabs stay in the row, so it fits without scrolling.
const MORE = ["playlists", "sounds", "zones", "settings"];
const PHONE_TABS = ["panel", "history", "monitor"];
const narrow = window.matchMedia("(max-width: 760px)");

function moreMenu(buttons) {
  const pick = () => {
    if (narrow.matches && buttons.length > PHONE_TABS.length + 1) {
      return buttons.filter((b) => !PHONE_TABS.includes(b.dataset.tab));
    }
    return buttons.length > 6 ? buttons.filter((b) => MORE.includes(b.dataset.tab)) : [];
  };
  if (!pick().length && buttons.length <= PHONE_TABS.length + 1) return null;
  const nav = document.querySelector("nav.tabs");
  const toggle = h("button", { class: "more-toggle", "aria-haspopup": "menu", "aria-expanded": "false" }, "More ▾");
  const menu = h("div", { class: "more-menu", role: "menu", hidden: true });
  const box = h("div", { class: "more" }, toggle, menu);
  let inMore = [];
  let current = null;
  // Put each tab in the row or the menu, in their usual order.
  const place = () => {
    inMore = pick();
    for (const b of buttons) {
      if (inMore.includes(b)) {
        b.setAttribute("role", "menuitem");
        menu.append(b);
      } else {
        b.removeAttribute("role");
        nav.insertBefore(b, box);
      }
    }
    box.hidden = !inMore.length;
    showing(current);
  };
  // Say which of its tabs is showing, if one is.
  const showing = (name) => {
    current = name;
    const b = inMore.find((x) => x.dataset.tab === name);
    toggle.textContent = b ? `${b.textContent} ▾` : "More ▾";
    toggle.classList.toggle("current", !!b);
  };
  const close = () => { menu.hidden = true; toggle.setAttribute("aria-expanded", "false"); };
  const open = () => {
    // Fixed under the button: the tab row may scroll sideways (phones), which would clip it.
    const r = toggle.getBoundingClientRect();
    menu.style.top = `${r.bottom + 4}px`;
    menu.style.left = `${Math.max(8, Math.min(r.left, window.innerWidth - 190))}px`;
    menu.hidden = false;
    toggle.setAttribute("aria-expanded", "true");
    inMore[0].focus();
  };
  window.addEventListener("resize", () => close());
  window.addEventListener("scroll", () => close(), { passive: true });
  toggle.onclick = (e) => { e.stopPropagation(); menu.hidden ? open() : close(); };
  document.addEventListener("click", (e) => { if (!menu.contains(e.target)) close(); });
  menu.addEventListener("keydown", (e) => {
    const i = inMore.indexOf(document.activeElement);
    if (e.key === "Escape") { close(); toggle.focus(); }
    else if (e.key === "ArrowDown") { e.preventDefault(); inMore[(i + 1) % inMore.length].focus(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); inMore[(i - 1 + inMore.length) % inMore.length].focus(); }
  });
  nav.append(box);
  place();
  narrow.addEventListener("change", () => { close(); place(); });
  return { close, showing };
}

function tabs() {
  const allowed = (TABS[state.me.role] ?? []).filter((t) => !state.view.kiosk || t === "panel");
  const all = [...document.querySelectorAll("nav.tabs button")];
  for (const b of all) {
    if (!allowed.includes(b.dataset.tab)) {
      b.remove();
      $(`tab-${b.dataset.tab}`).remove();
    }
  }
  const buttons = all.filter((b) => allowed.includes(b.dataset.tab));
  const more = moreMenu(buttons);
  const show = (name) => {
    if (!buttons.some((b) => b.dataset.tab === name)) name = buttons[0]?.dataset.tab;
    for (const b of buttons) {
      const on = b.dataset.tab === name;
      b.setAttribute("aria-selected", String(on));
      $(`tab-${b.dataset.tab}`).hidden = !on;
    }
    more?.showing(name);
  };
  for (const b of buttons) b.onclick = () => { location.hash = b.dataset.tab; more?.close(); };
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
      const me = await api("GET", "/health");
      state.me = { name: me.name ?? null, role: me.role ?? "admin", announcements: me.announcements ?? [], categories: me.categories ?? [] };
      return me;
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
  themeButton(document.getElementById("theme"));
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
  if (state.me.name) {
    $("whoami").hidden = false;
    $("whoami").textContent = state.me.role === "admin" ? state.me.name : `${state.me.name} (${state.me.role})`;
    // Phones hide the name to keep the header to one row; it's on Sign out.
    $("logout").title = `Signed in as ${$("whoami").textContent}`;
  }
  if (state.me.role === "trigger") {
    setStatus("offline");
    $("all-stop").hidden = true;
    $("fatal").hidden = false;
    $("fatal").textContent = `"${state.me.name}" is a trigger token, for other systems to play announcements through the API. Sign out and use an operator or admin token to use this page.`;
    return;
  }
  if (state.view.display) return displayMode();
  if (can.operate()) $("all-stop").onclick = allStop;
  else $("all-stop").hidden = true;

  await refreshAll().catch(() => {});
  if (state.view.kiosk) kiosk();
  tabs();
  const mounts = {
    panel: () => panel.mount($("panel")),
    history: () => history.mount($("history")),
    monitor: () => monitor.mount($("monitor")),
    announcements: () => announcements.mount($("announcements")),
    zones: () => zones.mount($("zones")),
    schedules: () => schedules.mount($("schedules"), $("days-off"), $("schedule-groups")),
    settings: () => settings.mount($("tab-settings")),
    sounds: () => sounds.mount($("sounds")),
    playlists: () => playlists.mount($("playlists")),
  };
  for (const t of TABS[state.me.role] ?? []) if (!state.view.kiosk || t === "panel") mounts[t]?.();
  // "in 5 min" moves on, and once-only times drop off the list.
  if (can.operate()) setInterval(() => refreshUpcoming(), 60000);
  subscribe(["server"], () => { showBackend(); showPhonebook(); });
  showBackend();
  showPhonebook();

  onRunEnded((run) => {
    const played = run.targets.filter((t) => t.played > 0).length;
    if (run.state === "done") toast(`"${run.name}" played to ${played} of ${run.targets.length} groups`, "ok");
    else if (run.state === "failed") toast(`"${run.name}": ${run.error ?? "failed"}`, "error");
    else if (run.state === "missed") toast(`"${run.name}": ${run.error}`, "info");
  });
  // Speakers going offline between plays, and coming back.
  onSpeakersChanged((changes) => {
    for (const c of changes) {
      const where = c.speaker.zones.length ? ` (${c.speaker.zones.join(", ")})` : "";
      if (c.online) toast(`Speaker ${c.speaker.name}${where} is back`, "ok");
      else toast(`Speaker ${c.speaker.name}${where} is offline: it hasn't reported for a minute`, "error");
    }
  });
  // A speaker expected to hear a play didn't, or heard it badly (seen
  // live, not on loading).
  onRunSettled((run, before) => {
    if (!before) return;
    const list = (l) => (l.length === 1 ? l[0] : `${l.slice(0, -1).join(", ")} and ${l.at(-1)}`);
    const silent = run.listeners?.silent ?? [];
    const poor = run.listeners?.poor ?? [];
    if (silent.length) toast(`"${run.name}": ${list(silent)} didn't hear it`, "error");
    if (poor.length) toast(`"${run.name}": ${list(poor)} heard it badly, losing more than 5% of it`, "error");
  });

  let first = true;
  follow(apply, (s) => {
    setStatus(s);
    // Revoked or expired meanwhile: back to signing in.
    if (s === "offline") api("GET", "/health").catch((e) => { if (e.status === 401) location.reload(); });
    // Events may have been missed while disconnected.
    if (s === "live" && !first) refreshAll().catch(() => {});
    if (s === "live") first = false;
  });
}

// The display page: text pages for some zones, full screen, and nothing else.
async function displayMode() {
  document.body.classList.add("display-mode");
  kiosk();
  $("all-stop").hidden = true;
  $("tab-panel").hidden = true;
  $("tab-display").hidden = false;
  document.title = "Intercom display";
  await refreshAll().catch((e) => toast(`Can't load the text pages: ${e.message}`, "error"));
  display.mount($("display"));
  let first = true;
  follow(apply, (s) => {
    setStatus(s);
    if (s === "offline") api("GET", "/health").catch((e) => { if (e.status === 401) location.reload(); });
    if (s === "live" && !first) refreshAll().catch(() => {});
    if (s === "live") first = false;
  });
}

start();
