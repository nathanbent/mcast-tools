// Built-in chimes: options for the chime lists, listening to one, and a
// dialog for choosing which ones the lists show. Hiding is kept on the
// server, so it applies to every browser, mcast tui and the intercom panel.

import { api, chimeUrl } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe } from "./store.js";

const CATEGORIES = [
  ["chime", "Chimes"], ["bell", "Bells"], ["tone", "Beeps and tones"], ["alert", "Alerts and sirens"],
];

function title(name) {
  const s = name.replace(/-/g, " ");
  return s[0].toUpperCase() + s.slice(1);
}

// <option>s and <optgroup>s for a chime <select>: `first` ([value, label]
// pairs), then the shown built-ins by category. `current` stays listed even
// when hidden, so choosing a hidden chime elsewhere isn't lost here.
export function chimeOptions(first, current) {
  const option = ([v, label]) => h("option", { value: v, selected: v === current }, label);
  const groups = CATEGORIES.map(([cat, label]) => {
    const chimes = state.chimes.filter((c) => c.category === cat && (!c.hidden || c.name === current));
    return chimes.length && h("optgroup", { label },
      chimes.map((c) => option([c.name, title(c.name)])));
  });
  const known = first.some(([v]) => v === current) || state.chimes.some((c) => c.name === current);
  // A name this server doesn't know (from an older or newer version).
  const extra = current && !known ? option([current, current]) : null;
  return [...first.map(option), extra, ...groups];
}

let player = null;

// Play a built-in chime in the browser (stopping any already playing).
export function listen(name) {
  player?.pause();
  player = new Audio(chimeUrl(name));
  player.play().catch((e) => toast(`Can't play ${name}: ${e.message}`, "error"));
}

// A small button that plays whichever built-in `select` has chosen.
export function listenButton(select, fallback = {}) {
  return h("button", {
    type: "button", class: "icon", title: "Listen", "aria-label": "Listen to this chime",
    onclick: () => {
      const name = fallback[select.value] ?? select.value;
      if (state.chimes.some((c) => c.name === name)) listen(name);
      else toast("Only built-in chimes can be previewed here.", "info");
    },
  }, "▶");
}

async function setHidden(c, hidden, box) {
  box.disabled = true;
  try {
    Object.assign(c, await api("PUT", `/chimes/${encodeURIComponent(c.name)}`, { hidden }));
  } catch (e) {
    box.checked = !c.hidden;
    toast(e.message, "error");
  } finally {
    box.disabled = false;
  }
}

let dialog = null;

function renderDialog(body) {
  const shown = state.chimes.filter((c) => !c.hidden).length;
  const all = (hide) => async () => {
    try {
      state.chimes = await api("PUT", "/chimes", { hidden: hide ? state.chimes.map((c) => c.name) : [] });
    } catch (e) {
      toast(e.message, "error");
    }
    renderDialog(body);
  };
  replace(body,
    h("p", { class: "muted" },
      `Showing ${shown} of ${state.chimes.length}. Hidden chimes are left out of the chime lists everywhere, `,
      "but pages that already use one still play it."),
    h("div", { class: "buttons" },
      h("button", { type: "button", onclick: all(false) }, "Show all"),
      h("button", { type: "button", onclick: all(true) }, "Hide all")),
    CATEGORIES.map(([cat, label]) => {
      const chimes = state.chimes.filter((c) => c.category === cat);
      return chimes.length && h("fieldset", { class: "chime-list" }, h("legend", {}, label),
        chimes.map((c) => {
          const box = h("input", { type: "checkbox", checked: !c.hidden,
            onchange: () => setHidden(c, !box.checked, box) });
          return h("div", { class: "chime-row" },
            h("button", { type: "button", class: "icon", title: "Listen", "aria-label": `Listen to ${c.name}`,
              onclick: () => listen(c.name) }, "▶"),
            h("label", { class: "check" }, box,
              h("span", {}, h("strong", {}, title(c.name)), h("small", { class: "muted" }, c.description))));
        }));
    }));
}

// The dialog for choosing which chimes the lists show.
export function openChooser() {
  if (!dialog) {
    const body = h("div", { class: "chime-body" });
    dialog = h("dialog", { class: "wide", "aria-labelledby": "chimes-title" },
      h("div", { class: "dialog-head" },
        h("h2", { id: "chimes-title" }, "Chimes to list"),
        h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => dialog.close() }, "×")),
      body);
    dialog.addEventListener("close", () => player?.pause());
    document.body.append(dialog);
    subscribe((what) => { if (what === "chimes" && dialog.open) renderDialog(body); });
    dialog.body = body;
  }
  renderDialog(dialog.body);
  dialog.showModal();
}
