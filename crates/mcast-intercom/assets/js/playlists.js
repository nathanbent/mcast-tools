// Playlists: sounds that play one after another, as one stream, with the
// announcement's chimes before and after. Announcements play them (or a
// part of one does). Later, other kinds of page (text) can go in too.

import { h, replace, toast, seconds } from "./dom.js";
import { api, mediaUrl } from "./api.js";
import { state } from "./store.js";
import { mountEditor, field, select, numberInput, num, mediaOptions, mediaFilter } from "./editor.js";

const duration = (media) => state.library.files.find((f) => f.name === media)?.audio?.duration_s;

// "1:05" for a total, or "" when a length isn't known.
export function playlistLength(p) {
  const lengths = p.items.map((i) => duration(i.media));
  if (!lengths.length || lengths.some((l) => l == null)) return "";
  return seconds(lengths.reduce((a, b) => a + b, 0) + ((p.gap_ms ?? 0) / 1000) * (lengths.length - 1));
}

const usedBy = (p) => state.config.announcements
  .filter((a) => [a.audio, ...(a.parts ?? []).map((x) => x.audio)].some((au) => au?.playlist === p.id))
  .map((a) => a.name);

let player = null;
function listen(media) {
  player?.pause();
  player = new Audio(mediaUrl(media));
  player.play().catch((e) => toast(`Can't play ${media}: ${e.message}`, "error"));
}

// The items, as an ordered list: reorder by dragging or with the arrows.
function itemsEditor(initial) {
  const items = initial.map((i) => ({ ...i }));
  const list = h("ol", { class: "playlist-items" });
  const total = h("p", { class: "muted small" });
  let dragging = null;
  const draw = () => {
    replace(list, items.map((item, i) => {
      const len = duration(item.media);
      const move = (to) => { const [x] = items.splice(i, 1); items.splice(to, 0, x); draw(); };
      const li = h("li", {
        draggable: "true",
        ondragstart: (e) => { dragging = i; e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", item.media); },
        ondragover: (e) => { if (dragging != null) { e.preventDefault(); li.classList.add("drop-here"); } },
        ondragleave: () => li.classList.remove("drop-here"),
        ondrop: (e) => {
          e.preventDefault();
          li.classList.remove("drop-here");
          if (dragging != null && dragging !== i) { const from = dragging; dragging = null; const [x] = items.splice(from, 1); items.splice(i, 0, x); draw(); }
        },
        ondragend: () => { dragging = null; },
      },
        h("span", { class: "handle", "aria-hidden": "true", title: "Drag to reorder" }, "⠿"),
        h("button", { type: "button", class: "icon", "aria-label": `Listen to ${item.media}`, onclick: () => listen(item.media) }, "▶"),
        h("span", { class: "grow" }, item.media,
          !state.media.includes(item.media) && h("span", { class: "badge failed" }, "missing")),
        h("span", { class: "muted small" }, len == null ? "" : len < 10 ? `${len.toFixed(1)} s` : seconds(len)),
        h("button", { type: "button", class: "icon", "aria-label": "Move up", disabled: i === 0, onclick: () => move(i - 1) }, "↑"),
        h("button", { type: "button", class: "icon", "aria-label": "Move down", disabled: i === items.length - 1, onclick: () => move(i + 1) }, "↓"),
        h("button", { type: "button", class: "icon danger", "aria-label": `Take ${item.media} out`, onclick: () => { items.splice(i, 1); draw(); } }, "✕"));
      return li;
    }));
    total.textContent = items.length
      ? `${items.length} item${items.length === 1 ? "" : "s"}${playlistLength({ items, gap_ms: 0 }) ? `, ${playlistLength({ items, gap_ms: 0 })} of sound` : ""}`
      : "Nothing in it yet: add sounds below.";
  };
  const pick = select(mediaOptions("media:", null), "");
  const add = h("button", {
    type: "button",
    onclick: () => {
      if (!pick.value.startsWith("media:")) return;
      items.push({ type: "sound", media: pick.value.slice(6) });
      draw();
    },
  }, "Add");
  draw();
  return {
    el: h("div", {}, list, total,
      h("div", { class: "row add-item" }, field("Sound", pick), mediaFilter(pick), add)),
    value: () => items,
  };
}

function form(p, save, cancel) {
  const name = h("input", { value: p?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Closing message" });
  const gap = numberInput(p ? (p.gap_ms ?? 0) / 1000 : 0.5, { min: 0, max: 60, step: 0.1 });
  const items = itemsEditor(p?.items ?? []);
  return h("form", {
    onsubmit: (e) => {
      e.preventDefault();
      save({ name: name.value.trim(), gap_ms: Math.round((num(gap) ?? 0) * 1000), items: items.value() });
    },
  },
    h("div", { class: "row" }, field("Name", name), field("Seconds between items", gap)),
    h("fieldset", { class: "part" }, h("legend", {}, "Plays, in order"), items.el),
    h("p", { class: "small muted" }, "It plays as one stream, with the announcement's chimes before and after. Files at other sample rates are converted to the first one's."),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

export function mount(el) {
  mountEditor(el, {
    kind: "playlist",
    path: "/playlists",
    items: () => state.config.playlists ?? [],
    parts: ["library"],
    before: () => h("p", { class: "muted" }, "A playlist plays sounds one after another, as one announcement. Choose one as an announcement's audio, or for one of its parts."),
    columns: [
      ["Name", (p) => p.name],
      ["Items", (p) => String(p.items.length)],
      ["Length", (p) => playlistLength(p) || h("span", { class: "muted" }, "—"), true],
      ["Used by", (p) => usedBy(p).join(", ") || h("span", { class: "muted" }, "—"), true],
    ],
    form,
  });
}
