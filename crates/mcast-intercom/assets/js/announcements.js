// Announcements: what to play (a media file or a chime), its chimes, where
// it goes, how it plays, and how it sounds. Chimes and sound settings left
// unset use the panel's defaults (the Settings tab).

import { h } from "./dom.js";
import { state, zoneName } from "./store.js";
import { mountEditor, field, select, numberInput, num, mediaOptions } from "./editor.js";
import { chimeField, chimeLabel, chooserLink, effectiveChime } from "./chimes.js";

const PRIORITIES = [["normal", "Normal"], ["high", "High"], ["emergency", "Emergency"]];
export const MODES = [["voice", "Voice"], ["music", "Music"], ["none", "None"]];

export const modeLabel = (m) => (m ? MODES.find(([v]) => v === m)?.[1] ?? m : "Server default");

// Normalize as a select: "" (inherit), "on", "off".
export const normalizeValue = (n) => (n == null ? "" : n ? "on" : "off");
export const normalizeOf = (v) => (v === "" ? null : v === "on");
const INPUT_GAIN_HINT = "Boosts the audio before processing (not the chimes): raise it for a quiet microphone";
const NORMALIZE_HINT = "Levels out quiet and loud audio. Voice and Music processing always do";

function audioLabel(a) {
  if (a.audio === "live") return "Live (push to talk)";
  return a.audio === "chime_only" ? "Chime only" : a.audio.media;
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
  const media = a && typeof a.audio === "object" ? a.audio.media : null;
  const name = h("input", { value: a?.name ?? "", required: true, maxLength: 64 });
  const audio = select([["chime_only", "Chime only"], ["live", "Live: talk from the panel (push to talk)"],
    { group: "Media files", options: mediaOptions("media:", media) }],
    media ? `media:${media}` : a?.audio === "live" ? "live" : "chime_only");
  const chime = chimeField("Before", a?.chime, "chime", { inherit: true });
  const endChime = chimeField("After", a?.end_chime, "end_chime", { inherit: true });
  const mode = select([["", `Default (${modeLabel(d.mode)})`], ...MODES], a?.mode ?? "");
  const volume = numberInput(a?.volume_db, { placeholder: `Default: ${d.volume_db ?? 0}` });
  const inputGain = numberInput(a?.input_gain_db, { min: -40, max: 40, placeholder: `Default: ${d.input_gain_db ?? 0}` });
  const normalize = select([["", `Default (${d.normalize ? "on" : "off"})`], ["on", "On"], ["off", "Off"]],
    normalizeValue(a?.normalize));
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
      audio: audio.value === "chime_only" || audio.value === "live" ? audio.value : { media: audio.value.slice(6) },
      chime: chime.value(),
      end_chime: endChime.value(),
      mode: mode.value || null,
      volume_db: num(volume),
      input_gain_db: num(inputGain),
      normalize: normalizeOf(normalize.value),
      lead_in_ms: num(leadIn),
      repeat: num(repeat) ?? 1,
      repeat_gap_s: num(gap) ?? 0,
      zones: [...chosen, ...unknown],
      priority: priority.value,
    });
  };

  return h("form", { onsubmit: submit },
    part("General",
      h("div", { class: "row" }, field("Name", name), field("Audio", audio, "Upload files on the Sounds tab"))),
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
    part("Playback",
      h("div", { class: "row" },
        field("Times to play", repeat), field("Seconds between", gap),
        field("Priority", priority, "Normal skips busy groups. High takes them over except from high or emergency; emergency, except from emergency."))),
    part("Sound",
      h("div", { class: "row" },
        field("Processing", mode), field("Volume (dB)", volume),
        field("Lead-in (ms)", leadIn, "Silence first, to wake amplifiers")),
      h("div", { class: "row" },
        field("Input gain (dB)", inputGain, INPUT_GAIN_HINT),
        field("Normalize loudness", normalize, NORMALIZE_HINT)),
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
      ["Zones", (a) => a.zones.map(zoneName).join(", ")],
      ["Plays", (a) => (a.repeat > 1 ? `${a.repeat}×` : "once"), true],
      ["Priority", (a) => a.priority ?? "normal", true],
    ],
    form,
  });
}
