// The panel: a big button per announcement, and what's playing now.

import { api } from "./api.js";
import { h, replace, toast, saved } from "./dom.js";
import { state, subscribe, isOver, zoneName, siteName, activeRun, fireTime, fromNow, can, panelAnnouncements, onRunSettled } from "./store.js";
import { ordered } from "./layout.js";
import * as customize from "./customize.js";
import { runCard, runState, stopRun } from "./runs.js";
import { onTalk, pushToTalk, micProblem } from "./talk.js";

// How the Panel looks (Settings → Panel), with the defaults filled in.
export const LOOK_DEFAULTS = { size: "medium", mode: "tiles", show_details: true, show_next: true, show_playing: true, show_stats: true, confirm_emergency: false };
// The URL can set this screen's size and layout.
const look = () => {
  const l = { ...LOOK_DEFAULTS, ...(state.config.panel ?? {}) };
  if (state.view.size) l.size = state.view.size;
  if (state.view.mode) l.mode = state.view.mode;
  return l;
};

const categoryOf = (a) => (state.config.categories ?? []).find((c) => c.id === a.category);
const colorOf = (a) => a.color ?? categoryOf(a)?.color ?? null;
// Classes for a tile: priority, and its color if it has one.
const tileClass = (a, extra = "") => [
  "tile", a.priority ?? "normal", colorOf(a) && `colored color-${colorOf(a)}`, extra,
].filter(Boolean).join(" ");

let talkState = null;
// The level meter of the tile being talked on, updated in place.
let meter = null;

async function play(a) {
  const ask = a.confirm || (look().confirm_emergency && a.priority === "emergency");
  if (ask && !confirm(`Play "${a.name}"?`)) return;
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
    a.icon && h("span", { class: "icon-big", "aria-hidden": "true" }, a.icon),
    h("span", { class: "name" }, a.name),
    meta && h("span", { class: "meta" }, meta),
    h("span", { class: "talk-hint" }, mine?.phase === "live" && h("span", { class: "dot", "aria-hidden": "true" }), hint),
    mine && h("span", { class: "meter", "aria-hidden": "true" }, bar),
    a.priority && a.priority !== "normal" && h("span", { class: `badge ${a.priority}` }, a.priority));
  pushToTalk(button, a.id, a.name);
  return h("div", { class: tileClass(a, `live${run ? " active" : ""}${mine ? " on-air" : ""}`) },
    button,
    run && h("div", { class: "playing" },
      h("span", {}, runState(run)),
      h("button", { class: "danger", onclick: () => stopRun(run) }, "Stop")));
}

function tile(a) {
  const run = activeRun(a.id);
  const zones = [a.zones, ...(a.parts ?? []).map((p) => p.zones)].flat().map(zoneName).join(", ");
  const meta = look().show_details ? [zones, a.repeat > 1 && `${a.repeat}×`].filter(Boolean).join(" · ") : "";
  if (a.audio === "live") return liveTile(a, run, meta);
  return h("div", { class: tileClass(a, run ? "active" : "") },
    h("button", {
      class: "play",
      disabled: !!run,
      title: run ? "Playing" : `Play "${a.name}"`,
      onclick: () => play(a),
    },
      a.icon && h("span", { class: "icon-big", "aria-hidden": "true" }, a.icon),
      h("span", { class: "name" }, a.name),
      meta && h("span", { class: "meta" }, meta),
      a.priority && a.priority !== "normal" && h("span", { class: `badge ${a.priority}` }, a.priority)),
    run && h("div", { class: "playing" },
      h("span", {}, runState(run)),
      h("button", { class: "danger", onclick: () => stopRun(run) }, "Stop")));
}

// The tag the Panel is filtered to, per screen ("" for all).
let filter = state.view.tag ?? saved.get("panel.tag", "");
// Categories folded away on this screen.
const collapsed = new Set(saved.get("panel.collapsed", []));
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

function tagBar(anns) {
  // A screen whose URL picks the tag shows no chips.
  if (state.view.tag) return null;
  const customizeLink = can.edit() && !state.view.kiosk;
  const tags = [...new Map(anns.flatMap((a) => a.tags ?? []).map((t) => [t.toLowerCase(), t])).values()]
    .sort((a, b) => a.localeCompare(b));
  if (!tags.length && !customizeLink) return null;
  const chip = (label, value) => h("button", {
    class: `tag${filter.toLowerCase() === value.toLowerCase() ? " on" : ""}`,
    onclick: () => { filter = value; saved.set("panel.tag", value); renderPanel(); },
  }, label);
  return h("div", { class: "tag-bar panel-bar" },
    tags.length > 0 && [chip("All", ""), tags.map((t) => chip(t, t))],
    h("span", { class: "spacer" }),
    customizeLink && h("button", {
      class: "customize-open",
      title: "Make groups, and drag buttons and groups into place, for every screen",
      onclick: () => customize.begin(panelEl, { look, onDone: () => renderPanel() }),
    }, "✎ Customize"));
}

let renderPanel = () => {};
let panelEl = null;

