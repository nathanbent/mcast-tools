// Zones: sets of multicast groups (phonebook names or addresses), each with
// the codec and volume its speakers need.

import { h, replace, ago, toast } from "./dom.js";
import { state, siteName, onRunSettled, subscribe } from "./store.js";
import { api } from "./api.js";
import { mountEditor, field, select, numberInput, num } from "./editor.js";

export const ENCODINGS = [
  ["pcmu", "G.711 µ-law"], ["pcma", "G.711 A-law"],
  ["g722", "G.722"], ["l16", "L16"], ["l24", "L24"], ["opus", "Opus"],
];

// The CLI and API also take g711u / g711a (and ulaw / alaw); show those as their pcm names.
export const encodingValue = (e) =>
  ({ g711u: "pcmu", ulaw: "pcmu", g711a: "pcma", alaw: "pcma" }[e?.toLowerCase()] ?? e?.toLowerCase() ?? "");
export const encodingLabel = (e) => ENCODINGS.find(([v]) => v === encodingValue(e))?.[1] ?? e;

// What a group with no codec of its own uses: the panel's default, else the server's.
export const defaultEncoding = () => {
  const d = state.config.defaults?.encoding;
  return d ? encodingLabel(d) : "L16, the server's";
};

function describe(t) {
  const codec = t.encoding ? encodingLabel(t.encoding) : `default ${defaultEncoding().split(",")[0]}`;
  const opts = [codec, t.volume_db != null && `${t.volume_db > 0 ? "+" : ""}${t.volume_db} dB`].filter(Boolean);
  return opts.length ? `${t.group} (${opts.join(", ")})` : t.group;
}

// Play the chime to one zone, to check its speakers. It's an ordinary
// play ("Test: Hall"), so it shows on the Panel and in the history.
async function testZone(z, button) {
  button.disabled = true;
  try {
    const run = await api("POST", `/zones/${encodeURIComponent(z.id)}/test`);
    const t = run.targets.find((x) => x.error);
    // One that didn't play at all gets the Panel's toast as it ends.
    if (run.state === "failed") return;
    if (t) toast(`Testing ${z.name}, but not everywhere: ${t.error}`, "error");
    else toast(`Playing the chime to ${z.name}`, "ok");
  } catch (e) {
    toast(`Couldn't test ${z.name}: ${e.message}`, "error");
  } finally {
    // Long enough for the chime, so a second press doesn't find it busy.
    setTimeout(() => { button.disabled = false; }, 3000);
  }
}

// The speakers expected to hear a zone's pages: names as receivers give
// them (`mcast receive --name Lobby`). Offers the ones the mcast server
// hears now.
function listenersPart(initial) {
  const names = [...initial];
  const has = (n) => names.some((x) => x.toLowerCase() === n.toLowerCase());
  const chips = h("div", { class: "tag-bar" });
  const heardBox = h("div", { class: "small muted" }, "Looking for receivers…");
  let heard = [];
  const input = h("input", { maxLength: 64, placeholder: "e.g. Lobby", list: "heard-listeners",
    "aria-label": "Listener name",
    onkeydown: (e) => { if (e.key === "Enter") { e.preventDefault(); add(input.value); } } });
  const datalist = h("datalist", { id: "heard-listeners" });

  const add = (n) => {
    n = n.trim();
    if (n && !has(n) && names.length < 100) names.push(n);
    input.value = "";
    render();
  };
  const render = () => {
    replace(chips, names.length
      ? names.map((n) => h("button", { type: "button", class: "tag", title: `Remove ${n}`,
          onclick: () => { names.splice(names.indexOf(n), 1); render(); } }, n, " ×"))
      : [h("span", { class: "muted small" }, "None: plays to this zone aren't checked.")]);
    const fresh = heard.filter((l) => !has(l.name));
    replace(datalist, fresh.map((l) => h("option", { value: l.name })));
    if (heard.length) {
      replace(heardBox, "Heard now: ", ...(fresh.length
        ? fresh.map((l) => h("button", { type: "button", class: "tag", title: `Add ${l.name} (on ${l.group})`,
            onclick: () => add(l.name) }, "+ ", l.name))
        : ["all of them are listed."]));
    }
  };
  api("GET", "/listeners").then((list) => {
    const seen = new Map();
    for (const l of list ?? []) {
      const name = l.name || l.cname;
      if (name && !l.left && !seen.has(name.toLowerCase())) seen.set(name.toLowerCase(), { name, group: l.group });
    }
    heard = [...seen.values()].sort((a, b) => a.name.localeCompare(b.name));
    if (!heard.length) replace(heardBox, "No receivers are reporting to the mcast server now.");
    render();
  }, (e) => {
    replace(heardBox, e.status === 404
      ? "The mcast server's monitor is off (--no-monitor), so it can't hear receivers."
      : `Couldn't ask the mcast server who is listening: ${e.message}`);
  });
  render();

  const el = h("fieldset", { class: "part" }, h("legend", {}, "Listeners"),
    h("p", { class: "small muted" },
      "Speakers that should hear this zone's pages, by the name they give (mcast receive --name). After each play the panel says which of them heard it."),
    chips,
    h("div", { class: "listener-add" }, input, datalist,
      h("button", { type: "button", onclick: () => add(input.value) }, "Add")),
    heardBox);
  return { el, value: () => names };
}

