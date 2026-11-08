// Arrangeable tabs: admins put a tab's sections in the order they want, and
// every screen shows it (kept in the panel's config as `layout`). Sections
// a layout leaves out (new ones, say) follow in their usual order.
//
// `arrangeable(tab, sections)` takes the tab's sections as
// [{ id, title, el }] in their usual order, and returns the element to put
// on the page. The sections' elements are moved, never rebuilt, so what's
// typed in them stays.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, can } from "./store.js";
import { sortable } from "./drag.js";

// The ids in the order to show them.
export function ordered(tab, ids) {
  const saved = (state.config.layout?.[tab] ?? []).filter((id) => ids.includes(id));
  return [...saved, ...ids.filter((id) => !saved.includes(id))];
}

// A list to put in order by dragging (mouse or touch) or with arrows.
// `order` (ids) is changed in place; `title(id)` names each.
export function sortableList(order, title) {
  const list = h("ol", { class: "playlist-items arrange" });
  const draw = (focus) => {
    replace(list, order.map((id, i) => {
      const move = (to) => { order.splice(to, 0, ...order.splice(i, 1)); draw({ id, dir: to > i ? "down" : "up" }); };
      return h("li", { "data-i": String(i), "data-id": id },
        h("span", { class: "handle drag-handle", "aria-hidden": "true" }, "⠿"),
        h("span", { class: "grow" }, title(id)),
        h("button", { type: "button", class: "icon", "data-no-drag": "", "data-move": "up", "aria-label": `Move ${title(id)} up`, disabled: i === 0, onclick: () => move(i - 1) }, "↑"),
        h("button", { type: "button", class: "icon", "data-no-drag": "", "data-move": "down", "aria-label": `Move ${title(id)} down`, disabled: i === order.length - 1, onclick: () => move(i + 1) }, "↓"));
    }));
    // Keep the keyboard on the moved row's arrow (the other one at an end).
    if (focus) {
      const row = [...list.children].find((li) => li.dataset.id === focus.id);
      (row?.querySelector(`[data-move="${focus.dir}"]:not(:disabled)`) ?? row?.querySelector("[data-move]:not(:disabled)"))?.focus();
    }
  };
  sortable(list, {
    item: "li",
    containers: "ol.arrange",
    axis: "y",
    onDrop: (li, _c, to) => { order.splice(to, 0, ...order.splice(Number(li.dataset.i), 1)); draw(); },
    onCancel: () => draw(),
  });
  draw();
  return list;
}

// Save layouts (`{tab: order}`, or `{tab: null}` to reset) and say so.
export async function saveLayouts(changes, done) {
  try {
    for (const [tab, order] of Object.entries(changes)) {
      await api(order ? "PUT" : "DELETE", `/layout/${encodeURIComponent(tab)}`, order ? { order } : undefined);
    }
    toast(done, "ok");
    return true;
  } catch (e) {
    toast(`Couldn't save: ${e.message}`, "error");
    return false;
  }
}

// A dialog for arranging: `body` is its content; `onSave()` and
// `onReset()` return whether to close.
export function arrangeDialog(title, note, body, onSave, onReset) {
  const d = h("dialog", { class: "wide", "aria-label": title });
  d.addEventListener("close", () => d.remove());
  replace(d, h("form", { onsubmit: async (e) => { e.preventDefault(); if (await onSave()) d.close(); } },
    h("div", { class: "dialog-head" }, h("h2", {}, title),
      h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => d.close() }, "×")),
    h("p", { class: "muted small" }, note),
    body,
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"),
      h("span", { class: "spacer" }),
      h("button", { type: "button", class: "link", onclick: async () => { if (await onReset()) d.close(); } }, "Usual order"))));
  document.body.append(d);
  d.showModal();
}

function arrange(tab, sections) {
  const order = ordered(tab, sections.map((s) => s.id));
  const title = (id) => sections.find((s) => s.id === id)?.title ?? id;
  arrangeDialog("Arrange this page", "Drag the sections, or use the arrows. Everyone using the panel sees this order.",
    sortableList(order, title),
    () => saveLayouts({ [tab]: order }, "Saved the arrangement"),
    () => saveLayouts({ [tab]: null }, "Back to the usual order"));
}

export function arrangeable(tab, sections) {
  const box = h("div", { class: "stack" });
  for (const s of sections) s.el.dataset.section = s.id;
  const place = () => {
    for (const id of ordered(tab, sections.map((s) => s.id))) box.append(sections.find((s) => s.id === id).el);
  };
  place();
  subscribe(["config"], place);
  return h("div", {},
    can.edit() && h("div", { class: "arrange-bar" },
      h("button", { type: "button", class: "link", onclick: () => arrange(tab, sections) }, "Arrange this page…")),
    box);
}
