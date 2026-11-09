// Playlists: sounds and text pages that play one after another, as one
// stream, with the announcement's chimes before and after. Announcements
// play them (or a part of one does). A text page is spoken by the mcast
// server's text to speech, shown on the zones' displays, or both.

import { h, replace, toast, seconds } from "./dom.js";
import { api, mediaUrl } from "./api.js";
import { state } from "./store.js";
import { mountEditor, field, select, numberInput, num, mediaOptions, mediaFilter } from "./editor.js";

const duration = (media) => state.library.files.find((f) => f.name === media)?.audio?.duration_s;

const isText = (i) => i.type === "text";
const spoken = (i) => isText(i) && i.speak !== false;
const heard = (i) => !isText(i) || spoken(i);
// A page only shown is a pause of its own in the stream, for this long.
const SHOW_SECS = 5;
const showOnly = (i) => isText(i) && i.speak === false && i.show !== false;
const showFor = (i) => i.seconds ?? SHOW_SECS;
// What goes in the stream: sounds, spoken pages and the pauses for pages only shown.
const inStream = (i) => !isText(i) || spoken(i) || showOnly(i);

// "1:05" for a total ("1:05 + speech" with spoken pages), or "" when a
// sound's length isn't known.
export function playlistLength(p) {
  const sounds = p.items.filter((i) => !isText(i));
  const lengths = [...sounds.map((i) => duration(i.media)), ...p.items.filter(showOnly).map(showFor)];
  if (lengths.some((l) => l == null)) return "";
  const gaps = Math.max(0, p.items.filter(inStream).length - 1);
  const speech = p.items.some(spoken);
  if (!lengths.length) return speech ? "speech" : "";
  const total = seconds(lengths.reduce((a, b) => a + b, 0) + ((p.gap_ms ?? 0) / 1000) * gaps);
  return speech ? `${total} + speech` : total;
}

