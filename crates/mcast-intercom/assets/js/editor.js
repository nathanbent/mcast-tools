// A list of zones or announcements with Edit / Delete, an Add button, and
// a form below for the one being edited. The list follows the config as it
// changes; the open form is left alone so typing isn't lost.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, isHidden } from "./store.js";

// Form helpers.

export function field(label, input, hint) {
  return h("label", { class: "field" }, h("span", {}, label), input, hint && h("span", { class: "muted small" }, hint));
}

// `options`: [value, label] pairs, or { group: "Label", options: [...] }.
export function select(options, value) {
  const el = h("select", {}, options.map((o) => Array.isArray(o)
    ? h("option", { value: o[0] }, o[1])
    : h("optgroup", { label: o.group }, o.options.map(([v, l]) => h("option", { value: v }, l)))));
  el.value = value;
  return el;
}

export function numberInput(value, props = {}) {
  return h("input", { type: "number", step: "any", value: value == null ? "" : String(value), ...props });
}

// Media file names as [prefix + name, label] options, with `current` even if
// the server no longer has it.
export function mediaOptions(prefix, current) {
  // Hidden ones are left out, unless already chosen.
  const names = state.media.filter((n) => !isHidden(n) || n === current);
  if (current && !names.includes(current)) names.push(current);
  const tags = state.config.media_tags ?? {};
  return names.map((n) => [prefix + n,
    (state.media.includes(n) ? n : `${n} (missing)`) + (tags[n]?.length ? `  (${tags[n].join(", ")})` : "")]);
}

// A box that narrows `sel`'s media file options to those whose name or
// tags match what's typed (#tag for a tag exactly). The chosen one stays.
export function mediaFilter(sel) {
  const input = h("input", { type: "search", placeholder: "Filter sounds: name, tag or #tag", "aria-label": "Filter sounds" });
  input.addEventListener("input", () => {
    const words = input.value.toLowerCase().split(/\s+/).filter(Boolean);
    const tags = state.config.media_tags ?? {};
    for (const o of sel.querySelectorAll("option")) {
      const m = /^(media|file):(.*)$/.exec(o.value);
      if (!m) continue;
      const name = m[2].toLowerCase();
      const t = (tags[m[2]] ?? []).map((x) => x.toLowerCase());
      const ok = words.every((w) => w.startsWith("#") ? t.includes(w.slice(1)) : name.includes(w) || t.some((x) => x.includes(w)));
      o.hidden = !ok && o.value !== sel.value;
    }
  });
  return input;
}

// A number input's value, or null when empty.
export const num = (input) => (input.value.trim() === "" ? null : Number(input.value));

export function mountEditor(el, spec) {
  // spec: { kind, path, items(), columns: [[header, item => text or node, wideOnly]]
  //           (or a function returning them, for views that change),
  //         form(item, save, cancel), label?(item) for messages (default: its name),
  //         before?() for anything to show above the list,
  //         sections?(items) => [{ head: node or null, items }] to show the rows
  //           under headings (a head is put in a full-width row) }
  const list = h("div");
  const formArea = h("div");
  let editing; // undefined: no form open; null: a new one; otherwise the id
  const label = spec.label ?? ((item) => item.name);

  const close = () => { editing = undefined; replace(formArea); };

  const save = async (body) => {
    const isNew = editing === null;
    try {
      const saved = isNew
        ? await api("POST", spec.path, body)
        : await api("PUT", `${spec.path}/${encodeURIComponent(editing)}`, body);
      toast(`${isNew ? "Added" : "Saved"} ${spec.kind} "${label(saved)}"`, "ok");
      close();
    } catch (e) {
      toast(`Couldn't save: ${e.message}`, "error");
    }
  };

  const open = (item) => {
    editing = item ? item.id : null;
    replace(formArea, h("div", { class: "editor-form" },
      h("h2", {}, item ? `Edit ${spec.kind}: ${label(item)}` : `New ${spec.kind}`),
      spec.form(item, save, close)));
    formArea.scrollIntoView({ behavior: "smooth", block: "nearest" });
  };

  const remove = async (item) => {
    if (!confirm(`Delete ${spec.kind} "${label(item)}"?`)) return;
    try {
      await api("DELETE", `${spec.path}/${encodeURIComponent(item.id)}`);
      toast(`Deleted ${spec.kind} "${label(item)}"`, "ok");
      if (editing === item.id) close();
    } catch (e) {
      toast(`Couldn't delete: ${e.message}`, "error");
    }
  };

  const render = () => {
    const items = spec.items();
    const columns = typeof spec.columns === "function" ? spec.columns() : spec.columns;
    replace(list,
      spec.before?.(),
      state.configError && h("p", { class: "error" }, `The intercom config has a problem: ${state.configError}`),
      items.length
        ? h("div", { class: "scroll" }, h("table", { class: "items" },
            h("thead", {}, h("tr", {}, columns.map(([head, , wide]) => h("th", { class: wide && "wide" }, head)), h("th"))),
            (spec.sections?.(items) ?? [{ head: null, items }]).map((sec) => h("tbody", {},
              sec.head && h("tr", { class: "section" }, h("td", { colspan: columns.length + 1 }, sec.head)),
              sec.items.map((item) => h("tr", { class: spec.rowClass?.(item) || null },
                columns.map(([, cell, wide]) => h("td", { class: wide && "wide" }, cell(item))),
                h("td", { class: "actions" }, h("div", { class: "buttons nowrap" },
                  h("button", { onclick: () => open(item) }, "Edit"),
                  h("button", { class: "danger", onclick: () => remove(item) }, "Delete")))))))))
        : h("p", { class: "muted" }, `No ${spec.plural ?? `${spec.kind}s`} yet.`),
      h("div", { class: "buttons", style: { marginTop: "12px" } },
        h("button", { class: "primary", onclick: () => open(null) }, `Add ${spec.kind}`)));
  };

  subscribe(["config", "server", ...(spec.parts ?? [])], render);
  render();
  el.replaceChildren(list, formArea);
  // For views that change what's listed.
  return { render };
}
