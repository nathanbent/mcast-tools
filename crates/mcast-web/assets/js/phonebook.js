// The phonebook: groups saved under a name and description. Names can be
// typed (or picked from the suggestions) in any group field.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, bookLabel } from "./store.js";
import { useGroup } from "./page.js";

let list;
let editor;
// The entry being edited (its name before any rename), or null when adding.
let editing = null;

async function save(form) {
  const v = Object.fromEntries(new FormData(form));
  const name = v.name.trim();
  const body = { group: v.group.trim(), description: v.description.trim() };
  const path = editing ?? name;
  if (editing && editing !== name) body.name = name;
  try {
    await api("PUT", `/phonebook/${encodeURIComponent(path)}`, body);
    toast(editing ? `Updated “${name}”` : `Added “${name}”`, "ok");
    editing = null;
    renderForm(false);
  } catch (e) {
    toast(e.message, "error");
  }
}

async function remove(entry) {
  if (!confirm(`Remove “${entry.name}” (${entry.group}) from the phonebook?`)) return;
  try {
    await api("DELETE", `/phonebook/${encodeURIComponent(entry.name)}`);
    if (editing === entry.name) { editing = null; renderForm(false); }
    toast(`Removed “${entry.name}”`, "ok");
  } catch (e) {
    toast(e.message, "error");
  }
}

function row(e) {
  return h("tr", {},
    h("td", {}, h("strong", {}, e.name), h("div", { class: "narrow-only mono" }, e.group),
      e.description && h("div", { class: "muted small" }, e.description)),
    h("td", { class: "mono" }, e.group),
    h("td", { class: "actions" },
      h("span", { class: "buttons nowrap" },
        h("button", { title: "Use this group in the Page form", onclick: () => useGroup(e.name) }, "Page"),
        h("button", { onclick: () => { editing = e.name; renderForm(false); } }, "Edit"),
        h("button", { class: "danger", "aria-label": `Remove ${e.name}`, onclick: () => remove(e) }, "×"))));
}

function form() {
  const e = editing != null ? state.phonebook.find((x) => x.name === editing) : null;
  if (editing != null && !e) editing = null; // removed elsewhere
  return h("form", { class: "book-form", onsubmit: (ev) => { ev.preventDefault(); save(ev.target); } },
    h("div", { class: "row" },
      h("label", { class: "field" }, h("span", {}, "Name"),
        h("input", { name: "name", required: true, maxlength: 64, value: e?.name ?? "", placeholder: "zone-1",
          title: "Letters, digits, spaces, - _ and ." })),
      h("label", { class: "field" }, h("span", {}, "Group"),
        h("input", { name: "group", required: true, value: e?.group ?? "", placeholder: "239.192.2.1:10000",
          class: "mono" }))),
    h("label", { class: "field" }, h("span", {}, "Description"),
      h("input", { name: "description", maxlength: 500, value: e?.description ?? "",
        placeholder: "e.g. Ground floor speakers (G.711)" })),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, e ? "Save changes" : "Add to phonebook"),
      e && h("button", { type: "button", onclick: () => { editing = null; renderForm(false); } }, "Cancel")));
}

function renderList() {
  replace(list, state.phonebook.length
    ? h("div", { class: "scroll" }, h("table", { class: "book" },
        h("thead", {}, h("tr", {}, ["Name", "Group", ""].map((c) => h("th", {}, c)))),
        h("tbody", {}, state.phonebook.map(row))))
    : h("p", { class: "muted" }, "No saved groups yet. Add one below, then type its name wherever a group goes."));
}

// `open`: keep the "Add a group" section open (after adding one, say).
function renderForm(open) {
  if (editing != null) {
    replace(editor, form());
    editor.querySelector("input[name=name]")?.focus();
    editor.scrollIntoView({ behavior: "smooth", block: "nearest" });
  } else {
    replace(editor, h("details", { class: "book-add", open: open || state.phonebook.length === 0 },
      h("summary", {}, "Add a group"), form()));
  }
}

// Suggestions for every group field outside the Page form.
function suggestions() {
  replace(document.getElementById("phonebook-groups"),
    state.phonebook.map((e) => h("option", { value: e.name, label: bookLabel(e) })));
}

export function mount(el) {
  list = h("div");
  editor = h("div");
  el.append(list, editor);
  renderList();
  renderForm(false);
  suggestions();
  subscribe((what) => {
    if (what !== "phonebook") return;
    renderList();
    suggestions();
    // Leave a half-typed entry alone, unless the one being edited is gone.
    if (editing != null && !state.phonebook.some((e) => e.name === editing)) {
      editing = null;
      renderForm(false);
    }
  });
}
