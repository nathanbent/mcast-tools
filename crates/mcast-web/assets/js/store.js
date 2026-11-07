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
};

const subscribers = new Set();

export function subscribe(fn) {
  subscribers.add(fn);
}

// `detail` narrows it down where useful (the probe ID for "probes").
function changed(what, detail) {
  for (const fn of subscribers) fn(what, detail);
}

export async function refresh(what) {
  switch (what) {
    case "jobs":
      state.jobs = new Map((await api("GET", "/jobs")).map((j) => [j.id, j]));
      break;
    case "streams":
      state.streams = await api("GET", "/streams");
      break;
    case "media":
      state.media = await api("GET", "/media");
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
      state.probes = new Map((await api("GET", "/probes")).map((p) => [p.id, p]));
      break;
    case "probe-defaults":
      state.probeDefaults = await api("GET", "/probe-defaults");
      what = "probes";
      break;
  }
  changed(what);
}

export function refreshAll() {
  return Promise.all(["jobs", "streams", "media", "devices", "monitor", "probes"].map(refresh));
}

export function putJob(job) {
  state.jobs.set(job.id, job);
  changed("jobs");
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
    case "probe_updated":
      state.probes.set(event.probe.id, event.probe);
      changed("probes", event.probe.id);
      break;
    case "probe_removed":
      state.probes.delete(event.id);
      changed("probes", event.id);
      break;
    case "lagged":
      refreshAll();
      break;
  }
}
