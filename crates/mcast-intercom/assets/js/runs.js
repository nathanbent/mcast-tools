// A run (one play of an announcement) as a card, for the panel and the history.

import { api } from "./api.js";
import { h, ago, toast } from "./dom.js";
import { isOver, atTime } from "./store.js";

const RUN_STATE = {
  playing: "playing", waiting: "between repeats", stopping: "stopping",
  done: "done", stopped: "stopped", failed: "failed", missed: "missed",
};

const TARGET_STATE = {
  starting: "starting", playing: "playing", done: "done", busy: "busy",
  preempted: "taken over", stopped: "stopped", failed: "failed",
};

export function runState(run) {
  const label = RUN_STATE[run.state] ?? run.state;
  return run.rounds > 1 && !isOver(run) ? `${label} · ${run.round} of ${run.rounds}` : label;
}

export async function stopRun(run) {
  try {
    await api("DELETE", `/runs/${run.id}`);
  } catch (e) {
    toast(`Couldn't stop "${run.name}": ${e.message}`, "error");
  }
}

function targetName(t) {
  return t.address && t.address !== t.group ? `${t.group} (${t.address})` : t.group;
}

export function runCard(run) {
  const played = run.targets.filter((t) => t.played > 0).length;
  return h("li", { class: `run ${run.state}` },
    h("div", { class: "run-head" },
      h("strong", {}, run.name),
      h("span", { class: `badge ${run.state}` }, runState(run)),
      run.priority !== "normal" && h("span", { class: `badge ${run.priority}` }, run.priority),
      h("span", { class: "spacer" }),
      h("span", { class: "muted small" },
        run.trigger.type === "schedule" ? `scheduled for ${atTime(run.trigger.at_ms)}` : ago(run.started_ms),
        run.trigger.by && ` · ${run.trigger.type === "api" ? "through the API by" : "by"} ${run.trigger.by}`,
        isOver(run) && run.targets.length > 0 && ` · played to ${played} of ${run.targets.length}`),
      !isOver(run) && h("button", { class: "danger", onclick: () => stopRun(run) }, "Stop")),
    run.error && h("div", { class: "error small" }, run.error),
    h("div", { class: "targets" },
      run.targets.flatMap((t) => [
        h("span", { class: "mono" }, targetName(t)),
        h("span", { class: `badge ${t.state}` }, TARGET_STATE[t.state] ?? t.state),
        t.error && h("span", { class: "err" }, t.error),
      ])));
}
