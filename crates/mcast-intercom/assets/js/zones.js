// Zones: sets of multicast groups (phonebook names or addresses), each with
// the codec and volume its speakers need.

import { h, replace } from "./dom.js";
import { state } from "./store.js";
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
  const codec = t.encoding ? encodingLabel(t.encoding) : `${defaultEncoding().split(",")[0]} (default)`;
  const opts = [codec, t.volume_db != null && `${t.volume_db > 0 ? "+" : ""}${t.volume_db} dB`].filter(Boolean);
  return opts.length ? `${t.group} (${opts.join(", ")})` : t.group;
}

function form(z, save, cancel) {
  const name = h("input", { value: z?.name ?? "", required: true, maxLength: 64 });
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

  for (const t of z?.targets ?? []) addRow(t);
  if (!rows.length) addRow();

  const submit = (e) => {
    e.preventDefault();
    save({
      name: name.value.trim(),
      targets: rows.map((r) => ({
        group: r.group.value.trim(),
        encoding: r.encoding.value || null,
        volume_db: num(r.volume),
        tos: num(r.tos),
        ttl: num(r.ttl),
        interface: r.iface.value.trim() || null,
      })),
    });
  };

  return h("form", { onsubmit: submit },
    h("fieldset", { class: "part" }, h("legend", {}, "General"), field("Name", name)),
    h("fieldset", { class: "part" }, h("legend", {}, "Groups"),
      h("p", { class: "small muted" },
        "Each group with the options its speakers need. Volume is added to the announcement's. A codec left on Default uses the one in Settings; other options left empty use the server's."),
      rowsEl,
      h("div", { class: "buttons", style: { marginTop: "10px" } },
        h("button", { type: "button", onclick: () => addRow() }, "Add group"))),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

export function mount(el) {
  mountEditor(el, {
    kind: "zone",
    path: "/zones",
    items: () => state.config.zones,
    columns: [
      ["Name", (z) => z.name],
      ["Groups", (z) => z.targets.map(describe).join(", "), true],
    ],
    form,
  });
}
