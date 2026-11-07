// The panel: a big button per announcement, and what's playing now.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, isOver, zoneName, activeRun, fireTime, fromNow } from "./store.js";
import { runCard, runState, stopRun } from "./runs.js";

async function play(a) {
  try {
    const run = await api("POST", `/announcements/${encodeURIComponent(a.id)}/play`);
    const busy = run.targets.filter((t) => t.state === "busy").length;
    if (busy) toast(`"${a.name}": ${busy} group${busy > 1 ? "s" : ""} busy`, "info");
  } catch (e) {
    toast(`Couldn't play "${a.name}": ${e.message}`, "error");
  }
}

function tile(a) {
  const run = activeRun(a.id);
  const zones = a.zones.map(zoneName).join(", ");
  const meta = [zones, a.repeat > 1 && `${a.repeat}×`].filter(Boolean).join(" · ");
  return h("div", { class: `tile ${a.priority ?? "normal"}${run ? " active" : ""}` },
    h("button", {
      class: "play",
      disabled: !!run,
      title: run ? "Playing" : `Play "${a.name}"`,
      onclick: () => play(a),
    },
      h("span", { class: "name" }, a.name),
      h("span", { class: "meta" }, meta),
      a.priority && a.priority !== "normal" && h("span", { class: `badge ${a.priority}` }, a.priority)),
    run && h("div", { class: "playing" },
      h("span", {}, runState(run)),
      h("button", { class: "danger", onclick: () => stopRun(run) }, "Stop")));
}

export function mount(el) {
  const render = () => {
    if (state.configError) {
      replace(el, h("p", { class: "error" }, `The intercom config has a problem: ${state.configError}`));
      return;
    }
    const anns = state.config.announcements;
    const active = [...state.runs.values()].filter((r) => !isOver(r)).sort((a, b) => b.id - a.id);
    const next = state.upcoming.fires[0];
    replace(el,
      next && h("p", { class: "next" },
        h("span", { class: "muted" }, "Next: "), h("strong", {}, next.name),
        ` · ${fireTime(next)} `, h("span", { class: "muted" }, `(${fromNow(next.at_ms)})`)),
      anns.length
        ? h("div", { class: "tiles" }, anns.map(tile))
        : h("p", { class: "muted" }, "No announcements yet: add zones, then announcements, from the tabs above."),
      active.length > 0 && h("h2", { style: { marginTop: "24px" } }, "Now playing"),
      active.length > 0 && h("ul", { class: "runs" }, active.map(runCard)));
  };
  subscribe(["config", "runs", "upcoming"], render);
  render();
}
