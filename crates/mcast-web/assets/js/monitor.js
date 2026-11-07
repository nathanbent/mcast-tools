// Live rate, loss, jitter and level for every stream on the network.

import { h, replace } from "./dom.js";
import { state, subscribe } from "./store.js";
import { groupCell } from "./streams.js";

const FLOOR = -60;

function meter(peak, rms) {
  const pct = (db) => (db == null ? 0 : Math.max(0, Math.min(100, ((db - FLOOR) / -FLOOR) * 100)));
  const label = peak == null ? "silent" : `${peak.toFixed(1)} dBFS peak`;
  return h("div", { class: "meter", role: "meter", "aria-valuemin": FLOOR, "aria-valuemax": 0,
      "aria-valuenow": peak ?? FLOOR, "aria-label": label, title: label },
    h("div", { class: "rms", style: { width: `${pct(rms)}%` } }),
    h("div", { class: `peak${peak != null && peak > -1 ? " hot" : ""}`, style: { left: `${pct(peak)}%` } }));
}

function row(r) {
  const idle = r.idle_ms > 2000;
  return h("tr", { class: idle ? "idle" : "" },
    h("td", {}, h("strong", {}, r.name ?? "–"), h("div", { class: "muted small mono" }, `${r.source}`)),
    groupCell(r.group),
    h("td", {}, r.codec ?? "?"),
    h("td", { class: "num" }, idle ? `idle ${Math.round(r.idle_ms / 1000)} s` : `${Math.round(r.kbit_per_sec)} kbit/s`),
    h("td", { class: `num${r.loss_percent > 1 ? " bad" : ""}` }, `${r.loss_percent.toFixed(1)} %`),
    h("td", { class: "num" }, r.jitter_ms == null ? "–" : `${r.jitter_ms.toFixed(1)} ms`),
    h("td", { class: "level" }, meter(r.peak_dbfs, r.rms_dbfs)));
}

function render(el) {
  const m = state.monitor;
  if (m === false) return replace(el, h("p", { class: "muted" }, "The monitor is off (server started with --no-monitor)."));
  if (!m || m.streams.length === 0) return replace(el, h("p", { class: "muted" }, "No traffic on watched groups."));
  replace(el, h("table", {},
    h("thead", {}, h("tr", {}, ["Stream", "Group", "Codec", "Rate", "Loss", "Jitter", "Level"].map((c) => h("th", {}, c)))),
    h("tbody", {}, m.streams.map(row))));
}

export function mount(el) {
  const body = h("div", { class: "scroll" });
  el.append(body);
  render(body);
  subscribe((what) => what === "monitor" && render(body));
}
