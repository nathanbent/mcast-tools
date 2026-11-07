// The Probes view: every probe's state, its streams, the multicast vs.
// unicast verdicts, a loss timeline, and its configuration.

import { api } from "./api.js";
import { h, replace, ago, toast } from "./dom.js";
import { state, subscribe, refresh } from "./store.js";

// verdict -> [class, label, explanation]
const VERDICTS = {
  ok: ["ok", "OK", "Multicast and its unicast copy are both clean."],
  multicast_loss: ["bad", "Multicast loss",
    "Loss on multicast only: check the multicast path (IGMP snooping and querier, PIM, a Wi-Fi hop's multicast rate)."],
  both_loss: ["warn", "Loss on both", "Loss on both paths: the network as a whole, or the sender, not multicast as such."],
  unicast_loss: ["warn", "Unicast loss", "Loss on the unicast copy only."],
  no_multicast: ["bad", "No multicast",
    "The unicast copy arrives but the group doesn't: not joined? IGMP snooping without a querier, routing, TTL."],
  idle: ["idle", "Idle", "No packets on either path this interval."],
};

const HISTORY = 360;

// UI state that survives re-renders, by probe ID.
const forms = new Map();      // id -> <form> being edited
const timelines = new Map();  // id -> { el, reports, loading }
const totalsOpen = new Set();
let addForm = null;
let defaultsForm = null;

const multicastRows = (report) => (report?.streams ?? []).filter((r) => !isUnicast(r.group));

function isUnicast(group) {
  const ip = group.startsWith("[") ? group.slice(1, group.indexOf("]")) : group.slice(0, group.lastIndexOf(":"));
  if (ip.includes(":")) return !ip.toLowerCase().startsWith("ff");
  const first = Number(ip.split(".")[0]);
  return !(first >= 224 && first <= 239);
}

// ok | warn | bad | offline | pending
export function health(p) {
  if (p.last_seen_ms == null) return "pending";
  if (!p.online) return "offline";
  if (p.comparisons.some((c) => c.verdict === "multicast_loss" || c.verdict === "no_multicast")) return "bad";
  if (p.comparisons.some((c) => c.verdict === "both_loss" || c.verdict === "unicast_loss")) return "warn";
  if (multicastRows(p.latest).some((r) => r.lost > 0)) return "warn";
  if ((p.latest?.groups ?? []).some((g) => g.error)) return "warn";
  return "ok";
}

const HEALTH_LABEL = { ok: "Online", warn: "Online · loss", bad: "Online · problem", offline: "Offline", pending: "Never seen" };

