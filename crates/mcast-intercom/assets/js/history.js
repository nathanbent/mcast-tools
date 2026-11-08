// The History tab: plays that ended, from the panel's log on disk (kept for
// Settings → History's number of days), newest first, with filters, more on
// request, and a spreadsheet of what's shown.

import { api, download } from "./api.js";
import { h, replace, toast, saved } from "./dom.js";
import { state, subscribe, isOver, panelAnnouncements } from "./store.js";
import { runCard } from "./runs.js";
import { field, select } from "./editor.js";

const PAGE = 50;
const OUTCOMES = [["", "Any"], ["done", "Played"], ["stopped", "Stopped"], ["failed", "Failed"], ["missed", "Missed"]];

let runs = [];
let more = false;
let next = null; // the server's mark for the next page
let error = null;
let loading = false;

const filters = saved.get("history.filters", { from: "", to: "", announcement: "", state: "", by: "" });

function query(extra = {}) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries({ ...filters, ...extra })) if (v !== "" && v != null) q.set(k, v);
  return q.toString();
}

async function load(page = false) {
  if (loading) return;
  loading = true;
  try {
    const r = await api("GET", `/history?${query({ limit: PAGE, before: page ? next : null })}`);
    runs = page ? [...runs, ...r.runs] : r.runs;
    more = r.more;
    next = r.next ?? null;
    error = null;
  } catch (e) {
    error = e.message;
  }
  loading = false;
  render();
}

let el = null;
let list = null;

function render() {
  if (!list) return;
  // Still going: from memory, above what's ended.
  const going = [...state.runs.values()].filter((r) => !isOver(r)).sort((a, b) => b.id - a.id);
  replace(list,
    error && h("p", { class: "error" }, `Couldn't read the history: ${error}`),
    going.length > 0 && [h("h3", {}, "Playing now"), h("ul", { class: "runs" }, going.map(runCard))],
    runs.length
      ? [going.length > 0 && h("h3", {}, "Ended"), h("ul", { class: "runs" }, runs.map(runCard))]
      : !error && h("p", { class: "muted" }, Object.values(filters).some(Boolean) ? "Nothing matches." : "Nothing played yet."),
    more && h("div", { class: "buttons" }, h("button", { onclick: () => load(true), disabled: loading }, "Show more")));
}

function filterBar() {
  const date = (key) => h("input", { type: "date", value: filters[key], onchange: (e) => set(key, e.target.value) });
  const anns = panelAnnouncements();
  const ann = select([["", "Any"], ...anns.map((a) => [a.id, a.name])], filters.announcement);
  ann.onchange = () => set("announcement", ann.value);
  const outcome = select(OUTCOMES, filters.state);
  outcome.onchange = () => set("state", outcome.value);
  const by = h("input", { value: filters.by, placeholder: "Token name", onchange: (e) => set("by", e.target.value.trim()) });
  return h("div", { class: "row history-filters" },
    field("From", date("from")), field("To", date("to")),
    field("Announcement", ann), field("Outcome", outcome), field("Started by", by),
    h("div", { class: "buttons" },
      h("button", { type: "button", onclick: () => { for (const k of Object.keys(filters)) filters[k] = ""; saved.set("history.filters", filters); draw(); load(); } }, "Clear"),
      h("button", {
        type: "button",
        onclick: () => download(`/history.csv?${query()}`, "intercom-history.csv").catch((e) => toast(`Couldn't download: ${e.message}`, "error")),
      }, "Download CSV")));
}

function set(key, value) {
  filters[key] = value;
  saved.set("history.filters", filters);
  load();
}

let bar = null;
function draw() {
  replace(bar, filterBar());
}

export function mount(target) {
  el = target;
  bar = h("div");
  list = h("div");
  el.replaceChildren(bar, list);
  draw();
  load();
  // A run that ends is in the log now: read the first page again.
  let ended = new Set([...state.runs.values()].filter(isOver).map((r) => r.id));
  subscribe(["runs"], () => {
    const now = new Set([...state.runs.values()].filter(isOver).map((r) => r.id));
    const fresh = [...now].some((id) => !ended.has(id));
    ended = now;
    if (fresh) load();
    else render();
  });
  subscribe(["config"], draw);
  // "5 min ago" moves on by itself.
  setInterval(render, 30000);
}
