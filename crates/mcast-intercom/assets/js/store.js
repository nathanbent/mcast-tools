// What the panel knows, kept current by server-sent events. Modules
// subscribe to the parts they show: "config", "runs" or "server".

import { api } from "./api.js";

export const state = {
  // Who's signed in, from /health: { name, role, announcements }. No token
  // on an open panel: admin, with no name.
  me: { name: null, role: "admin", announcements: [], categories: [] },
  // This screen's view, from the URL: ?kiosk (only the Panel, full screen),
  // categories=a,b and tag=x (what it shows), size= and mode= (how).
  view: (() => {
    const q = new URLSearchParams(location.search);
    const list = (k) => (q.get(k) ?? "").split(",").map((x) => x.trim()).filter(Boolean);
    return {
      kiosk: q.has("kiosk") && q.get("kiosk") !== "0",
      categories: list("categories"),
      tag: q.get("tag") || null,
      size: ["small", "medium", "large"].includes(q.get("size")) ? q.get("size") : null,
      mode: ["tiles", "list"].includes(q.get("mode")) ? q.get("mode") : null,
    };
  })(),
  config: { zones: [], announcements: [] },
  configError: null,
  runs: new Map(),
  media: [],
  phonebook: [],
  chimes: [],  // the mcast server's built-ins: { name, category, description, hidden }
  // The Sounds tab's view: { files: [{ name, bytes, modified_ms, hidden, used_by }],
  //                          folders: [{ name, hidden }] }, or an error message.
  library: { files: [], folders: [] },
  libraryError: null,
  backend: null,
  // { time_zone, now_ms, fires: [{ at_ms, local, weekday, schedule, announcement, name }] }
  upcoming: { time_zone: null, fires: [] },
  siteUpcoming: {},
  // GPIO inputs' states (admins): [{ id, name, enabled, pressed, error, changed_ms, run }].
  inputs: [],
  // Expected speakers between plays (GET /speakers): { known, error,
  // speakers: [{ name, zones, online, since_ms, last_seen_ms }] }.
  speakers: { known: false, error: null, speakers: [] },
};

// What the signed-in token may do (the server checks too).
export const can = {
  edit: () => state.me.role === "admin",
  operate: () => ["admin", "operator"].includes(state.me.role),
  changeSounds: () => ["admin", "media"].includes(state.me.role),
};

// The tabs each role sees.
export const TABS = {
  admin: ["panel", "history", "monitor", "schedules", "announcements", "playlists", "sounds", "zones", "settings"],
  operator: ["panel", "history", "monitor", "sounds"],
  media: ["sounds"],
  trigger: [],
};

// The announcements this screen shows on the Panel: those the token may
// play (an operator token can be limited to categories), narrowed by the
// URL's categories.
export function panelAnnouncements() {
  const mine = state.me.categories ?? [];
  const wanted = state.view.categories;
  return state.config.announcements.filter((a) =>
    (!mine.length || mine.includes(a.category))
    && (!wanted.length || wanted.includes(a.category ?? "other")));
}

const listeners = [];

export function subscribe(parts, fn) {
  listeners.push({ parts, fn });
}

function changed(part) {
  for (const l of listeners) if (l.parts.includes(part)) l.fn();
}

const endedListeners = [];
export function onRunEnded(fn) { endedListeners.push(fn); }
const speakerListeners = [];
// Speakers that went offline or came back: `fn([{ speaker, online }])`.
export function onSpeakersChanged(fn) { speakerListeners.push(fn); }
const settledListeners = [];
// Once a run is in the history: when it ends, or for one whose listeners
// are checked, a few seconds later. `fn(run, before)`: `before` is its last
// state seen, if any.
export function onRunSettled(fn) { settledListeners.push(fn); }

export const isOver = (run) => ["done", "stopped", "failed", "missed"].includes(run.state);

export async function refreshConfig() {
  try {
    state.config = await api("GET", "/config");
    state.configError = null;
  } catch (e) {
    state.configError = e.message;
  }
  changed("config");
}

// What's coming up: everything (`upcoming`), and each site's own
// (`siteUpcoming[id]`, with "" for schedules that have no site).
export async function refreshUpcoming() {
  const sites = (state.config.sites ?? []).map((s) => s.id);
  const keys = sites.length ? [...sites, ""] : [];
  const ask = (q) => api("GET", `/upcoming?n=10${q}`).catch(() => null);
  const [all, ...each] = await Promise.all([ask(""), ...keys.map((k) => ask(`&site=${encodeURIComponent(k)}`))]);
  // On failure the config has a problem; that's shown elsewhere.
  if (all) state.upcoming = all;
  state.siteUpcoming = Object.fromEntries(keys.map((k, i) => [k, each[i]]).filter(([, v]) => v));
  changed("upcoming");
}

export async function refreshRuns() {
  const runs = await api("GET", "/runs");
  state.runs = new Map(runs.map((r) => [r.id, r]));
  changed("runs");
}

export async function refreshInputs() {
  try { state.inputs = await api("GET", "/input-status"); } catch {}
  changed("inputs");
}