function duration(s) {
  if (s < 90) return `${Math.round(s)} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  if (s < 172800) return `${(s / 3600).toFixed(1)} h`;
  return `${Math.round(s / 86400)} d`;
}

const ms = (v) => (v == null ? "–" : `${v.toFixed(v < 10 ? 2 : 0)} ms`);
const pct = (v) => `${v.toFixed(v > 0 && v < 0.1 ? 3 : 2)} %`;

// ---- configuration form ----

function configForm({ config, id, onSave, onReset, onCancel, askId = false }) {
  const input = (props) => h("input", props);
  const els = {
    id: askId && input({ type: "text", required: true, pattern: "[A-Za-z0-9_.\\-]{1,64}",
      placeholder: "idf-3", title: "1-64 letters, digits, '.', '_' or '-'" }),
    label: input({ type: "text", value: config.label ?? "", maxLength: 100, placeholder: "3rd floor IDF" }),
    sap: input({ type: "checkbox", checked: config.sap }),
    groups: input({ type: "text", value: config.groups.join(", "), placeholder: "239.192.2.1:10000, …" }),
    interval: input({ type: "number", min: 1, max: 3600, step: "any", value: config.report_interval_s }),
    unicast: input({ type: "checkbox", checked: config.unicast }),
  };
  let busy = false;
  const form = h("form", { class: "probe-form", onsubmit: async (e) => {
      e.preventDefault();
      if (busy) return;
      const next = {
        label: els.label.value.trim() || null,
        sap: els.sap.checked,
        groups: els.groups.value.split(/[\s,]+/).filter(Boolean),
        report_interval_s: Number(els.interval.value),
        unicast: els.unicast.checked,
      };
      busy = true;
      try { await onSave(next, els.id ? els.id.value.trim() : id); } catch (err) { toast(err.message, "error"); }
      busy = false;
    } },
    h("div", { class: "row" },
      els.id && h("label", { class: "field" }, h("span", {}, "Probe ID"), els.id),
      h("label", { class: "field" }, h("span", {}, "Label"), els.label),
      h("label", { class: "field" }, h("span", {}, "Report every (s)"), els.interval)),
    h("label", { class: "field" }, h("span", {}, "Also watch groups"), els.groups),
    h("label", { class: "check" }, els.sap, "Watch every announced (SAP) stream"),
    h("label", { class: "check" }, els.unicast, "Send it unicast copies of every page, to compare with multicast"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      onReset && h("button", { type: "button", onclick: onReset }, "Use the defaults"),
      onCancel && h("button", { type: "button", class: "subtle", onclick: onCancel }, "Cancel")));
  return form;
}

function edit(p) {
  forms.set(p.id, configForm({
    config: p.config,
    id: p.id,
    onSave: async (config) => {
      putProbe(await api("PUT", `/probes/${encodeURIComponent(p.id)}/config`, config));
      forms.delete(p.id);
      rerender();
      toast(`Saved. ${p.online ? `${p.id} picks it up at its next check-in.` : `${p.id} gets it when it next checks in.`}`, "ok");
    },
    onReset: p.custom_config && (async () => {
      try {
        putProbe(await api("DELETE", `/probes/${encodeURIComponent(p.id)}/config`));
        forms.delete(p.id);
        rerender();
      } catch (e) { toast(e.message, "error"); }
    }),
    onCancel: () => { forms.delete(p.id); rerender(); },
  }));
  rerender();
}

async function forget(p) {
  if (!confirm(`Forget ${p.id}? It comes back with the defaults if it checks in again.`)) return;
  try {
    await api("DELETE", `/probes/${encodeURIComponent(p.id)}`);
    state.probes.delete(p.id);
    forms.delete(p.id);
    timelines.delete(p.id);
    rerender();
  } catch (e) { toast(e.message, "error"); }
}

// ---- loss timeline ----

async function loadTimeline(id) {
  const t = timelines.get(id);
  if (!t || t.loading) return;
  t.loading = true;
  try {
    const last = t.reports.at(-1)?.end_ms;
    const q = last == null ? "" : `?after_ms=${last}`;
    const more = await api("GET", `/probes/${encodeURIComponent(id)}/reports${q}`);
    t.reports = t.reports.concat(more).slice(-HISTORY);
    drawTimeline(t);
  } catch (e) {
    replace(t.el, h("p", { class: "error small" }, e.message));
  } finally {
    t.loading = false;
  }
}

function toggleTimeline(id) {
  if (timelines.has(id)) {
    timelines.delete(id);
  } else {
    timelines.set(id, { el: h("div", { class: "timeline" }, h("p", { class: "muted small" }, "Loading…")), reports: [], loading: false });
    loadTimeline(id);
  }
  rerender();
}

function drawTimeline(t) {
  if (t.reports.length === 0) return replace(t.el, h("p", { class: "muted small" }, "No reports yet."));
  const bars = t.reports.map((r) => {
    const rows = multicastRows(r);
    const packets = rows.reduce((n, s) => n + s.packets, 0);
    const lost = rows.reduce((n, s) => n + s.lost, 0);
    const loss = packets + lost === 0 ? 0 : (lost * 100) / (packets + lost);
    const kind = packets === 0 && lost === 0 ? "none" : lost === 0 ? "ok" : loss >= 1 ? "bad" : "warn";
    const when = new Date(r.end_ms).toLocaleTimeString();
    const tip = kind === "none" ? `${when}: no multicast packets` : `${when}: ${packets} packets, ${lost} lost (${pct(loss)})`;
    // Height shows how bad: any loss is at least a third, 5 % or more is full.
    const height = kind === "ok" ? 100 : kind === "none" ? 100 : Math.min(100, 33 + loss * 13.4);
    return h("div", { class: `bar ${kind}`, title: tip },
      h("div", { style: { height: `${height}%` } }));
  });
  const first = new Date(t.reports[0].start_ms).toLocaleTimeString();
  const lossy = t.reports.filter((r) => multicastRows(r).some((s) => s.lost > 0)).length;
  replace(t.el,
    h("div", { class: "bars", role: "img", "aria-label": `${lossy} of ${t.reports.length} intervals had multicast loss` }, bars),
    h("div", { class: "axis muted small" },
      h("span", {}, first),
      h("span", {}, `${lossy} of ${t.reports.length} intervals with loss`),
      h("span", {}, "now")));
}

// ---- cards ----

function streamsTable(p) {
  const rows = multicastRows(p.latest);
  if (!p.latest) return h("p", { class: "muted small" }, p.last_seen_ms == null ? "Waiting for it to check in." : "No report yet.");
  const errors = (p.latest.groups ?? []).filter((g) => g.error);
  return [
    errors.map((g) => h("p", { class: "error small" }, `${g.group}: ${g.error}`)),
    rows.length === 0
      ? h("p", { class: "muted small" }, "No multicast traffic on the groups it watches.")
      : h("div", { class: "scroll" }, h("table", {},
          h("thead", {}, h("tr", {}, ["Stream", "Group", "Packets/s", "Loss", "Jitter", "Max gap"].map((c) => h("th", {}, c)))),
          h("tbody", {}, rows.map((r) => h("tr", { class: r.packets === 0 ? "idle" : "" },
            h("td", {}, h("strong", {}, r.name ?? "–"), h("div", { class: "muted small mono" }, r.source)),
            h("td", { class: "mono" }, r.group),
            h("td", { class: "num" }, r.packets_per_sec.toFixed(1)),
            h("td", { class: `num${r.lost > 0 ? " bad" : ""}` }, r.lost > 0 ? `${r.lost} (${pct(r.loss_percent)})` : "0"),
            h("td", { class: "num" }, ms(r.jitter_ms)),
            h("td", { class: `num${r.max_gap_ms > 100 ? " bad" : ""}` }, ms(r.max_gap_ms))))))),
  ];
}

function comparisons(p) {
  if (!p.config.unicast) return null;
  if (p.comparisons.length === 0) {
    return h("p", { class: "muted small" },
      p.unicast_addr ? `Unicast copies go to ${p.unicast_addr}; none arrived in the last report (is the server paging?).`
        : "Waiting for the probe to start listening for unicast copies.");
  }
  return h("ul", { class: "verdicts" }, p.comparisons.map((c) => {
    const [cls, label, why] = VERDICTS[c.verdict] ?? ["idle", c.verdict, ""];
    const path = (s) => (s ? `${s.packets} pkts, ${s.lost} lost` : "nothing");
    return h("li", { class: `verdict ${cls}` },
      h("span", { class: `badge ${cls}` }, label),
      h("span", {}, h("strong", {}, c.name ?? c.group ?? `SSRC ${c.ssrc.toString(16)}`),
        h("span", { class: "muted small" }, ` multicast ${path(c.multicast)} · unicast copy ${path(c.unicast)}`),
        c.verdict !== "ok" && c.verdict !== "idle" && h("div", { class: "small" }, why)));
  }));
}

function totals(p) {
  if (p.totals.length === 0) return null;
  const d = h("details", { class: "totals", open: totalsOpen.has(p.id),
      ontoggle: (e) => (e.target.open ? totalsOpen.add(p.id) : totalsOpen.delete(p.id)) },
    h("summary", {}, "Since first seen"),
    h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, ["Path", "Stream", "Packets", "Lost", "Lossy intervals", "Max gap", "Worst jitter"].map((c) => h("th", {}, c)))),
      h("tbody", {}, p.totals.map((t) => h("tr", {},
        h("td", {}, t.unicast ? "unicast" : "multicast"),
        h("td", {}, h("strong", {}, t.name ?? "–"), h("div", { class: "muted small mono" }, t.group)),
        h("td", { class: "num" }, t.packets.toLocaleString()),
        h("td", { class: `num${t.lost > 0 ? " bad" : ""}` }, `${t.lost} (${pct(t.loss_percent)})`),
        h("td", { class: "num" }, `${t.lossy_intervals} / ${t.intervals}`),
        h("td", { class: "num" }, ms(t.max_gap_ms)),
        h("td", { class: "num" }, ms(t.worst_jitter_ms))))))));
  return d;
}

function facts(p) {
  const parts = [];
  if (p.hostname) parts.push(`host ${p.hostname}`);
  if (p.version) parts.push(`v${p.version}`);
  if (p.online && p.uptime_s != null) parts.push(`up ${duration(p.uptime_s)}`);
  if (p.last_seen_ms != null) {
    const s = (Date.now() - p.last_seen_ms) / 1000;
    parts.push(`last check-in ${s < 60 ? `${Math.max(0, Math.round(s))} s ago` : ago(p.last_seen_ms)}`);
  }
  parts.push(`every ${p.config.report_interval_s} s`);
  parts.push(p.custom_config ? "own config" : "default config");
  return parts.join(" · ");
}

function warnings(p) {
  const w = [];
  if (p.last_seen_ms != null && !p.config_applied) w.push("It hasn't applied its configuration yet.");
  if (p.queued > 0) w.push(`Catching up: ${p.queued} reports still queued on the probe.`);
  if (p.dropped > 0) w.push(`${p.dropped} reports were lost while it couldn't reach the server.`);
  return w.map((m) => h("p", { class: "warn small" }, m));
}

