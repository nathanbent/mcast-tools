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

// Who heard it: speakers its zones expect, from their receiver reports.
function heardLine(l) {
  if (!l) return null;
  const expected = l.expected.length;
  const heardExpected = l.expected.filter((e) => l.heard.some((x) => x.name.toLowerCase() === e.toLowerCase())).length;
  const others = l.heard.filter((x) => !l.expected.some((e) => e.toLowerCase() === x.name.toLowerCase()));
  const worst = Math.max(0, ...l.heard.map((x) => x.loss_percent ?? 0));
  const names = (list) => list.join(", ");
  if (l.error) return h("div", { class: "muted small" }, `Couldn't tell which speakers heard it: ${l.error}`);
  if (l.checking) {
    return h("div", { class: "muted small heard" },
      expected ? `Listening for speakers… heard by ${heardExpected} of ${expected}` : "Listening for speakers…");
  }
  if (!expected) return others.length ? h("div", { class: "muted small heard" }, `Heard by ${names(others.map((x) => x.name))}`) : null;
  return h("div", { class: "small heard" },
    h("span", { class: l.silent.length ? "warn" : "ok", title: names(l.heard.map((x) => x.name)) },
      `Heard by ${heardExpected} of ${expected}`),
    l.silent.length > 0 && h("span", { class: "bad" }, ` · silent: ${names(l.silent)}`),
    others.length > 0 && h("span", { class: "muted" }, ` · also ${names(others.map((x) => x.name))}`),
    worst >= 1 && h("span", { class: "muted" }, ` · up to ${worst.toFixed(1)}% loss`));
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
    heardLine(run.listeners),
    h("div", { class: "targets" },
      run.targets.flatMap((t) => [
        h("span", { class: "mono" }, targetName(t)),
        h("span", { class: `badge ${t.state}` }, TARGET_STATE[t.state] ?? t.state),
        t.error && h("span", { class: "err" }, t.error),
      ])));
}
