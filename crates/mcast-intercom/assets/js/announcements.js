// Announcements: what to play (a media file or a chime), its chimes, where
// it goes, how it plays, and how it sounds. Chimes and sound settings left
// unset use the panel's defaults (the Settings tab).

import { h } from "./dom.js";
import { state, zoneName } from "./store.js";
import { mountEditor, field, select, numberInput, num, mediaOptions, mediaFilter } from "./editor.js";
import { chimeField, chimeLabel, chooserLink, effectiveChime } from "./chimes.js";

const PRIORITIES = [["normal", "Normal"], ["high", "High"], ["emergency", "Emergency"]];
export const MODES = [["voice", "Voice"], ["music", "Music"], ["none", "None"]];

export const modeLabel = (m) => (m ? MODES.find(([v]) => v === m)?.[1] ?? m : "Server default");

// "Chime only", "lunch.wav", "Playlist: Closing".
export function audioName(au) {
  if (au === "live") return "Live (push to talk)";
  if (au === "chime_only") return "Chime only";
  if (au?.playlist) return `Playlist: ${(state.config.playlists ?? []).find((p) => p.id === au.playlist)?.name ?? au.playlist}`;
  return au?.media ?? "?";
}

function audioLabel(a) {
  const parts = a.parts?.length ? ` + ${a.parts.length} part${a.parts.length === 1 ? "" : "s"}` : "";
  return audioName(a.audio) + parts;
}

// A select of what can play: chime only, live (unless `live` is false),
// playlists and media files.
function audioSelect(current, { live = true } = {}) {
  const media = current && typeof current === "object" && current.media ? current.media : null;
  const value = media ? `media:${media}`
    : current?.playlist ? `playlist:${current.playlist}`
    : current === "live" && live ? "live" : "chime_only";
  return select([
    ["chime_only", "Chime only"],
    ...(live ? [["live", "Live: talk from the panel (push to talk)"]] : []),
    { group: "Playlists", options: (state.config.playlists ?? []).map((p) => [`playlist:${p.id}`, `${p.name} (${p.items.length} item${p.items.length === 1 ? "" : "s"})`]) },
    { group: "Media files", options: mediaOptions("media:", media) },
  ], value);
}

const audioValue = (v) => v === "chime_only" || v === "live" ? v
  : v.startsWith("playlist:") ? { playlist: v.slice(9) } : { media: v.slice(6) };

// The parts: other audio to other zones, in the same run.
function partsEditor(initial) {
  const parts = [];
  const list = h("div", { class: "parts" });
  const add = (p) => {
    const audio = audioSelect(p?.audio ?? "chime_only", { live: false });
    const boxes = state.config.zones.map((z) => ({
      id: z.id, box: h("input", { type: "checkbox", checked: !!p?.zones.includes(z.id) }), name: z.name,
    }));
    const entry = { audio, boxes, el: null };
    entry.el = h("div", { class: "part-row" },
      h("div", { class: "row" }, h("div", { class: "field" }, field("Plays", audio), mediaFilter(audio)),
        h("button", { type: "button", class: "danger", onclick: () => { parts.splice(parts.indexOf(entry), 1); entry.el.remove(); } }, "Remove part")),
      h("div", { class: "checks" }, boxes.map((b) => h("label", { class: "check" }, b.box, b.name))));
    parts.push(entry);
    list.append(entry.el);
  };
  for (const p of initial) add(p);
  return {
    el: h("div", {}, list,
      h("button", { type: "button", onclick: () => add(null) }, "Add a part")),
    value: () => parts.map((p) => ({ audio: audioValue(p.audio.value), zones: p.boxes.filter((b) => b.box.checked).map((b) => b.id) })),
  };
}

// "Ding dong → Falling", marking chimes that come from the defaults.
function chimesLabel(a) {
  const one = (which) => {
    const c = effectiveChime(a, which);
    if (c == null) return null;
    return chimeLabel(c, which) + (a[which] == null ? " (default)" : "");
  };
  const [before, after] = [one("chime"), one("end_chime")];
  if (!before && !after) return a.audio === "chime_only" ? "Server default" : "None";
  return [before ?? "—", after].filter(Boolean).join(" → ");
}

