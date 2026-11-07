// Shared state: lists fetched from the API, kept current by events.
// Views subscribe and re-render the parts they show.

import { api } from "./api.js";

export const state = {
  jobs: new Map(),   // id -> job
  streams: [],
  media: [],
  devices: [],
  monitor: null,     // null: not loaded; false: monitor is off
};

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
  }
  changed(what);
}

export function refreshAll() {
  return Promise.all(["jobs", "streams", "media", "devices", "monitor"].map(refresh));
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
    case "lagged":
      refreshAll();
      break;
  }
}
