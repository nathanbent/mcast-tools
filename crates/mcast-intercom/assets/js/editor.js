// A list of zones or announcements with Edit / Delete, an Add button, and
// a form below for the one being edited. The list follows the config as it
// changes; the open form is left alone so typing isn't lost.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe } from "./store.js";

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

// A number input's value, or null when empty.
export const num = (input) => (input.value.trim() === "" ? null : Number(input.value));

export function mountEditor(el, spec) {
  // spec: { kind, path, items(), columns: [[header, item => text, wideOnly]], form(item, save, cancel) }
  const list = h("div");
  const formArea = h("div");
  let editing; // undefined: no form open; null: a new one; otherwise the id

  const close = () => { editing = undefined; replace(formArea); };

  const save = async (body) => {
    const isNew = editing === null;
    try {
      const saved = isNew
        ? await api("POST", spec.path, body)
        : await api("PUT", `${spec.path}/${encodeURIComponent(editing)}`, body);
      toast(`${isNew ? "Added" : "Saved"} ${spec.kind} "${saved.name}"`, "ok");
      close();
    } catch (e) {
      toast(`Couldn't save: ${e.message}`, "error");
    }
  };

  const open = (item) => {
    editing = item ? item.id : null;
    replace(formArea, h("div", { class: "editor-form" },
      h("h2", {}, item ? `Edit ${spec.kind}: ${item.name}` : `New ${spec.kind}`),
      spec.form(item, save, close)));
    formArea.scrollIntoView({ behavior: "smooth", block: "nearest" });
  };

  const remove = async (item) => {
    if (!confirm(`Delete ${spec.kind} "${item.name}"?`)) return;
    try {
      await api("DELETE", `${spec.path}/${encodeURIComponent(item.id)}`);
      toast(`Deleted ${spec.kind} "${item.name}"`, "ok");
      if (editing === item.id) close();
    } catch (e) {
      toast(`Couldn't delete: ${e.message}`, "error");
    }
  };

  const render = () => {
    const items = spec.items();
    replace(list,
      state.configError && h("p", { class: "error" }, `The intercom config has a problem: ${state.configError}`),
      items.length
        ? h("div", { class: "scroll" }, h("table", { class: "items" },
            h("thead", {}, h("tr", {}, spec.columns.map(([head, , wide]) => h("th", { class: wide && "wide" }, head)), h("th"))),
            h("tbody", {}, items.map((item) => h("tr", {},
              spec.columns.map(([, cell, wide]) => h("td", { class: wide && "wide" }, cell(item))),
              h("td", { class: "actions" }, h("div", { class: "buttons nowrap" },
                h("button", { onclick: () => open(item) }, "Edit"),
                h("button", { class: "danger", onclick: () => remove(item) }, "Delete"))))))))
        : h("p", { class: "muted" }, `No ${spec.kind}s yet.`),
      h("div", { class: "buttons", style: { marginTop: "12px" } },
        h("button", { class: "primary", onclick: () => open(null) }, `Add ${spec.kind}`)));
  };

  subscribe(["config", "server"], render);
  render();
  el.replaceChildren(list, formArea);
}