function card(p) {
  const hl = health(p);
  const t = timelines.get(p.id);
  return h("section", { class: `probe ${hl}`, "aria-label": p.id },
    h("div", { class: "probe-head" },
      h("div", {},
        h("h3", {}, p.id, p.config.label && h("span", { class: "muted" }, ` · ${p.config.label}`)),
        h("div", { class: "muted small" }, facts(p))),
      h("span", { class: `badge ${hl}` }, HEALTH_LABEL[hl]),
      h("div", { class: "probe-actions" },
        p.last_seen_ms != null && h("button", { class: "subtle", "aria-pressed": t ? "true" : "false",
          onclick: () => toggleTimeline(p.id) }, "Timeline"),
        h("button", { class: "subtle", disabled: forms.has(p.id), onclick: () => edit(p) }, "Configure"),
        h("button", { class: "subtle danger", onclick: () => forget(p) }, "Forget"))),
    warnings(p),
    forms.get(p.id),
    t?.el,
    streamsTable(p),
    comparisons(p),
    totals(p));
}

// ---- the view ----

function header() {
  const probes = [...state.probes.values()];
  const counts = { ok: 0, warn: 0, bad: 0, offline: 0, pending: 0 };
  for (const p of probes) counts[health(p)]++;
  const online = counts.ok + counts.warn + counts.bad;
  return h("div", { class: "probes-summary" },
    h("span", {}, h("strong", {}, `${online}`), ` of ${probes.length} online`),
    counts.bad > 0 && h("span", { class: "badge bad" }, `${counts.bad} with a problem`),
    counts.warn > 0 && h("span", { class: "badge warn" }, `${counts.warn} with loss`),
    counts.offline > 0 && h("span", { class: "badge offline" }, `${counts.offline} offline`),
    h("span", { class: "spacer" }),
    h("button", { disabled: !!addForm, onclick: openAdd }, "Add probe"),
    h("button", { disabled: !!defaultsForm, onclick: openDefaults }, "Defaults"));
}

