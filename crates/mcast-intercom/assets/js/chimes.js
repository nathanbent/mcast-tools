// The mcast server's built-in chimes: options for the chime lists, listening
// to one, and a dialog for choosing which ones the lists show. Hiding is
// kept on the mcast server, so it applies to its own web page and TUI too.

import { api, token } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, refreshChimes } from "./store.js";
import { field, select, mediaOptions } from "./editor.js";

const CATEGORIES = [
  ["chime", "Chimes"], ["bell", "Bells"], ["tone", "Beeps and tones"], ["alert", "Alerts and sirens"],
];

// Until the server says otherwise (or if it's too old to list them).
const FALLBACK = ["rising", "falling", "ding-dong", "arpeggio", "bell", "alert"];

export function title(name) {
  const s = name.replace(/-/g, " ");
  return s[0].toUpperCase() + s.slice(1);
}

// Option groups (for editor.js's select()) of the built-ins the lists show,
// plus `current` even if hidden, so choosing a hidden chime isn't lost.
export function builtInGroups(current) {
  if (!state.chimes.length) {
    const names = FALLBACK.includes(current) || !current ? FALLBACK : [...FALLBACK, current];
    return [{ group: "Built in", options: names.map((c) => [c, title(c)]) }];
  }
  const groups = CATEGORIES.map(([cat, label]) => ({
    group: label,
    options: state.chimes
      .filter((c) => c.category === cat && (!c.hidden || c.name === current))
      .map((c) => [c.name, title(c.name)]),
  })).filter((g) => g.options.length);
  return groups;
}

let player = null;

// Play a built-in chime in the browser (stopping any already playing).
export function listen(name) {
  player?.pause();
  const t = token();
  player = new Audio(`/api/intercom/v1/chimes/${encodeURIComponent(name)}/preview${t ? `?access_token=${encodeURIComponent(t)}` : ""}`);
  player.play().catch((e) => toast(`Can't play ${name}: ${e.message}`, "error"));
}

// A small button that plays whichever built-in `select` has chosen;
// `fallback` maps other values (such as "default") to a built-in, and may
// be a function returning that map.
export function listenButton(select, fallback = {}) {
  return h("button", {
    type: "button", class: "icon", title: "Listen", "aria-label": "Listen to this chime",
    onclick: () => {
      const map = typeof fallback === "function" ? fallback() : fallback;
      const name = map[select.value] ?? select.value;
      if (state.chimes.some((c) => c.name === name)) listen(name);
      else toast("Only the mcast server's built-in chimes can be previewed.", "info");
    },
  }, "▶");
}

async function setHidden(names) {
  try {
    state.chimes = await api("PUT", "/chimes", { hidden: names });
  } catch (e) {
    toast(`Couldn't change the chimes: ${e.message}`, "error");
  }
}

let dialog = null;
let body = null;
let onChange = null;

function render() {
  const hidden = state.chimes.filter((c) => c.hidden).map((c) => c.name);
  const change = async (names) => { await setHidden(names); render(); onChange?.(); };
  replace(body,
    state.chimes.length
      ? h("p", { class: "muted" },
        `Showing ${state.chimes.length - hidden.length} of ${state.chimes.length}. Hidden chimes are left out of `,
        "the chime lists here and on the mcast server, but announcements that already use one still play it.")
      : h("p", { class: "error" }, "The mcast server didn't list its chimes (it may be unreachable, or too old)."),
    h("div", { class: "buttons" },
      h("button", { type: "button", onclick: () => change([]) }, "Show all"),
      h("button", { type: "button", onclick: () => change(state.chimes.map((c) => c.name)) }, "Hide all")),
    CATEGORIES.map(([cat, label]) => {
      const chimes = state.chimes.filter((c) => c.category === cat);
      return chimes.length > 0 && h("fieldset", { class: "chime-list" }, h("legend", {}, label),
        chimes.map((c) => {
          const box = h("input", {
            type: "checkbox", checked: !c.hidden,
            onchange: () => {
              box.disabled = true;
              change(box.checked ? hidden.filter((n) => n !== c.name) : [...hidden, c.name]);
            },
          });
          return h("div", { class: "chime-row" },
            h("button", { type: "button", class: "icon", title: "Listen", "aria-label": `Listen to ${c.name}`,
              onclick: () => listen(c.name) }, "▶"),
            h("label", { class: "check" }, box,
              h("span", {}, h("strong", {}, title(c.name)), h("small", { class: "muted" }, c.description))));
        }));
    }));
}

