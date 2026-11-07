// The panel: a big button per announcement, and what's playing now.

import { api } from "./api.js";
import { h, replace, toast, saved } from "./dom.js";
import { state, subscribe, isOver, zoneName, siteName, activeRun, fireTime, fromNow, can } from "./store.js";
import { ordered, sortableList, saveLayouts, arrangeDialog } from "./layout.js";
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

// The tag the Panel is filtered to, per screen ("" for all).
let filter = saved.get("panel.tag", "");
const OTHER = "other"; // announcements in no category

// Categories in the Panel's order, each with its announcements in order:
// [{ id, name, anns }], the uncategorized last (as "Other", when there are
// categories at all).
function groups(anns) {
  const cats = state.config.categories ?? [];
  const byId = ordered("panel-buttons", anns.map((a) => a.id));
  const sorted = byId.map((id) => anns.find((a) => a.id === id));
  const known = (a) => cats.some((c) => c.id === a.category);
  const out = ordered("panel", cats.map((c) => c.id)).map((id) => ({
    id, name: cats.find((c) => c.id === id).name, anns: sorted.filter((a) => a.category === id),
  }));
  out.push({ id: OTHER, name: cats.length ? "Other" : null, anns: sorted.filter((a) => !known(a)) });
  return out;
}

function arrangePanel() {
  const all = state.config.announcements;
  const gs = groups(all);
  const catOrder = gs.filter((g) => g.id !== OTHER).map((g) => g.id);
  const buttonOrders = Object.fromEntries(gs.map((g) => [g.id, g.anns.map((a) => a.id)]));
  const name = (id) => all.find((a) => a.id === id)?.name ?? id;
  const catName = (id) => (state.config.categories ?? []).find((c) => c.id === id)?.name ?? id;
  arrangeDialog("Arrange the Panel",
    "Drag (or use the arrows) to order the categories, and the buttons in each. Everyone using the panel sees this. Choose an announcement's category in its editor.",
    h("div", {},
      catOrder.length > 1 && [h("h3", {}, "Categories"), sortableList(catOrder, catName)],
      gs.filter((g) => g.anns.length > 1).map((g) => [
        h("h3", {}, g.name ?? "Buttons"), sortableList(buttonOrders[g.id], name)])),
    () => saveLayouts({ panel: catOrder, "panel-buttons": gs.flatMap((g) => buttonOrders[g.id]) }, "Saved the Panel's arrangement"),
    () => saveLayouts({ panel: null, "panel-buttons": null }, "The Panel is back to the usual order"));
}

function tagBar(anns) {
  const tags = [...new Map(anns.flatMap((a) => a.tags ?? []).map((t) => [t.toLowerCase(), t])).values()]
    .sort((a, b) => a.localeCompare(b));
  if (!tags.length && !can.edit()) return null;
  const chip = (label, value) => h("button", {
    class: `tag${filter.toLowerCase() === value.toLowerCase() ? " on" : ""}`,
    onclick: () => { filter = value; saved.set("panel.tag", value); renderPanel(); },
  }, label);
  return h("div", { class: "tag-bar panel-bar" },
    tags.length > 0 && [chip("All", ""), tags.map((t) => chip(t, t))],
    h("span", { class: "spacer" }),
    can.edit() && h("button", { class: "link", onclick: arrangePanel }, "Arrange the Panel…"));
}

let renderPanel = () => {};

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
      anns.length > 0 && tagBar(anns),
      anns.length
        ? groups(anns.filter((a) => !filter || (a.tags ?? []).some((t) => t.toLowerCase() === filter.toLowerCase())))
          .filter((g) => g.anns.length)
          .map((g) => h("section", { class: "panel-group" },
            g.name && h("h3", { class: "group-head" }, g.name),
            h("div", { class: "tiles" }, g.anns.map(tile))))
        : h("p", { class: "muted" }, "No announcements yet: add zones, then announcements, from the tabs above."),
      active.length > 0 && h("h2", { style: { marginTop: "24px" } }, "Now playing"),
      active.length > 0 && h("ul", { class: "runs" }, active.map(runCard)));
    if (focused) el.querySelector(`[data-talk="${CSS.escape(focused)}"]`)?.focus();
  };
  renderPanel = render;
  // A tag that's gone shows everything.
  if (filter && !state.config.announcements.some((a) => (a.tags ?? []).some((t) => t.toLowerCase() === filter.toLowerCase()))) filter = "";
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