function openAdd() {
  addForm = configForm({
    config: state.probeDefaults ?? { label: null, sap: true, groups: [], report_interval_s: 10, unicast: false },
    askId: true,
    onSave: async (config, id) => {
      putProbe(await api("PUT", `/probes/${encodeURIComponent(id)}/config`, config));
      addForm = null;
      rerender();
      toast(`${id} is set up. Start it with: mcast probe run --server ${location.origin} --id ${id}`, "ok");
    },
    onCancel: () => { addForm = null; rerender(); },
  });
  rerender();
  addForm.querySelector("input")?.focus();
}

function openDefaults() {
  defaultsForm = configForm({
    config: state.probeDefaults ?? { label: null, sap: true, groups: [], report_interval_s: 10, unicast: false },
    onSave: async (config) => {
      state.probeDefaults = await api("PUT", "/probe-defaults", config);
      defaultsForm = null;
      rerender();
      toast("Saved the defaults: probes without their own configuration pick them up at their next check-in.", "ok");
    },
    onCancel: () => { defaultsForm = null; rerender(); },
  });
  rerender();
}

function empty() {
  return h("section", { class: "empty" },
    h("h3", {}, "No probes yet"),
    h("p", {}, "Probes are small receivers (a Raspberry Pi, say) that watch the multicast streams where they sit and report here. On each probe, run:"),
    h("pre", { class: "mono" }, `mcast probe run --server ${location.origin} --token TOKEN --id NAME`),
    h("p", { class: "muted small" },
      "Start the server with --probe-token to give probes a token that can only check in. ",
      "With “Send it unicast copies” on, each page also goes to the probe by unicast, so it can tell multicast trouble from network trouble."));
}

let root = null;

function putProbe(p) {
  state.probes.set(p.id, p);
}

function rerender() {
  if (!root) return;
  const probes = [...state.probes.values()].sort((a, b) => a.id.localeCompare(b.id));
  replace(root,
    header(),
    addForm && h("section", { class: "probe-edit" }, h("h3", {}, "Add a probe"),
      h("p", { class: "muted small" }, "Set it up before it's deployed; it gets this configuration when it first checks in."), addForm),
    defaultsForm && h("section", { class: "probe-edit" }, h("h3", {}, "Defaults"),
      h("p", { class: "muted small" }, "For every probe without a configuration of its own."), defaultsForm),
    probes.length === 0 ? empty() : h("div", { class: "probes" }, probes.map(card)));
}

export function mount(el) {
  root = el;
  rerender();
  subscribe((what, id) => {
    if (what !== "probes") return;
    // Keep open timelines up to date.
    if (id != null && timelines.has(id)) loadTimeline(id);
    if (id != null && !state.probes.has(id)) { forms.delete(id); timelines.delete(id); }
    rerender();
  });
  // "Last check-in 3 min ago" goes stale on its own.
  setInterval(rerender, 15000);
  refresh("probe-defaults").catch(() => {});
}
