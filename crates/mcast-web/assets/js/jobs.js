// Running jobs with live stats and a stop button, then recent ones.

import { api } from "./api.js";
import { h, replace, seconds, ago, toast } from "./dom.js";
import { state, subscribe, putJob, bookName } from "./store.js";

const RECENT = 8;
const ACTIVE = new Set(["starting", "running", "stopping"]);

async function stop(id) {
  try { putJob(await api("DELETE", `/jobs/${id}`)); } catch (e) { toast(e.message, "error"); }
}

function stats(job) {
  const s = job.stats || {};
  if (job.kind === "text") {
    return `${s.lines_sent ?? 0} sent · ${s.lines_received ?? 0} received · ${s.senders ?? 0} others heard`;
  }
  if (job.kind === "file_send") {
    if (!s.file) return "starting";
    const of = s.count > 1 ? ` (${s.index} of ${s.count})` : "";
    const pct = s.total ? ` · ${Math.min(100, Math.round((s.sent * 100) / s.total))} %` : "";
    return `${s.file}${of}${s.looping ? " · looping" : pct} · ${s.sent} packets`;
  }
  if (job.kind === "file_recv") {
    const now = s.receiving ? `receiving ${s.receiving} (${s.percent ?? 0} %)` : "waiting for files";
    return `${now} · ${s.received ?? 0} saved${s.failed ? ` · ${s.failed} failed` : ""}`;
  }
  if (job.kind === "send") {
    const parts = [seconds(s.seconds || 0), `${s.packets ?? 0} packets`];
    if (s.send_errors) parts.push(`${s.send_errors} send errors`);
    return parts.join(" · ");
  }
  const parts = [`${s.packets ?? 0} packets`, `${s.lost ?? 0} lost`, `buffer ${Math.round(s.buffered_ms ?? 0)} ms`];
  if (s.underruns) parts.push(`${s.underruns} underruns`);
  return parts.join(" · ");
}

const VERBS = { send: "Paging", recv: "Playing", text: "Text", file_send: "Sending files", file_recv: "Receiving files" };

function title(job) {
  const verb = VERBS[job.kind] ?? job.kind;
  const book = job.group && bookName(job.group);
  return `${verb} ${job.name ? `“${job.name}”` : book ? `“${book}” (${job.group})` : job.group ?? ""}`;
}

function activeCard(job) {
  return h("li", { class: `job ${job.kind}` },
    h("div", { class: "job-head" },
      h("strong", {}, title(job)),
      h("span", { class: `badge ${job.state}` }, job.state)),
    h("div", { class: "muted small" }, job.summary),
    h("div", { class: "stats" }, stats(job)),
    job.notes.length > 0 && h("div", { class: "muted small" }, job.notes.at(-1)),
    h("button", { class: "danger", disabled: job.state === "stopping", onclick: () => stop(job.id) },
      job.state === "stopping" ? "Stopping…" : "Stop"));
}

function recentRow(job) {
  return h("li", { class: "recent" },
    h("span", { class: `badge ${job.state}` }, job.state),
    h("span", {}, title(job)),
    h("span", { class: "muted small" }, job.error || `${stats(job)} · ${ago(job.ended_ms || job.created_ms)}`));
}

function render(el) {
  const jobs = [...state.jobs.values()].sort((a, b) => b.id - a.id);
  const active = jobs.filter((j) => ACTIVE.has(j.state));
  const recent = jobs.filter((j) => !ACTIVE.has(j.state)).slice(0, RECENT);
  replace(el,
    active.length
      ? h("ul", { class: "jobs" }, active.map(activeCard))
      : h("p", { class: "muted" }, "Nothing playing."),
    recent.length > 0 && h("details", { class: "recent-jobs" },
      h("summary", {}, `Recent (${recent.length})`),
      h("ul", {}, recent.map(recentRow))));
}

export function mount(el) {
  const body = h("div");
  el.append(body);
  // Keep the "Recent" section's open/closed state across re-renders.
  let open = false;
  body.addEventListener("toggle", (e) => { open = e.target.open; }, true);
  const update = () => {
    render(body);
    const d = body.querySelector("details");
    if (d) d.open = open;
  };
  update();
  subscribe((what) => (what === "jobs" || what === "phonebook") && update());
}
