// The panel: a big button per announcement, and what's playing now.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, isOver, zoneName, siteName, activeRun, fireTime, fromNow } from "./store.js";
import { runCard, runState, stopRun } from "./runs.js";
import { onTalk, pushToTalk, micProblem } from "./talk.js";

let talkState = null;
// The level meter of the tile being talked on, updated in place.
let meter = null;

async function play(a) {
  try {
    const run = await api("POST", `/announcements/${encodeURIComponent(a.id)}/play`);
    const busy = run.targets.filter((t) => t.state === "busy").length;
    if (busy) toast(`"${a.name}": ${busy} group${busy > 1 ? "s" : ""} busy`, "info");
  } catch (e) {
    toast(`Couldn't play "${a.name}": ${e.message}`, "error");
  }
}

// A live announcement: hold to talk, or tap for hands-free.
function liveTile(a, run, meta) {
  const mine = talkState?.id === a.id ? talkState : null;
  const hint = !mine
    ? micProblem() ? "Needs HTTPS for the microphone" : "Hold to talk · tap for hands-free"
    : { starting: "Connecting…", ending: "Finishing…" }[mine.phase]
      ?? (mine.latched ? "On air · tap to end" : "On air · let go to end");
  const bar = h("span", { class: "meter-fill" });
  if (mine) meter = bar;
  const button = h("button", {
    class: "play talk",
    "data-talk": a.id,
    // Someone else is talking on it from another page.
    disabled: !!run && !mine,
    title: "Hold to talk and let go to end, or tap to talk hands-free and tap again to end",
    "aria-pressed": String(!!mine),
  },
    h("span", { class: "name" }, a.name),
    h("span", { class: "meta" }, meta),
    h("span", { class: "talk-hint" }, mine?.phase === "live" && h("span", { class: "dot", "aria-hidden": "true" }), hint),
    mine && h("span", { class: "meter", "aria-hidden": "true" }, bar),
    a.priority && a.priority !== "normal" && h("span", { class: `badge ${a.priority}` }, a.priority));
  pushToTalk(button, a.id, a.name);
  return h("div", { class: `tile live ${a.priority ?? "normal"}${run ? " active" : ""}${mine ? " on-air" : ""}` },
    button,
    run && h("div", { class: "playing" },
      h("span", {}, runState(run)),
      h("button", { class: "danger", onclick: () => stopRun(run) }, "Stop")));
}

function tile(a) {
  const run = activeRun(a.id);
  const zones = a.zones.map(zoneName).join(", ");
  const meta = [zones, a.repeat > 1 && `${a.repeat}×`].filter(Boolean).join(" · ");
  if (a.audio === "live") return liveTile(a, run, meta);
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
    // A redraw replaces the buttons: keep the keyboard on the one it was on.
    const focused = document.activeElement?.dataset?.talk;
    const active = [...state.runs.values()].filter((r) => !isOver(r)).sort((a, b) => b.id - a.id);
    const next = state.upcoming.fires[0];
    replace(el,
      next && h("p", { class: "next" },
        h("span", { class: "muted" }, "Next: "), h("strong", {}, next.name),
        next.site && ` at ${siteName(next.site)}`,
        ` · ${fireTime(next)} `, h("span", { class: "muted" }, `(${fromNow(next.at_ms)})`)),
      anns.length
        ? h("div", { class: "tiles" }, anns.map(tile))
        : h("p", { class: "muted" }, "No announcements yet: add zones, then announcements, from the tabs above."),
      active.length > 0 && h("h2", { style: { marginTop: "24px" } }, "Now playing"),
      active.length > 0 && h("ul", { class: "runs" }, active.map(runCard)));
    if (focused) el.querySelector(`[data-talk="${CSS.escape(focused)}"]`)?.focus();
  };
  subscribe(["config", "runs", "upcoming"], render);
  onTalk((t) => {
    const changed = t?.id !== talkState?.id || t?.phase !== talkState?.phase || t?.latched !== talkState?.latched;
    talkState = t;
    if (changed) {
      meter = null;
      render();
    }
    if (meter && t) meter.style.width = `${Math.round(Math.min(1, t.level * 1.5) * 100)}%`;
  });
  render();
}
