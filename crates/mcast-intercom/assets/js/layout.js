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

// The ids in the order to show them.
export function ordered(tab, ids) {
  const saved = (state.config.layout?.[tab] ?? []).filter((id) => ids.includes(id));
  return [...saved, ...ids.filter((id) => !saved.includes(id))];
}

function arrange(tab, sections) {
  const order = ordered(tab, sections.map((s) => s.id));
  const title = (id) => sections.find((s) => s.id === id)?.title ?? id;
  const d = h("dialog", { "aria-label": "Arrange this page" });
  d.addEventListener("close", () => d.remove());
  const list = h("ol", { class: "playlist-items arrange" });
  let dragging = null;
  const draw = () => replace(list, order.map((id, i) => {
    const move = (to) => { order.splice(to, 0, ...order.splice(i, 1)); draw(); };
    const li = h("li", {
      draggable: "true",
      ondragstart: (e) => { dragging = i; e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", id); },
      ondragover: (e) => { if (dragging != null) { e.preventDefault(); li.classList.add("drop-here"); } },
      ondragleave: () => li.classList.remove("drop-here"),
      ondrop: (e) => {
        e.preventDefault();
        li.classList.remove("drop-here");
        if (dragging != null && dragging !== i) { const from = dragging; dragging = null; order.splice(i, 0, ...order.splice(from, 1)); draw(); }
      },
      ondragend: () => { dragging = null; },
    },
      h("span", { class: "handle", "aria-hidden": "true" }, "⠿"),
      h("span", { class: "grow" }, title(id)),
      h("button", { type: "button", class: "icon", "aria-label": `Move ${title(id)} up`, disabled: i === 0, onclick: () => move(i - 1) }, "↑"),
      h("button", { type: "button", class: "icon", "aria-label": `Move ${title(id)} down`, disabled: i === order.length - 1, onclick: () => move(i + 1) }, "↓"));
    return li;
  }));
  draw();
  const send = async (method, body, done) => {
    try {
      await api(method, `/layout/${encodeURIComponent(tab)}`, body);
      toast(done, "ok");
      d.close();
    } catch (e) {
      toast(`Couldn't save: ${e.message}`, "error");
    }
  };
  replace(d, h("form", { onsubmit: (e) => { e.preventDefault(); send("PUT", { order }, "Saved the arrangement"); } },
    h("div", { class: "dialog-head" }, h("h2", {}, "Arrange this page"),
      h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => d.close() }, "×")),
    h("p", { class: "muted small" }, "Drag the sections, or use the arrows. Everyone using the panel sees this order."),
    list,
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"),
      h("span", { class: "spacer" }),
      h("button", { type: "button", class: "link", onclick: () => send("DELETE", undefined, "Back to the usual order") }, "Usual order"))));
  document.body.append(d);
  d.showModal();
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
