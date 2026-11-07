// Shared state: lists fetched from the API, kept current by events.
// Views subscribe and re-render the parts they show.

import { api } from "./api.js";

export const state = {
  jobs: new Map(),   // id -> job
  streams: [],
  media: [],
  devices: [],
  monitor: null,     // null: not loaded; false: monitor is off
  texts: new Map(),  // text job id -> lines, oldest first
  phonebook: [],     // { name, group, description }, sorted by name
  chimes: [],        // built-in chimes: { name, category, description, hidden }
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

function changed(what) {
  for (const fn of subscribers) fn(what);
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
    case "chimes":
      state.chimes = await api("GET", "/chimes");
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
  }
  changed(what);
}

export function refreshAll() {
  return Promise.all(["jobs", "streams", "media", "devices", "monitor", "phonebook", "chimes"].map(refresh));
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
    case "text_line":
      addLine(event.line);
      break;
    case "text_renamed":
      // A sender's name arrived after its first lines: they had its address.
      for (const l of state.texts.get(event.job) ?? []) {
        if (!l.own && l.ssrc === event.ssrc) l.from = event.name;
      }
      changed("texts");
      break;
    case "file_received":
      refresh("media");
      changed({ received: event.file });
      break;
    case "phonebook_changed":
      refresh("phonebook");
      break;
    case "media_changed":
      refresh("media");
      break;
    case "chimes_changed":
      refresh("chimes");
      break;
    case "lagged":
      refreshAll();
      break;
  }
}
