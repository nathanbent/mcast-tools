// The Monitor tab: what the play history says over the last 7, 30 or 90
// days: plays a day (by outcome), per announcement and per zone, how
// schedules kept time, what started plays, and how long the mcast server was
// reachable. From GET /monitor.

import { api } from "./api.js";
import { h, replace, saved } from "./dom.js";
import { onRunSettled } from "./store.js";

// Outcomes, in stacking order, with their status colors (see app.css).
const OUTCOMES = [
  ["played", "Played", "s-good"],
  ["stopped", "Stopped", "s-neutral"],
  ["missed", "Missed", "s-warn"],
  ["failed", "Failed", "s-critical"],
];

let days = saved.get("monitor.days", 7);
let data = null;
let error = null;
let asTable = saved.get("monitor.table", false);
let el = null;

async function load() {
  try {
    data = await api("GET", `/monitor?days=${days}`);
    error = null;
  } catch (e) {
    error = e.message;
  }
  render();
}

const pct = (n, of) => (of ? `${Math.round((n / of) * 100)}%` : "—");
const plural = (n, one, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;
const dateLabel = (d) => new Date(`${d}T12:00:00`).toLocaleDateString(undefined, { day: "numeric", month: "short" });
const when = (ms) => new Date(ms).toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
function duration(ms) {
  const m = Math.round(ms / 60000);
  if (m < 1) return "under a minute";
  if (m < 120) return `${m} min`;
  const hrs = Math.round(m / 60);
  return hrs < 48 ? `${hrs} h` : `${Math.round(hrs / 24)} days`;
}

function tile(label, value, note, cls) {
  return h("div", { class: "stat-tile" },
    h("span", { class: "label" }, label),
    h("span", { class: `value ${cls ?? ""}` }, value),
    note && h("span", { class: "note muted small" }, note));
}

function legend() {
  return h("div", { class: "legend" }, OUTCOMES.map(([, label, cls]) =>
    h("span", { class: "legend-item" }, h("span", { class: `swatch ${cls}`, "aria-hidden": "true" }), label)));
}

// A floating note for the column under the pointer.
const tip = h("div", { class: "chart-tip", role: "tooltip", hidden: true });

function dayChart() {
  const max = Math.max(1, ...data.days.map((d) => d.plays));
  // Label every few days so they don't crowd.
  const every = data.days.length <= 7 ? 1 : data.days.length <= 31 ? 5 : 14;
  const cols = data.days.map((d, i) => {
    const col = h("div", {
      class: "col", tabindex: "0", "aria-label": `${dateLabel(d.date)}: ${plural(d.plays, "play")}`,
      onmouseenter: (e) => showTip(e.currentTarget, d), onfocus: (e) => showTip(e.currentTarget, d),
      onmouseleave: hideTip, onblur: hideTip,
    },
      h("div", { class: "stack", style: { height: `${(d.plays / max) * 100}%` } },
        OUTCOMES.filter(([k]) => d[k] > 0).map(([k, , cls]) =>
          h("div", { class: `seg ${cls}`, style: { flexGrow: String(d[k]) } }))),
      h("span", { class: "x" }, i % every === 0 || i === data.days.length - 1 ? dateLabel(d.date) : ""));
    return col;
  });
  return h("div", { class: "chart" },
    h("div", { class: "y" }, h("span", {}, String(max)), h("span", {}, "0")),
    h("div", { class: "cols" }, cols));
}

function showTip(target, d) {
  replace(tip,
    h("strong", {}, dateLabel(d.date)), h("div", {}, plural(d.plays, "play")),
    OUTCOMES.filter(([k]) => d[k] > 0).map(([k, label, cls]) =>
      h("div", { class: "tip-row" }, h("span", { class: `swatch ${cls}` }), `${label}: ${d[k]}`)));
  tip.hidden = false;
  const r = target.getBoundingClientRect();
  const box = el.getBoundingClientRect();
  tip.style.left = `${Math.min(r.left - box.left + r.width / 2, box.width - 160)}px`;
  tip.style.top = `${r.top - box.top - 8}px`;
}
function hideTip() { tip.hidden = true; }

function dayTable() {
  return h("div", { class: "scroll" }, h("table", { class: "items" },
    h("thead", {}, h("tr", {}, h("th", {}, "Day"), h("th", {}, "Plays"), OUTCOMES.map(([, label]) => h("th", {}, label)))),
    h("tbody", {}, [...data.days].reverse().map((d) => h("tr", {},
      h("td", {}, dateLabel(d.date)), h("td", {}, String(d.plays)), OUTCOMES.map(([k]) => h("td", {}, String(d[k]))))))));
}

// One row's outcome as a thin 100% bar, labeled in the cells beside it.
function outcomeBar(parts) {
  const total = parts.reduce((n, [v]) => n + v, 0);
  return h("div", { class: "bar100", "aria-hidden": "true" },
    total ? parts.filter(([v]) => v > 0).map(([v, cls]) => h("div", { class: `seg ${cls}`, style: { flexGrow: String(v) } })) : null);
}

function render() {
  if (!el) return;
  const periods = [7, 30, 90].map((n) => h("button", {
    class: `tag${n === days ? " on" : ""}`, "aria-pressed": String(n === days),
    onclick: () => { days = n; saved.set("monitor.days", n); load(); },
  }, `Last ${n} days`));
  if (!data) {
    replace(el, h("div", { class: "tag-bar" }, periods), error ? h("p", { class: "error" }, error) : h("p", { class: "muted" }, "Loading…"));
    return;
  }
  const t = data.totals;
  const sch = data.schedules;
  const scheduled = sch.on_time + sch.late + sch.missed;
  const c = data.connection;
  const ears = data.listeners ?? { checked: 0, with_silent: 0, list: [] };
  replace(el,
    h("div", { class: "tag-bar" }, periods,
      h("span", { class: "spacer" }),
      h("button", { class: "link", onclick: load }, "Refresh")),
    error && h("p", { class: "error" }, error),
    h("div", { class: "tiles-row" },
      tile("Plays", t.plays.toLocaleString()),
      tile("Played", pct(t.played, t.plays), `${t.played.toLocaleString()} to every group reached`),
      tile("Failed", t.failed.toLocaleString(), t.failed ? "nothing played" : null, t.failed ? "bad" : ""),
      tile("Missed", t.missed.toLocaleString(), t.missed ? "schedules that couldn't play" : null, t.missed ? "warn" : ""),
      tile("Schedules on time", pct(sch.on_time, scheduled), scheduled ? `${sch.on_time} of ${scheduled}` : "none scheduled"),
      tile("mcast server up", c.uptime == null ? "—" : `${(c.uptime * 100).toFixed(c.uptime < 1 ? 1 : 0)}%`,
        c.uptime == null ? "not seen yet" : c.outages.length ? plural(c.outages.length, "outage") : "no outages",
        c.uptime != null && c.uptime < 0.99 ? "bad" : ""),
      ears.checked > 0 && tile("Heard by every speaker", pct(ears.checked - ears.with_silent, ears.checked),
        ears.with_silent ? `${ears.with_silent} of ${ears.checked} checked plays missed someone` : `all ${ears.checked} checked plays`,
        ears.with_silent ? "warn" : "")),
    h("section", { class: "panel-group" },
      h("div", { class: "chart-head" }, h("h3", { class: "group-head" }, "Plays a day"), legend(),
        h("span", { class: "spacer" }),
        h("button", { class: "link", onclick: () => { asTable = !asTable; saved.set("monitor.table", asTable); render(); } },
          asTable ? "Show the chart" : "Show as a table")),
      asTable ? dayTable() : dayChart()),
    h("div", { class: "two-col" },
      h("section", { class: "panel-group" },
        h("h3", { class: "group-head" }, "Announcements"),
        data.announcements.length
          ? h("div", { class: "scroll" }, h("table", { class: "items" },
              h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", {}, "Plays"), h("th", { class: "wide" }, "Outcome"), h("th", {}, "Failed"), h("th", {}, "Missed"))),
              h("tbody", {}, data.announcements.map((a) => h("tr", {},
                h("td", {}, a.name), h("td", {}, String(a.plays)),
                h("td", { class: "wide" }, outcomeBar([[a.played, "s-good"], [a.stopped, "s-neutral"], [a.missed, "s-warn"], [a.failed, "s-critical"]])),
                h("td", { class: a.failed ? "bad" : "" }, String(a.failed)), h("td", { class: a.missed ? "warn" : "" }, String(a.missed)))))))
          : h("p", { class: "muted" }, "Nothing played in this time.")),
      h("section", { class: "panel-group" },
        h("h3", { class: "group-head" }, "Zones"),
        h("p", { class: "muted small" }, "Each send to a zone's group: played, busy (something else was using it), failed, or taken over by a higher priority."),
        data.zones.length
          ? h("div", { class: "scroll" }, h("table", { class: "items" },
              h("thead", {}, h("tr", {}, h("th", {}, "Zone"), h("th", {}, "Sends"), h("th", {}, "Played"), h("th", { class: "wide" }, "Outcome"), h("th", {}, "Busy"), h("th", {}, "Failed"), h("th", {}, "Taken over"))),
              h("tbody", {}, data.zones.map((z) => h("tr", {},
                h("td", {}, z.name), h("td", {}, String(z.sends)), h("td", {}, pct(z.played, z.sends)),
                h("td", { class: "wide" }, outcomeBar([[z.played, "s-good"], [z.taken_over, "s-neutral"], [z.busy, "s-warn"], [z.failed, "s-critical"]])),
                h("td", { class: z.busy ? "warn" : "" }, String(z.busy)), h("td", { class: z.failed ? "bad" : "" }, String(z.failed)), h("td", {}, String(z.taken_over)))))))
          : h("p", { class: "muted" }, "Nothing sent in this time."))),
    ears.list.length > 0 && h("section", { class: "panel-group" },
      h("h3", { class: "group-head" }, "Speakers"),
      h("p", { class: "muted small" }, "Plays to zones that list their speakers: which ones reported hearing each play (mcast receive --name)."),
      h("div", { class: "scroll" }, h("table", { class: "items" },
        h("thead", {}, h("tr", {}, h("th", {}, "Speaker"), h("th", {}, "Expected"), h("th", {}, "Heard"), h("th", { class: "wide" }, "Outcome"),
          h("th", {}, "Missed"), h("th", {}, "Last heard one"), h("th", {}, "Worst loss"))),
        h("tbody", {}, ears.list.map((e) => h("tr", {},
          h("td", {}, e.name), h("td", {}, e.expected ? String(e.expected) : "not expected"),
          h("td", {}, String(e.heard)),
          h("td", { class: "wide" }, outcomeBar([[e.expected - e.missed, "s-good"], [e.missed, "s-critical"]])),
          h("td", { class: e.missed ? "bad" : "" }, String(e.missed)),
          h("td", {}, e.last_heard_ms ? when(e.last_heard_ms) : "—"),
          h("td", {}, e.worst_loss_percent != null ? `${e.worst_loss_percent.toFixed(1)}%` : "—"))))))),
    h("div", { class: "two-col" },
      h("section", { class: "panel-group" },
        h("h3", { class: "group-head" }, "Schedules"),
        scheduled
          ? h("dl", { class: "facts" },
              h("dt", {}, "On time"), h("dd", {}, `${sch.on_time} (${pct(sch.on_time, scheduled)})`),
              h("dt", {}, `Late (over ${sch.late_after_ms / 1000} s)`), h("dd", { class: sch.late ? "warn" : "" }, String(sch.late)),
              h("dt", {}, "Missed"), h("dd", { class: sch.missed ? "warn" : "" }, String(sch.missed)),
              h("dt", {}, "Latest start"), h("dd", {}, sch.most_late_ms < 1000 ? "under a second" : `${(sch.most_late_ms / 1000).toFixed(1)} s`))
          : h("p", { class: "muted" }, "No scheduled plays in this time."),
        h("h3", { class: "group-head", style: { marginTop: "16px" } }, "Started by"),
        h("dl", { class: "facts" },
          [["manual", "Buttons and push to talk"], ["schedule", "Schedules"], ["api", "Other systems (trigger API)"]].map(([k, label]) =>
            [h("dt", {}, label), h("dd", {}, String(data.triggers[k] ?? 0))]),
          data.by.length > 0 && [h("dt", {}, "Most by"), h("dd", {}, data.by.slice(0, 5).map((b) => `${b.name} (${b.plays})`).join(", "))])),
      h("section", { class: "panel-group" },
        h("h3", { class: "group-head" }, "Connection to the mcast server"),
        c.uptime == null
          ? h("p", { class: "muted" }, "The panel hasn't seen the connection in this time yet.")
          : [h("p", {}, `Reachable ${(c.uptime * 100).toFixed(2)}% of the time the panel was running (${duration(c.known_ms)} seen).`),
            c.outages.length
              ? h("table", { class: "items" },
                  h("thead", {}, h("tr", {}, h("th", {}, "Lost"), h("th", {}, "Back"), h("th", {}, "For"))),
                  h("tbody", {}, c.outages.map((o) => h("tr", {},
                    h("td", {}, when(o.from_ms)), h("td", {}, o.to_ms ? when(o.to_ms) : "not yet"),
                    h("td", {}, duration((o.to_ms ?? Date.now()) - o.from_ms))))))
              : h("p", { class: "muted" }, "No outages.")])),
    tip);
}

export function mount(target) {
  el = target;
  render();
  load();
  // New plays count once they end: read again then, if the tab is showing,
  // or when it's next shown.
  let stale = false;
  const showing = () => !el.closest("[hidden]");
  let soon = null;
  const refresh = () => {
    if (showing()) { stale = false; clearTimeout(soon); soon = setTimeout(load, 1000); } else stale = true;
  };
  onRunSettled(refresh);
  window.addEventListener("hashchange", () => { if (stale && location.hash === "#monitor") refresh(); });
  setInterval(refresh, 5 * 60 * 1000);
}
