// What the panel knows, kept current by server-sent events. Modules
// subscribe to the parts they show: "config", "runs" or "server".

import { api } from "./api.js";

export const state = {
  config: { zones: [], announcements: [] },
  configError: null,
  runs: new Map(),
  media: [],
  phonebook: [],
  backend: null,
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

export const isOver = (run) => ["done", "stopped", "failed"].includes(run.state);

export async function refreshConfig() {
  try {
    state.config = await api("GET", "/config");
    state.configError = null;
  } catch (e) {
    state.configError = e.message;
  }
  changed("config");
}

export async function refreshRuns() {
  const runs = await api("GET", "/runs");
  state.runs = new Map(runs.map((r) => [r.id, r]));
  changed("runs");
}

// Media files, the phonebook and the connection to the mcast server.
export async function refreshServer() {
  const [media, phonebook, backend] = await Promise.all([
    api("GET", "/media").catch(() => null),
    api("GET", "/phonebook").catch(() => null),
    api("GET", "/backend").catch(() => null),
  ]);
  if (media) state.media = media;
  if (phonebook) state.phonebook = phonebook;
  if (backend) state.backend = backend;
  changed("server");
}

export function refreshAll() {
  return Promise.all([refreshConfig(), refreshRuns().catch(() => {}), refreshServer()]);
}

export function apply(ev) {
  switch (ev.type) {
    case "run": {
      const before = state.runs.get(ev.run.id);
      state.runs.set(ev.run.id, ev.run);
      changed("runs");
      if (isOver(ev.run) && before && !isOver(before)) {
        for (const fn of endedListeners) fn(ev.run);
      }
      break;
    }
    case "config_changed":
      refreshConfig();
      break;
    case "backend": {
      const wasUp = state.backend?.connected;
      state.backend = { ...state.backend, ...ev.status };
      changed("server");
      // Back again: its media and phonebook may have changed meanwhile.
      if (ev.status.connected && !wasUp) refreshServer();
      break;
    }
    case "lagged":
      refreshAll();
      break;
  }
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