// The stats strip (GET /stats), refreshed as plays end and every minute.
let stats = null;
async function loadStats() {
  try {
    stats = await api("GET", "/stats");
  } catch {
    stats = null;
  }
  renderPanel();
}

const plural = (n, one, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function statsStrip(next) {
  const b = state.backend;
  const item = (label, ...value) => h("div", { class: "stat" }, h("span", { class: "label" }, label), h("span", { class: "value" }, ...value));
  const bad = (text) => h("span", { class: "bad" }, text);
  const problems = (c) => [c.failed > 0 && bad(` · ${c.failed} failed`), c.missed > 0 && bad(` · ${c.missed} missed`)];
  const t = stats?.today, w = stats?.week, d = stats?.last_24h;
  const offline = state.speakers.speakers.filter((s) => s.online === false);
  return h("div", { class: "stats-strip" },
    b && !b.connected && item("mcast server", bad("unreachable")),
    offline.length > 0 && item("Speakers", bad(`${offline.map((s) => s.name).join(", ")} offline`)),
    look().show_next && next && item("Next", h("strong", {}, next.name), next.site && ` at ${siteName(next.site)}`,
      ` · ${fireTime(next)} `, h("span", { class: "muted" }, `(${fromNow(next.at_ms)})`)),
    t && item("Today", plural(t.plays, "play"), problems(t)),
    w && item("Last 7 days", plural(w.plays, "play"), problems(w)),
    d && item("Last 24 h", d.busy_groups + d.failed_groups === 0
      ? h("span", { class: "ok" }, "no busy or failed groups")
      : [d.busy_groups > 0 && bad(`${plural(d.busy_groups, "group")} busy`),
        d.busy_groups > 0 && d.failed_groups > 0 && ", ",
        d.failed_groups > 0 && bad(`${plural(d.failed_groups, "group")} failed`)]),
    !state.view.kiosk && h("a", { class: "link stat-more", href: "#history" }, "History →"));
}

export function mount(el) {
  panelEl = el;
  const render = () => {
    // Customizing draws its own page, and leaves it alone meanwhile.
    if (customize.editing()) return;
    if (state.configError) {
      replace(el, h("p", { class: "error" }, `The intercom config has a problem: ${state.configError}`));
      return;
    }
    const anns = panelAnnouncements();
    // A redraw replaces the buttons: keep the keyboard on the one it was on.
    const focused = document.activeElement?.dataset?.talk;
    const active = [...state.runs.values()].filter((r) => !isOver(r)).sort((a, b) => b.id - a.id);
    const next = state.upcoming.fires[0];
    replace(el,
      look().show_stats && statsStrip(next),
      !look().show_stats && look().show_next && next && h("p", { class: "next" },
        h("span", { class: "muted" }, "Next: "), h("strong", {}, next.name),
        next.site && ` at ${siteName(next.site)}`,
        ` · ${fireTime(next)} `, h("span", { class: "muted" }, `(${fromNow(next.at_ms)})`)),
      anns.length > 0 && tagBar(anns),
      anns.length
        ? groups(anns.filter((a) => !filter || (a.tags ?? []).some((t) => t.toLowerCase() === filter.toLowerCase())))
          .filter((g) => g.anns.length)
          .map((g) => {
            const shut = collapsed.has(g.id);
            const size = (state.config.categories ?? []).find((c) => c.id === g.id)?.size ?? look().size;
            return h("section", { class: "panel-group" },
              g.name && h("button", {
                class: "group-head", "aria-expanded": String(!shut),
                onclick: () => { shut ? collapsed.delete(g.id) : collapsed.add(g.id); saved.set("panel.collapsed", [...collapsed]); render(); },
              }, h("span", { "aria-hidden": "true" }, shut ? "▸" : "▾"), h("span", {}, g.name),
              shut && h("span", { class: "count" }, `(${g.anns.length})`)),
              !shut && h("div", { class: `tiles size-${size} mode-${look().mode}` }, g.anns.map(tile)));
          })
        : h("p", { class: "muted" }, state.config.announcements.length
          ? "Nothing to show here: this screen or token is limited to categories with no announcements."
          : "No announcements yet: add zones, then announcements, from the tabs above."),
      look().show_playing && active.length > 0 && h("h2", { style: { marginTop: "24px" } }, "Now playing"),
      look().show_playing && active.length > 0 && h("ul", { class: "runs" }, active.map(runCard)));
    if (focused) el.querySelector(`[data-talk="${CSS.escape(focused)}"]`)?.focus();
  };
  renderPanel = render;
  if (look().show_stats) loadStats();
  setInterval(() => { if (look().show_stats) loadStats(); }, 60000);
  // A play that ends changes the numbers.
  onRunSettled(() => { if (look().show_stats) loadStats(); });
  subscribe(["server"], render);
  // A tag that's gone shows everything.
  if (filter && !state.view.tag && !state.config.announcements.some((a) => (a.tags ?? []).some((t) => t.toLowerCase() === filter.toLowerCase()))) filter = "";
  subscribe(["config", "runs", "upcoming", "speakers"], render);
  subscribe(["config"], () => customize.configChanged());
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