export async function refreshSpeakers() {
  try { state.speakers = await api("GET", "/speakers"); } catch {}
  changed("speakers");
}

export async function refreshChimes() {
  try { state.chimes = await api("GET", "/chimes"); } catch {}
}

// Media files, the phonebook, chimes and the connection to the mcast server.
export async function refreshServer() {
  const [media, phonebook, chimes, backend] = await Promise.all([
    api("GET", "/media").catch(() => null),
    api("GET", "/phonebook").catch(() => null),
    api("GET", "/chimes").catch(() => null),
    api("GET", "/backend").catch(() => null),
  ]);
  if (media) state.media = media;
  if (phonebook) state.phonebook = phonebook;
  if (chimes) state.chimes = chimes;
  if (backend) state.backend = backend;
  changed("server");
}

export async function refreshLibrary() {
  try {
    state.library = await api("GET", "/library");
    state.libraryError = null;
  } catch (e) {
    state.libraryError = e.message;
  }
  changed("library");
}

// Whether a media file is hidden from lists (it, or a folder it's in).
export function isHidden(name) {
  return (state.config.hidden_media ?? []).some((h) => name === h || name.startsWith(`${h}/`));
}

export function refreshAll() {
  // A media token sees only the Sounds tab.
  if (!can.operate()) return refreshLibrary();
  // What's coming up depends on the sites in the config.
  return Promise.all([refreshConfig().then(refreshUpcoming), refreshRuns().catch(() => {}), refreshServer(), refreshLibrary(), refreshSpeakers()]);
}

export function apply(ev) {
  switch (ev.type) {
    case "run": {
      const before = state.runs.get(ev.run.id);
      state.runs.set(ev.run.id, ev.run);
      changed("runs");
      // Ended now, or recorded already over (a missed schedule).
      if (isOver(ev.run) && (!before || !isOver(before))) {
        for (const fn of endedListeners) fn(ev.run);
      }
      // In the history now: it's over and, if its zones list listeners,
      // it's known who heard it.
      const settled = (r) => r && isOver(r) && !r.listeners?.checking;
      if (settled(ev.run) && !settled(before)) {
        for (const fn of settledListeners) fn(ev.run, before);
      }
      // A schedule played: what's next has moved on.
      if (!before && ev.run.trigger.type === "schedule") refreshUpcoming();
      break;
    }
    case "config_changed":
      if (!can.operate()) { refreshLibrary(); break; }
      refreshConfig().then(refreshUpcoming);
      // Hidden files, and which announcements use which.
      refreshLibrary();
      break;
    case "media_changed":
      if (can.operate()) refreshServer();
      refreshLibrary();
      break;
    case "backend": {
      const wasUp = state.backend?.connected;
      state.backend = { ...state.backend, ...ev.status };
      changed("server");
      // Back again: its media and phonebook may have changed meanwhile.
      if (ev.status.connected && !wasUp) { if (can.operate()) refreshServer(); refreshLibrary(); }
      break;
    }
    case "inputs":
      state.inputs = ev.inputs;
      changed("inputs");
      break;
    case "speakers":
      state.speakers = ev.status;
      changed("speakers");
      if (ev.changes.length) for (const fn of speakerListeners) fn(ev.changes);
      break;
    case "lagged":
      refreshAll();
      break;
  }
}

// "Mon 9 Nov 12:00" from a fire's wall-clock time in the schedules' zone.
export function fireTime(f) {
  const [date, time] = f.local.split("T");
  const [y, m, d] = date.split("-").map(Number);
  const day = new Date(Date.UTC(y, m - 1, d))
    .toLocaleDateString(undefined, { day: "numeric", month: "short", timeZone: "UTC" });
  return `${f.weekday} ${day} ${time.slice(0, 5)}`;
}

// "in 5 min", "in 3 h", "in 2 days".
export function fromNow(ms) {
  const s = Math.max(0, (ms - Date.now()) / 1000);
  if (s < 60) return "in under a minute";
  if (s < 3600) return `in ${Math.round(s / 60)} min`;
  if (s < 86400 * 2) return `in ${Math.round(s / 3600)} h`;
  return `in ${Math.round(s / 86400)} days`;
}

// A moment as wall-clock time in a site's time zone, or the panel's (the
// browser's if unknown).
export function atTime(ms, site) {
  const opts = { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", hourCycle: "h23" };
  const tz = (site && state.upcoming.sites?.[site]?.time_zone) || state.upcoming.time_zone;
  try {
    return new Date(ms).toLocaleString(undefined, { ...opts, timeZone: tz || undefined });
  } catch {
    return new Date(ms).toLocaleString(undefined, opts);
  }
}

export function announcementName(id) {
  return state.config.announcements.find((a) => a.id === id)?.name ?? id;
}

export function siteName(id) {
  return (state.config.sites ?? []).find((s) => s.id === id)?.name ?? id;
}

export function zoneName(id) {
  return state.config.zones.find((z) => z.id === id)?.name ?? id;
}

export function activeRun(announcementId) {
  let found = null;
  for (const r of state.runs.values()) {
    if (r.announcement === announcementId && !isOver(r) && (!found || r.id > found.id)) found = r;
  }
  return found;
}