// The mcast server's voices, asked for once.
let voices = null;
function loadVoices(then) {
  if (voices) return then();
  api("GET", "/voices")
    .then((v) => { voices = v; then(); })
    .catch(() => { voices = { engine: null, voices: [] }; then(); });
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

// A text page's own controls: the text, whether it's spoken and shown,
// the voice, and how long one that's only shown is up.
function textControls(item, i, changed) {
  const text = h("textarea", {
    rows: 2,
    maxLength: 2000,
    required: true,
    placeholder: "e.g. The library closes in ten minutes.",
    "aria-label": `Text of item ${i + 1}`,
    oninput: () => { item.text = text.value; },
  });
  text.value = item.text ?? "";
  const check = (key, label) => {
    const box = h("input", { type: "checkbox", checked: item[key] !== false });
    box.addEventListener("change", () => {
      if (box.checked) delete item[key]; else item[key] = false;
      voice.disabled = item.speak === false;
      if (!showOnly(item)) delete item.seconds;
      showing.hidden = !showOnly(item);
      changed();
    });
    return h("label", { class: "check" }, box, label);
  };
  // Only a page that's only shown has a time: it holds the stream that long.
  const secs = numberInput(item.seconds ?? SHOW_SECS, {
    min: 1, max: 60, step: 1, required: true, class: "secs",
    "aria-label": `Seconds to show item ${i + 1}`,
    oninput: () => {
      const n = num(secs);
      if (n == null || n === SHOW_SECS) delete item.seconds; else item.seconds = n;
      changed();
    },
  });
  const showing = h("label", { class: "check", title: "The playlist pauses, in silence, while it's up" },
    "Show for", secs, "s");
  showing.hidden = !showOnly(item);
  const names = voices?.voices ?? [];
  const voice = select([["", voices?.default_voice ? `Default (${voices.default_voice})` : "Default voice"],
    ...[...new Set([...names, ...(item.voice && !names.includes(item.voice) ? [item.voice] : [])])].map((v) => [v, v])], item.voice ?? "");
  voice.setAttribute("aria-label", `Voice for item ${i + 1}`);
  voice.disabled = item.speak === false;
  voice.addEventListener("change", () => { if (voice.value) item.voice = voice.value; else delete item.voice; });
  const noEngine = voices && !voices.engine;
  return h("div", { class: "grow text-item" }, text,
    h("div", { class: "row small" }, check("speak", "Speak"), check("show", "Show on displays"), showing,
      names.length > 0 || item.voice ? voice : null,
      noEngine && h("span", { class: "muted" }, "The mcast server has no text to speech yet: install Piper or eSpeak NG there.")));
}

// The items, as an ordered list: reorder by dragging or with the arrows.
function itemsEditor(initial) {
  const items = initial.map((i) => ({ ...i }));
  const list = h("ol", { class: "playlist-items" });
  const total = h("p", { class: "muted small" });
  let dragging = null;
  const draw = () => {
    replace(list, items.map((item, i) => {
      const len = isText(item) ? null : duration(item.media);
      const label = isText(item) ? `text page ${i + 1}` : item.media;
      const move = (to) => { const [x] = items.splice(i, 1); items.splice(to, 0, x); draw(); };
      // A text page drags by its handle only, so its text can be selected.
      const handle = h("span", { class: "handle", "aria-hidden": "true", title: "Drag to reorder" }, "⠿");
      const li = h("li", {
        class: isText(item) ? "text" : null,
        draggable: isText(item) ? "false" : "true",
        ondragstart: (e) => { dragging = i; e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", label); },
        ondragover: (e) => { if (dragging != null) { e.preventDefault(); li.classList.add("drop-here"); } },
        ondragleave: () => li.classList.remove("drop-here"),
        ondrop: (e) => {
          e.preventDefault();
          li.classList.remove("drop-here");
          if (dragging != null && dragging !== i) { const from = dragging; dragging = null; const [x] = items.splice(from, 1); items.splice(i, 0, x); draw(); }
        },
        ondragend: () => { dragging = null; if (isText(item)) li.draggable = false; },
      },
        handle,
        isText(item)
          ? h("span", { class: "badge", title: "Text page" }, "T")
          : h("button", { type: "button", class: "icon", "aria-label": `Listen to ${item.media}`, onclick: () => listen(item.media) }, "▶"),
        isText(item)
          ? textControls(item, i, count)
          : h("span", { class: "grow" }, item.media,
            !state.media.includes(item.media) && h("span", { class: "badge failed" }, "missing")),
        h("span", { class: "muted small" }, len == null ? "" : len < 10 ? `${len.toFixed(1)} s` : seconds(len)),
        h("button", { type: "button", class: "icon", "aria-label": "Move up", disabled: i === 0, onclick: () => move(i - 1) }, "↑"),
        h("button", { type: "button", class: "icon", "aria-label": "Move down", disabled: i === items.length - 1, onclick: () => move(i + 1) }, "↓"),
        h("button", { type: "button", class: "icon danger", "aria-label": `Take ${label} out`, onclick: () => { items.splice(i, 1); draw(); } }, "✕"));
      if (isText(item)) {
        handle.addEventListener("pointerdown", () => { li.draggable = true; });
        handle.addEventListener("pointerup", () => { li.draggable = false; });
      }
      return li;
    }));
    count();
  };
  // The count and length under the list.
  const count = () => {
    const length = playlistLength({ items, gap_ms: 0 });
    total.textContent = items.length
      ? `${items.length} item${items.length === 1 ? "" : "s"}${length ? `, ${length}` : ""}`
        + (items.length && !items.some(heard) ? ". Nothing in it is heard yet: it needs a sound or a spoken page to play." : "")
      : "Nothing in it yet: add sounds or text pages below.";
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
  const addText = h("button", {
    type: "button",
    onclick: () => {
      items.push({ type: "text", text: "" });
      draw();
      list.querySelectorAll("textarea")[list.querySelectorAll("textarea").length - 1]?.focus();
    },
  }, "Add a text page");
  draw();
  loadVoices(draw);
  return {
    el: h("div", {}, list, total,
      h("div", { class: "row add-item" }, field("Sound", pick), mediaFilter(pick), add, addText)),
    value: () => items.map((i) => (isText(i) ? { ...i, text: (i.text ?? "").trim() } : i)),
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
    h("p", { class: "small muted" }, "It plays as one stream, with the announcement's chimes before and after. Files at other sample rates are converted to the first one's. A text page is spoken by the mcast server, shown on the displays of the zones it plays to, or both; one that's only shown is up for its own time, while the playlist pauses in silence."),
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
    before: () => h("p", { class: "muted" }, "A playlist plays sounds and text pages one after another, as one announcement. Choose one as an announcement's audio, or for one of its parts."),
    columns: [
      ["Name", (p) => p.name],
      ["Items", (p) => String(p.items.length)],
      ["Length", (p) => playlistLength(p) || h("span", { class: "muted" }, "—"), true],
      ["Used by", (p) => usedBy(p).join(", ") || h("span", { class: "muted" }, "—"), true],
    ],
    form,
  });
}
