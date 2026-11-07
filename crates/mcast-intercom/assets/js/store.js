// What the panel knows, kept current by server-sent events. Modules
// subscribe to the parts they show: "config", "runs" or "server".

import { api } from "./api.js";

export const state = {
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
};

const listeners = [];

export function subscribe(parts, fn) {
  listeners.push({ parts, fn });
}

function changed(part) {
  for (const l of listeners) if (l.parts.includes(part)) l.fn();
}

const endedListeners = [];
export function onRunEnded(fn) { endedListeners.push(fn); }

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

export async function refreshUpcoming() {
  try {
    state.upcoming = await api("GET", "/upcoming?n=10");
  } catch {
    // The config has a problem; that's shown elsewhere.
  }
  changed("upcoming");
}

export async function refreshRuns() {
  const runs = await api("GET", "/runs");
  state.runs = new Map(runs.map((r) => [r.id, r]));
  changed("runs");
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
  return Promise.all([refreshConfig(), refreshRuns().catch(() => {}), refreshServer(), refreshUpcoming(), refreshLibrary()]);
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
      // A schedule played: what's next has moved on.
      if (!before && ev.run.trigger.type === "schedule") refreshUpcoming();
      break;
    }
    case "config_changed":
      refreshConfig();
      refreshUpcoming();
      // Hidden files, and which announcements use which.
      refreshLibrary();
      break;
    case "media_changed":
      refreshServer();
      refreshLibrary();
      break;
    case "backend": {
      const wasUp = state.backend?.connected;
      state.backend = { ...state.backend, ...ev.status };
      changed("server");
      // Back again: its media and phonebook may have changed meanwhile.
      if (ev.status.connected && !wasUp) { refreshServer(); refreshLibrary(); }
      break;
    }
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

// A moment as wall-clock time in the schedules' zone (the browser's if unknown).
export function atTime(ms) {
  const opts = { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit", hourCycle: "h23" };
  try {
    return new Date(ms).toLocaleString(undefined, { ...opts, timeZone: state.upcoming.time_zone || undefined });
  } catch {
    return new Date(ms).toLocaleString(undefined, opts);
  }
}

export function announcementName(id) {
  return state.config.announcements.find((a) => a.id === id)?.name ?? id;
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