// The dialog for choosing which chimes the lists show; `changed` is called
// after each change (to redraw an open form's lists).
export async function openChooser(changed) {
  onChange = changed;
  if (!dialog) {
    body = h("div", { class: "chime-body" });
    dialog = h("dialog", { class: "wide", "aria-labelledby": "chimes-title" },
      h("div", { class: "dialog-head" },
        h("h2", { id: "chimes-title" }, "Chimes to list"),
        h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => dialog.close() }, "×")),
      body);
    dialog.addEventListener("close", () => player?.pause());
    document.body.append(dialog);
  }
  await refreshChimes();
  render();
  dialog.showModal();
}

// ---- Chime fields, for announcements and the panel's defaults ----

// The mcast server's own defaults, which "default" means.
export const SERVER_DEFAULT = { chime: "rising", end_chime: "falling" };

// A chime as the config has it ("bell", "default", "none", {file}) <-> a
// select value; "" is "not set".
export function chimeValue(c) {
  if (c == null) return "";
  return typeof c === "string" ? c : `file:${c.file}`;
}

export function chimeOf(v) {
  if (v === "") return null;
  return v.startsWith("file:") ? { file: v.slice(5) } : v;
}

// For people: "Ding dong", "lunch.wav", "None", "Server default (rising)".
export function chimeLabel(c, which = "chime") {
  if (c == null || c === "none") return "None";
  if (typeof c === "object") return c.file;
  if (c === "default") return `Server default (${SERVER_DEFAULT[which]})`;
  return title(c);
}

// The chime an announcement plays (null for none), with the panel's
// defaults filled in, as the panel does when it plays it.
export function effectiveChime(a, which) {
  const own = a[which];
  if (own === "none") return null;
  if (own != null) return own;
  const d = state.config.defaults?.[which];
  return d == null || d === "none" ? null : d;
}

// `which`: "chime" or "end_chime". `inherit`: true in an announcement, where
// "not set" means the panel's default and "none" turns it off; false in the
// defaults themselves, where "not set" is none.
function chimeOptions(current, which, inherit) {
  const file = current && typeof current === "object" ? current.file : null;
  const named = typeof current === "string" && !["default", "none"].includes(current) ? current : null;
  const panelDefault = chimeLabel(state.config.defaults?.[which], which);
  return [
    ...(inherit ? [["", `Panel default (${panelDefault})`], ["none", "None"]] : [["", "None"]]),
    ["default", `Server default (${SERVER_DEFAULT[which]})`],
    ...builtInGroups(named),
    { group: "Media files", options: mediaOptions("file:", file) },
  ];
}

// A labelled chime select with a button to listen to it. Its `redraw()`
// rebuilds the choices (keeping the one chosen), for when the chimes to
// list or the defaults change.
export function chimeField(label, current, which, { inherit = false, hint } = {}) {
  const sel = select(chimeOptions(current, which, inherit), chimeValue(current));
  const toBuiltIn = () => {
    const map = { default: SERVER_DEFAULT[which] };
    const d = state.config.defaults?.[which];
    if (inherit && typeof d === "string") map[""] = d === "default" ? SERVER_DEFAULT[which] : d;
    return map;
  };
  const wrap = h("span", { class: "with-button" }, sel, listenButton(sel, toBuiltIn));
  sel.redraw = () => {
    const value = sel.value;
    const now = select(chimeOptions(chimeOf(value), which, inherit), value);
    sel.replaceChildren(...now.childNodes);
    sel.value = value;
  };
  return { el: field(label, wrap, hint), select: sel, value: () => chimeOf(sel.value) };
}

// "Choose which chimes to list…", redrawing `fields` after a change.
export function chooserLink(fields) {
  return h("button", { type: "button", class: "link",
    onclick: () => openChooser(() => fields.forEach((f) => f.select.redraw())) },
  "Choose which chimes to list…");
}