// Text displays: groups that get the text pages of what plays here, as
// real-time text (`mcast text recv -g GROUP` shows them).
function displaysPart(initial) {
  const groups = [...initial];
  const has = (g) => groups.some((x) => x.toLowerCase() === g.toLowerCase());
  const chips = h("div", { class: "tag-bar" });
  const input = h("input", { maxLength: 64, placeholder: "Phonebook name or 239.x.x.x:port", list: "phonebook-groups",
    "aria-label": "Display group",
    onkeydown: (e) => { if (e.key === "Enter") { e.preventDefault(); add(input.value); } } });
  const add = (g) => {
    g = g.trim();
    if (g && !has(g) && groups.length < 20) groups.push(g);
    input.value = "";
    render();
  };
  const render = () => replace(chips, groups.length
    ? groups.map((g) => h("button", { type: "button", class: "tag", title: `Remove ${g}`,
        onclick: () => { groups.splice(groups.indexOf(g), 1); render(); } }, g, " ×"))
    : [h("span", { class: "muted small" }, "None. The panel's display page can still show this zone's text pages.")]);
  render();
  const el = h("fieldset", { class: "part" }, h("legend", {}, "Text displays"),
    h("p", { class: "small muted" },
      "Groups that get the text pages of playlists played here, as real-time text: run mcast text recv -g GROUP on a screen. A browser can show them too: Settings → Panel → Display page."),
    chips,
    h("div", { class: "listener-add" }, input,
      h("button", { type: "button", onclick: () => add(input.value) }, "Add")));
  return { el, value: () => groups };
}

