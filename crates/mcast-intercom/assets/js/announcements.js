// Announcements: what to play (a media file or a chime), chimes, where it
// goes, how many times, and its priority.

import { h } from "./dom.js";
import { state, zoneName } from "./store.js";
import { mountEditor, field, select, numberInput, num } from "./editor.js";
import { builtInGroups, listenButton, openChooser } from "./chimes.js";

const PRIORITIES = [["normal", "Normal"], ["high", "High"], ["emergency", "Emergency"]];

function mediaOptions(prefix, current) {
  const names = [...state.media];
  if (current && !names.includes(current)) names.push(current);
  return names.map((n) => [prefix + n, state.media.includes(n) ? n : `${n} (missing)`]);
}

// A chime as the API has it ("bell", "default", {file}) <-> a select value.
function chimeValue(c) {
  if (c == null) return "";
  return typeof c === "string" ? c : `file:${c.file}`;
}

function chimeOf(v) {
  if (v === "") return null;
  return v.startsWith("file:") ? { file: v.slice(5) } : v;
}

function chimeOptions(current, defaultLabel) {
  const file = current && typeof current === "object" ? current.file : null;
  return [
    ["", "None"],
    ["default", defaultLabel],
    ...builtInGroups(typeof current === "string" ? current : null),
    { group: "Media files", options: mediaOptions("file:", file) },
  ];
}

function chimeSelect(current, defaultLabel) {
  return select(chimeOptions(current, defaultLabel), chimeValue(current));
}

// A chime select with a button to listen to it, redrawn (keeping its
// choice) when the chimes to list change.
function chimeField(label, sel, defaultLabel, defaultName) {
  const wrap = h("span", { class: "with-button" }, sel, listenButton(sel, { default: defaultName }));
  sel.redraw = () => {
    const value = sel.value;
    const now = select(chimeOptions(chimeOf(value), defaultLabel), value);
    sel.replaceChildren(...now.childNodes);
    sel.value = value;
  };
  return field(label, wrap);
}

function audioLabel(a) {
  return a.audio === "chime_only" ? "Chime only" : a.audio.media;
}

function form(a, save, cancel) {
  const media = a && typeof a.audio === "object" ? a.audio.media : null;
  const name = h("input", { value: a?.name ?? "", required: true, maxLength: 64 });
  const audio = select([["chime_only", "Chime only"], { group: "Media files", options: mediaOptions("media:", media) }],
    media ? `media:${media}` : "chime_only");
  const chime = chimeSelect(a ? a.chime : "default", "Default (rising)");
  const endChime = chimeSelect(a?.end_chime, "Default (falling)");
  const mode = select([["", "Server default"], ["voice", "Voice"], ["music", "Music"], ["none", "None"]], a?.mode ?? "");
  const volume = numberInput(a?.volume_db, { placeholder: "0" });
  const leadIn = numberInput(a?.lead_in_ms, { min: 0, placeholder: "0" });
  const repeat = numberInput(a?.repeat ?? 1, { min: 1, max: 20, step: 1, required: true });
  const gap = numberInput(a?.repeat_gap_s ?? 2, { min: 0, max: 3600, required: true });
  const priority = select(PRIORITIES, a?.priority ?? "normal");
  const zones = state.config.zones.map((z) => ({
    id: z.id,
    box: h("input", { type: "checkbox", checked: !!a?.zones.includes(z.id) }),
    name: z.name,
  }));

  const submit = (e) => {
    e.preventDefault();
    const chosen = zones.filter((z) => z.box.checked).map((z) => z.id);
    // Keep zones this form doesn't know about (none, unless edited by hand meanwhile).
    const unknown = (a?.zones ?? []).filter((id) => !state.config.zones.some((z) => z.id === id));
    save({
      name: name.value.trim(),
      audio: audio.value === "chime_only" ? "chime_only" : { media: audio.value.slice(6) },
      chime: chimeOf(chime.value),
      end_chime: chimeOf(endChime.value),
      mode: mode.value || null,
      volume_db: num(volume),
      lead_in_ms: num(leadIn),
      repeat: num(repeat) ?? 1,
      repeat_gap_s: num(gap) ?? 0,
      zones: [...chosen, ...unknown],
      priority: priority.value,
    });
  };

  return h("form", { onsubmit: submit },
    h("div", { class: "row" }, field("Name", name), field("Audio", audio, "Upload files through mcast serve")),
    h("div", { class: "row" },
      chimeField("Chime before", chime, "Default (rising)", "rising"),
      chimeField("Chime after", endChime, "Default (falling)", "falling")),
    h("p", { class: "small chime-link" },
      h("button", { type: "button", class: "link",
        onclick: () => openChooser(() => { chime.redraw(); endChime.redraw(); }) },
        "Choose which chimes to list…")),
    h("fieldset", {}, h("legend", {}, "Zones"),
      zones.length
        ? h("div", { class: "checks" }, zones.map((z) => h("label", { class: "check" }, z.box, z.name)))
        : h("p", { class: "muted" }, "Add a zone first.")),
    h("div", { class: "row" },
      field("Times to play", repeat), field("Seconds between", gap),
      field("Priority", priority, "Normal skips busy groups. High takes them over except from high or emergency; emergency, except from emergency.")),
    h("details", {}, h("summary", {}, "More"),
      h("div", { class: "row" },
        field("Processing", mode), field("Volume (dB)", volume), field("Lead-in (ms)", leadIn, "Silence first, to wake amplifiers"))),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

export function mount(el) {
  mountEditor(el, {
    kind: "announcement",
    path: "/announcements",
    items: () => state.config.announcements,
    columns: [
      ["Name", (a) => a.name],
      ["Audio", audioLabel, true],
      ["Zones", (a) => a.zones.map(zoneName).join(", ")],
      ["Plays", (a) => (a.repeat > 1 ? `${a.repeat}×` : "once"), true],
      ["Priority", (a) => a.priority ?? "normal", true],
    ],
    form,
  });
}