// A section of the form.
const part = (legend, ...children) => h("fieldset", { class: "part" }, h("legend", {}, legend), ...children);

function form(a, save, cancel) {
  const d = state.config.defaults ?? {};
  const name = h("input", { value: a?.name ?? "", required: true, maxLength: 64 });
  const audio = audioSelect(a?.audio ?? "chime_only");
  const parts = partsEditor(a?.parts ?? []);
  const partsSection = part("Parts",
    h("p", { class: "small muted" }, "Other audio to other zones, in the same play: a message in two languages to two buildings, say. Each zone can be in only one place: here, or one part."),
    parts.el);
  const chime = chimeField("Before", a?.chime, "chime", { inherit: true });
  const endChime = chimeField("After", a?.end_chime, "end_chime", { inherit: true });
  const mode = select([["", `Default (${modeLabel(d.mode)})`], ...MODES], a?.mode ?? "");
  const volume = numberInput(a?.volume_db, { placeholder: `Default: ${d.volume_db ?? 0}` });
  const leadIn = numberInput(a?.lead_in_ms, { min: 0, placeholder: `Default: ${d.lead_in_ms ?? 0}` });
  const repeat = numberInput(a?.repeat ?? 1, { min: 1, max: 20, step: 1, required: true });
  const gap = numberInput(a?.repeat_gap_s ?? 2, { min: 0, max: 3600, required: true });
  const priority = select(PRIORITIES, a?.priority ?? "normal");
  const zones = state.config.zones.map((z) => ({
    id: z.id,
    box: h("input", { type: "checkbox", checked: !!a?.zones.includes(z.id) }),
    name: z.name,
  }));
  const setZones = (on) => { for (const z of zones) z.box.checked = on; };

  const liveOnly = () => {
    const live = audio.value === "live";
    repeat.disabled = live;
    gap.disabled = live;
    if (live) repeat.value = "1";
    partsSection.hidden = live;
  };
  audio.addEventListener("change", liveOnly);
  liveOnly();

  const submit = (e) => {
    e.preventDefault();
    const chosen = zones.filter((z) => z.box.checked).map((z) => z.id);
    // Keep zones this form doesn't know about (none, unless edited by hand meanwhile).
    const unknown = (a?.zones ?? []).filter((id) => !state.config.zones.some((z) => z.id === id));
    save({
      name: name.value.trim(),
      audio: audioValue(audio.value),
      parts: audio.value === "live" ? [] : parts.value(),
      chime: chime.value(),
      end_chime: endChime.value(),
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
    part("General",
      h("div", { class: "row" }, field("Name", name), h("div", { class: "field" }, field("Audio", audio, "Upload or record sounds on the Sounds tab"), mediaFilter(audio)))),
    part("Chimes",
      h("div", { class: "row" }, chime.el, endChime.el),
      h("p", { class: "small muted" },
        "Panel default uses the chime set in Settings. ", chooserLink([chime, endChime]))),
    part("Zones",
      zones.length
        ? h("div", { class: "checks" },
            zones.map((z) => h("label", { class: "check" }, z.box, z.name)),
            zones.length > 1 && h("button", { type: "button", class: "link", onclick: () => setZones(true) }, "all"),
            zones.length > 1 && h("button", { type: "button", class: "link", onclick: () => setZones(false) }, "none"))
        : h("p", { class: "muted" }, "Add a zone first.")),
    partsSection,
    part("Playback",
      h("div", { class: "row" },
        field("Times to play", repeat), field("Seconds between", gap),
        field("Priority", priority, "Normal skips busy groups. High takes them over except from high or emergency; emergency, except from emergency."))),
    part("Sound",
      h("div", { class: "row" },
        field("Processing", mode), field("Volume (dB)", volume),
        field("Lead-in (ms)", leadIn, "Silence first, to wake amplifiers")),
      h("p", { class: "small muted" }, "Leave a setting empty to use the default from Settings.")),
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
      ["Chimes", chimesLabel, true],
      ["Zones", (a) => [a.zones, ...(a.parts ?? []).map((p) => p.zones)].flat().map(zoneName).join(", ")],
      ["Plays", (a) => (a.repeat > 1 ? `${a.repeat}×` : "once"), true],
      ["Priority", (a) => a.priority ?? "normal", true],
    ],
    form,
  });
}