function form(z, save, cancel) {
  const name = h("input", { value: z?.name ?? "", required: true, maxLength: 64 });
  const sites = state.config.sites ?? [];
  const site = select([["", "No site"], ...sites.map((s) => [s.id, s.name])], z?.site ?? "");
  const rows = [];
  const rowsEl = h("div", { class: "runs" });

  const addRow = (t = {}) => {
    const row = {
      original: t,
      group: h("input", { value: t.group ?? "", required: true, list: "phonebook-groups", placeholder: "Phonebook name or 239.x.x.x:port" }),
      encoding: select([["", `Default (${defaultEncoding()})`], ...ENCODINGS], encodingValue(t.encoding)),
      volume: numberInput(t.volume_db, { placeholder: "0" }),
      tos: numberInput(t.tos, { min: 0, max: 255, step: 1, placeholder: "184", title: "IP TOS byte: 184 is DSCP EF" }),
      ttl: numberInput(t.ttl, { min: 1, max: 255, step: 1, placeholder: "Default" }),
      iface: h("input", { value: t.interface ?? "", placeholder: "Default" }),
    };
    row.el = h("div", { class: "target-row" },
      field("Group", row.group), field("Codec", row.encoding),
      field("Volume (dB)", row.volume), field("TOS", row.tos),
      field("TTL", row.ttl), field("Interface", row.iface),
      h("button", { type: "button", class: "danger", title: "Remove this group", onclick: () => {
        rows.splice(rows.indexOf(row), 1);
        render();
      } }, "Remove"));
    rows.push(row);
    render();
  };

  const render = () => replace(rowsEl, rows.map((r) => r.el));

  const listeners = listenersPart(z?.listeners ?? []);
  const displays = displaysPart(z?.displays ?? []);
  for (const t of z?.targets ?? []) addRow(t);
  if (!rows.length) addRow();

  const submit = (e) => {
    e.preventDefault();
    save({
      name: name.value.trim(),
      site: site.value || null,
      targets: rows.map((r) => ({
        group: r.group.value.trim(),
        encoding: r.encoding.value || null,
        volume_db: num(r.volume),
        tos: num(r.tos),
        ttl: num(r.ttl),
        interface: r.iface.value.trim() || null,
      })),
      listeners: listeners.value(),
      displays: displays.value(),
    });
  };

  return h("form", { onsubmit: submit },
    h("fieldset", { class: "part" }, h("legend", {}, "General"),
      h("div", { class: "row" }, field("Name", name),
        sites.length > 0 && field("Site", site, "A site's schedules play only to its zones"))),
    h("fieldset", { class: "part" }, h("legend", {}, "Groups"),
      h("p", { class: "small muted" },
        "Each group with the options its speakers need. Volume is added to the announcement's. A codec left on Default uses the one in Settings; other options left empty use the server's."),
      rowsEl,
      h("div", { class: "buttons", style: { marginTop: "10px" } },
        h("button", { type: "button", onclick: () => addRow() }, "Add group"))),
    listeners.el,
    displays.el,
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

// For the list: who is reporting to the mcast server's monitor now, and when
// each speaker last heard a page (from the history, over HEARD_DAYS days).
const HEARD_DAYS = 30;
const heard = { lastPage: null };

async function refreshHeard() {
  try {
    const m = await api("GET", `/monitor?days=${HEARD_DAYS}`);
    heard.lastPage = new Map((m.listeners?.list ?? [])
      .filter((e) => e.last_heard_ms).map((e) => [e.name.toLowerCase(), e.last_heard_ms]));
  } catch {}
}

// A zone's listeners, each with whether it's reporting now (the panel's
// watch on speakers, GET /speakers) and when it last heard a page.
function listenersCell(z) {
  if (!z.listeners?.length) return "—";
  const sp = state.speakers;
  return h("ul", { class: "ears" }, z.listeners.map((name) => {
    const key = name.toLowerCase();
    const s = sp.speakers.find((x) => x.name.toLowerCase() === key);
    const now = sp.known ? s?.online ?? null : null;
    const last = heard.lastPage?.get(key);
    const page = heard.lastPage
      ? last ? `heard a page ${ago(last)}` : `no page heard in ${HEARD_DAYS} days`
      : null;
    const state_ = now == null
      ? (sp.known ? "checking" : "?")
      : now ? "reporting" : `offline${s.since_ms ? ` since ${ago(s.since_ms)}` : ""}`;
    const why = now == null
      ? sp.error ?? "not seen yet: it's offline if it doesn't report within a minute"
      : now ? "reporting to the mcast server now"
        : "hasn't reported for a minute: off, unplugged, or not running mcast receive";
    return h("li", { title: [why, page].filter(Boolean).join("; ") },
      h("span", { class: `dot ${now == null ? "" : now ? "on" : "off"}`, "aria-hidden": "true" }),
      name,
      h("span", { class: "muted small" }, ` · ${state_}${page ? ` · ${page}` : ""}`));
  }));
}

export function mount(el) {
  const editor = mountEditor(el, {
    kind: "zone",
    path: "/zones",
    items: () => state.config.zones,
    columns: () => [
      ["Name", (z) => z.name],
      ...((state.config.sites ?? []).length ? [["Site", (z) => (z.site ? siteName(z.site) : "—"), true]] : []),
      ["Groups", (z) => z.targets.map(describe).join(", "), true],
      ["Listeners", listenersCell, true],
      ...(state.config.zones.some((z) => z.displays?.length)
        ? [["Displays", (z) => (z.displays?.length ? z.displays.join(", ") : "—"), true]] : []),
    ],
    actions: (z) => h("button", {
      title: `Play the chime to ${z.name} alone, to check its speakers`,
      onclick: (e) => testZone(z, e.currentTarget),
    }, "Test"),
    form,
  });
  // Last pages come from the history: worth asking while some zone lists
  // listeners and the tab shows. Who's reporting comes with the events.
  const wanted = () => state.config.zones.some((z) => z.listeners?.length) && !el.closest("[hidden]");
  const update = () => { if (wanted()) refreshHeard().then(editor.render); };
  update();
  setInterval(update, 60000);
  subscribe(["speakers"], editor.render);
  window.addEventListener("hashchange", () => { if (location.hash === "#zones") update(); });
  onRunSettled(update);
  subscribe(["config"], update);
}
