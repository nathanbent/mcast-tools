// Shared state: lists fetched from the API, kept current by events.
// Views subscribe and re-render the parts they show.

import { api } from "./api.js";

export const state = {
  jobs: new Map(),   // id -> job
  streams: [],
  media: [],
  devices: [],
  monitor: null,     // null: not loaded; false: monitor is off
  probes: new Map(), // id -> probe
  probeDefaults: null,
  alerts: { active: [], recent: [] }, // recent: newest first
  alertRules: null,
  texts: new Map(),  // text job id -> lines, oldest first
  phonebook: [],     // { name, group, description }, sorted by name
};

// The phonebook name saved for a group address, if any.
export function bookName(group) {
  return state.phonebook.find((e) => e.group === group)?.name ?? null;
}

// How a phonebook entry is shown next to its name in suggestions.
export function bookLabel(e) {
  return e.description ? `${e.group} · ${e.description}` : e.group;
}

const MAX_LINES = 200;

const subscribers = new Set();

export function subscribe(fn) {
  subscribers.add(fn);
}

// `detail` narrows it down where useful (the probe ID for "probes").
function changed(what, detail) {
  for (const fn of subscribers) fn(what, detail);
}

// Probe and alert events applied so far. A list fetched while one arrived
// would undo it, so it's fetched again instead of used.
const applied = { probes: 0, alerts: 0 };

// GET `path`, again if an event of `kind` arrived meanwhile.
async function fresh(kind, path) {
  for (;;) {
    const before = applied[kind];
    const value = await api("GET", path);
    if (applied[kind] === before) return value;
  }
}

export async function refresh(what) {
  switch (what) {
    case "jobs":
      state.jobs = new Map((await api("GET", "/jobs")).map((j) => [j.id, j]));
      for (const j of state.jobs.values()) if (j.kind === "text") loadText(j.id);
      break;
    case "streams":
      state.streams = await api("GET", "/streams");
      break;
    case "media":
      state.media = await api("GET", "/media");
      break;
    case "phonebook":
      state.phonebook = await api("GET", "/phonebook");
      break;
    case "devices":
      state.devices = (await api("GET", "/devices")).devices;
      break;
    case "monitor":
      try {
        state.monitor = await api("GET", "/monitor");
      } catch (e) {
        if (e.status !== 404) throw e;
        state.monitor = false;
      }
      break;
    case "probes":
      state.probes = new Map((await fresh("probes", "/probes")).map((p) => [p.id, p]));
      break;
    case "probe-defaults":
      state.probeDefaults = await api("GET", "/probe-defaults");
      what = "probes";
      break;
    case "alerts":
      state.alerts = await fresh("alerts", "/alerts");
      break;
    case "alert-rules":
      state.alertRules = await api("GET", "/alert-rules");
      what = "alerts";
      break;
  }
  changed(what);
}

export function refreshAll() {
  return Promise.all(["jobs", "streams", "media", "devices", "monitor", "phonebook", "probes", "alerts"].map(refresh));
}

export function putJob(job) {
  const isNew = !state.jobs.has(job.id);
  state.jobs.set(job.id, job);
  changed("jobs");
  // A text channel we haven't seen: fetch what was said so far.
  if (isNew && job.kind === "text" && !state.texts.has(job.id)) loadText(job.id);
}

export async function loadText(id) {
  try {
    state.texts.set(id, await api("GET", `/jobs/${id}/text`));
    changed("texts");
  } catch {}
}

function addLine(line) {
  const lines = state.texts.get(line.job) ?? [];
  lines.push(line);
  if (lines.length > MAX_LINES) lines.splice(0, lines.length - MAX_LINES);
  state.texts.set(line.job, lines);
  changed("texts");
}

export function apply(event) {
  switch (event.type) {
    case "job_started":
    case "job_updated":
    case "job_ended":
      putJob(event.job);
      break;
    case "stream_added":
    case "stream_changed":
    case "stream_removed":
      refresh("streams");
      break;
    case "monitor":
      state.monitor = event.snapshot;
      changed("monitor");
      break;
    case "alert_raised":
      applied.alerts++;
      state.alerts.active = state.alerts.active.filter((a) => a.id !== event.alert.id).concat(event.alert);
      changed("alerts");
      break;
    case "alert_resolved":
      applied.alerts++;
      state.alerts.active = state.alerts.active.filter((a) => a.id !== event.alert.id);
      state.alerts.recent = [event.alert, ...state.alerts.recent].slice(0, 200);
      changed("alerts");
      break;
    case "probe_updated":
      applied.probes++;
      state.probes.set(event.probe.id, event.probe);
      changed("probes", event.probe.id);
      break;
    case "probe_removed":
      applied.probes++;
      state.probes.delete(event.id);
      changed("probes", event.id);
      break;
    case "text_line":
      addLine(event.line);
      break;
    case "file_received":
      refresh("media");
      changed({ received: event.file });
      break;
    case "phonebook_changed":
      refresh("phonebook");
      break;
    case "lagged":
      refreshAll();
      break;
  }
}
