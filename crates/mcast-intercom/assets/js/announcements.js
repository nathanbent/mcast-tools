// Announcements: what to play (a sound, text, a chime, someone talking, or
// a playlist) and where it goes. That's all one needs; its chimes, repeats,
// sound and button are folded away, set to the panel's defaults (the
// Settings tab) until changed.

import { h, replace } from "./dom.js";
import { state, zoneName } from "./store.js";
import { mountEditor, field, select, numberInput, num, mediaOptions, mediaFilter, colorSelect, SIZES, emojiPicker } from "./editor.js";
import { chimeField, chimeLabel, chooserLink, effectiveChime } from "./chimes.js";
import { textControls, pageValue, pageHow, loadVoices } from "./playlists.js";

const PRIORITIES = [["normal", "Normal"], ["high", "High"], ["emergency", "Emergency"]];
export const MODES = [["voice", "Voice"], ["music", "Music"], ["none", "None"]];

export const modeLabel = (m) => (m ? MODES.find(([v]) => v === m)?.[1] ?? m : "Server default");

// "Chime only", "lunch.wav", "Playlist: Closing", "Text: Lunch is served".
export function audioName(au) {
  if (au === "live") return "Live (push to talk)";
  if (au === "chime_only") return "Chime only";
  if (au?.playlist) return `Playlist: ${(state.config.playlists ?? []).find((p) => p.id === au.playlist)?.name ?? au.playlist}`;
  if (au?.text) {
    const words = au.text.text.replace(/\s+/g, " ").trim();
    return `Text (${pageHow(au.text)}): ${words.length > 48 ? `${words.slice(0, 47)}…` : words}`;
  }
  return au?.media ?? "?";
}

// Normalize as a select: "" (inherit), "on", "off".
export const normalizeValue = (n) => (n == null ? "" : n ? "on" : "off");
export const normalizeOf = (v) => (v === "" ? null : v === "on");
const INPUT_GAIN_HINT = "Boosts the audio before processing (not the chimes): raise it for a quiet microphone";
const NORMALIZE_HINT = "Levels out quiet and loud audio. Voice and Music processing always do";

function audioLabel(a) {
  const parts = a.parts?.length ? ` + ${a.parts.length} part${a.parts.length === 1 ? "" : "s"}` : "";
  return audioName(a.audio) + parts;
}

// What an announcement (or a part) can play. Each kind shows only its own
// fields, so text needs no sound and a sound needs no playlist.
const KINDS = [
  ["sound", "A sound"],
  ["text", "Text"],
  ["chime_only", "Just a chime"],
  ["live", "Live (push to talk)"],
  ["playlist", "A playlist"],
];
const kindOf = (au) => (au === "live" || au === "chime_only" ? au
  : au?.text ? "text" : au?.playlist ? "playlist" : "sound");
let pickers = 0;

// A choice of what plays: the kind, then that kind's fields. `current` is
// an announcement's `audio` (none for a new one); `live: false` leaves
// push to talk out (parts can't be). `onchange` hears the kind change.
function audioPicker(current, { live = true, what = "the announcement", onchange } = {}) {
  const group = `plays-${++pickers}`;
  let kind = current == null ? (state.media.length ? "sound" : "text") : kindOf(current);
  if (kind === "live" && !live) kind = "chime_only";

  const sound = select([["", "Choose a sound…"], ...mediaOptions("media:", current?.media ?? null)],
    current?.media ? `media:${current.media}` : "");
  sound.required = true;
  const filter = mediaFilter(sound);
  const lists = state.config.playlists ?? [];
  const chosen = current?.playlist;
  const playlist = select([["", "Choose a playlist…"],
    ...lists.map((p) => [p.id, `${p.name} (${p.items.length} item${p.items.length === 1 ? "" : "s"})`]),
    ...(chosen && !lists.some((p) => p.id === chosen) ? [[chosen, `${chosen} (missing)`]] : [])], chosen ?? "");
  playlist.required = true;
  // The text being typed: kept while other kinds are looked at.
  const page = { type: "text", text: "", ...(current?.text ?? {}) };

  const fields = h("div", { class: "plays-fields" });
  const note = (text) => h("p", { class: "small muted" }, text);
  const draw = () => replace(fields,
    kind === "sound" && [
      h("div", { class: "row" }, field("Sound", sound), field("Find one", filter)),
      note("Upload or record sounds on the Sounds tab.")],
    kind === "text" && [
      textControls(page, what, () => {}),
      note("Spoken by the mcast server's text to speech, shown on the zones' displays, or both.")],
    kind === "chime_only" && note("Its chime plays and nothing else: a bell. Choose which under Chimes, below."),
    kind === "live" && note("Someone talks, from the Panel: hold its button to talk, or tap it to talk hands-free."),
    kind === "playlist" && [
      field("Playlist", playlist),
      note(lists.length ? "Several sounds and text pages, one after another. Change them on the Playlists tab."
        : "A playlist is several sounds and text pages, one after another. There are none yet: make one on the Playlists tab.")]);
  draw();
  // The voices, when they arrive: only the voice list changes.
  if (kind === "text") loadVoices(() => { if (kind === "text" && !fields.contains(document.activeElement)) draw(); });

  const radios = KINDS.filter(([k]) => live || k !== "live").map(([k, label]) => h("label", { class: "check" },
    h("input", {
      type: "radio", name: group, value: k, checked: k === kind,
      onchange: () => { kind = k; if (k === "text") loadVoices(draw); else draw(); onchange?.(k); },
    }), label));
  return {
    el: h("div", { class: "plays" },
      h("div", { class: "checks", role: "radiogroup", "aria-label": `What ${what} plays` }, radios), fields),
    kind: () => kind,
    value: () => {
      if (kind === "sound") return { media: sound.value.slice(6) };
      if (kind === "playlist") return { playlist: playlist.value };
      if (kind === "text") {
        const { type: _, ...text } = pageValue(page);
        return { text };
      }
      return kind;
    },
  };
}

// The parts: other audio to other zones, in the same run.
function partsEditor(initial) {
  const parts = [];
  const list = h("div", { class: "parts" });
  const add = (p) => {
    const text = h("input", { value: p?.text ?? "", maxLength: 2000, placeholder: "Optional" });
    const textField = field("Text on its displays", text, "Shown while it plays; nothing speaks it");
    const n = parts.length + 1;
    const audio = audioPicker(p?.audio ?? null, { live: false, what: `part ${n}`, onchange: (k) => { textField.hidden = k === "text"; } });
    textField.hidden = audio.kind() === "text";
    const boxes = state.config.zones.map((z) => ({
      id: z.id, box: h("input", { type: "checkbox", checked: !!p?.zones.includes(z.id) }), name: z.name,
    }));
    const entry = { audio, boxes, text, el: null };
    entry.el = h("div", { class: "part-row" },
      audio.el, textField,
      h("div", { class: "checks" }, h("span", { class: "muted" }, "To:"), boxes.map((b) => h("label", { class: "check" }, b.box, b.name))),
      h("div", {}, h("button", { type: "button", class: "danger", onclick: () => { parts.splice(parts.indexOf(entry), 1); entry.el.remove(); list.dispatchEvent(new Event("change", { bubbles: true })); } }, "Remove part")));
    parts.push(entry);
    list.append(entry.el);
  };
  for (const p of initial) add(p);
  return {
    el: h("div", {}, list,
      h("button", { type: "button", onclick: () => { add(null); list.dispatchEvent(new Event("change", { bubbles: true })); } }, "Add a part")),
    count: () => parts.length,
    value: () => parts.map((p) => ({
      audio: p.audio.value(),
      zones: p.boxes.filter((b) => b.box.checked).map((b) => b.id),
      ...(p.audio.kind() !== "text" && p.text.value.trim() ? { text: p.text.value.trim() } : {}),
    })),
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

// A section of the form that's always open: what nobody can do without.
const part = (legend, ...children) => h("fieldset", { class: "part" }, h("legend", {}, legend), ...children);

// A section that starts folded away, with what it's set to beside its name,
// so the form is short until something in it is wanted.
function more(title, summary, ...children) {
  const now = h("span", { class: "muted small" });
  const el = h("details", { class: "part" }, h("summary", {}, h("span", { class: "part-title" }, title), now), ...children);
  el.refresh = () => { now.textContent = summary(); };
  return el;
}

function form(a, save, cancel) {
  const d = state.config.defaults ?? {};
  const name = h("input", { value: a?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Lunch bell" });
  const cats = state.config.categories ?? [];
  const category = select([["", "None (Other)"], ...cats.map((c) => [c.id, c.name])], a?.category ?? "");
  const allTags = [...new Set(state.config.announcements.flatMap((x) => x.tags ?? []))].sort();
  const tagList = h("datalist", { id: "announcement-tags" }, allTags.map((t) => h("option", { value: t })));
  const tags = h("input", { value: (a?.tags ?? []).join(", "), list: "announcement-tags", placeholder: "e.g. Building A, daily" });
  const color = colorSelect(a?.color, "Its category's");
  const icon = h("input", { value: a?.icon ?? "", maxLength: 8, placeholder: "e.g. 🔔", class: "icon-input" });
  const confirmBox = h("input", { type: "checkbox", checked: !!a?.confirm });
  const text = h("textarea", { rows: 2, maxLength: 2000, placeholder: "e.g. 🚪 Doors open in five minutes" });
  text.value = a?.text ?? "";
  const textEmoji = emojiPicker(text, "Emoji for the text", "Or type any emoji, or a shortcode like :fire: or :bell:.");
  const parts = partsEditor(a?.parts ?? []);
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
  // A new one, where there's only one zone: that zone.
  const only = !a && state.config.zones.length === 1;
  const zones = state.config.zones.map((z) => ({
    id: z.id,
    box: h("input", { type: "checkbox", checked: only || !!a?.zones.includes(z.id) }),
    name: z.name,
  }));
  const setZones = (on) => { for (const z of zones) z.box.checked = on; };

  // The sections folded away, each saying what it's set to.
  const textSection = more("Text on displays",
    () => text.value.trim().replace(/\s+/g, " ").slice(0, 60) || "None",
    h("div", { class: "text-item" },
      field("Shown while it plays", text, "On its zones' displays for as long as it plays: the words of a recording, say. Nothing speaks it."),
      h("div", { class: "row small" }, textEmoji.toggle), textEmoji.row));
  const chimesSection = more("Chimes",
    () => chimesLabel({ audio: audio.value(), chime: chime.value(), end_chime: endChime.value() }),
    h("div", { class: "row" }, chime.el, endChime.el),
    h("p", { class: "small muted" },
      "Panel default uses the chime set in Settings. ", chooserLink([chime, endChime])));
  const playbackSection = more("Repeats and priority",
    () => [(num(repeat) ?? 1) > 1 ? `${repeat.value} times, ${gap.value || 0} s apart` : "Once",
      `${priority.value} priority`].join(", "),
    h("div", { class: "row" },
      field("Times to play", repeat), field("Seconds between", gap),
      field("Priority", priority, "Normal skips busy groups. High takes them over except from high or emergency; emergency, except from emergency.")));
  const soundSection = more("Sound",
    () => [mode.value && `${modeLabel(mode.value)} processing`,
      volume.value.trim() && `volume ${volume.value} dB`,
      inputGain.value.trim() && `input gain ${inputGain.value} dB`,
      normalize.value && `normalize ${normalize.value}`,
      leadIn.value.trim() && `lead-in ${leadIn.value} ms`].filter(Boolean).join(", ") || "The defaults",
    h("div", { class: "row" },
      field("Processing", mode), field("Volume (dB)", volume),
      field("Lead-in (ms)", leadIn, "Silence first, to wake amplifiers")),
    h("div", { class: "row" },
      field("Input gain (dB)", inputGain, INPUT_GAIN_HINT),
      field("Normalize loudness", normalize, NORMALIZE_HINT)),
    h("p", { class: "small muted" }, "Leave a setting empty to use the default from Settings."));
  const panelSection = more("Its button on the Panel",
    () => [category.selectedOptions[0]?.textContent === "None (Other)" ? "Under Other" : `Under ${category.selectedOptions[0]?.textContent}`,
      icon.value.trim(), color.value, tags.value.trim() && `tags: ${tags.value.trim()}`,
      confirmBox.checked && "asks first"].filter(Boolean).join(", "),
    h("div", { class: "row" },
      field("Category", category, cats.length ? "The heading it's under on the Panel" : "Add categories below the list"),
      field("Tags", tags, "Separate them with commas; the Panel can show just one tag's buttons")),
    h("div", { class: "row" },
      field("Color", color, "Its button's color; emergency and high priority keep their red or amber edge"),
      field("Icon", icon, "An emoji on its button, e.g. 🔔 🚨 🚪")),
    h("label", { class: "check" }, confirmBox, " Ask \"are you sure?\" before playing it from the Panel"),
    tagList);
  const partsSection = more("Other audio to other zones",
    () => (parts.count() ? `${parts.count()} part${parts.count() === 1 ? "" : "s"}` : "None"),
    h("p", { class: "small muted" }, "Parts play alongside, in the same play: a message in two languages to two buildings, say. Each zone can be in only one place: above, or one part."),
    parts.el);
  const sections = [textSection, chimesSection, playbackSection, soundSection, panelSection, partsSection];
  const refresh = () => { for (const s of sections) s.refresh(); };

  // What only some kinds have: text plays its own words, and someone
  // talking plays once, to its own zones.
  const byKind = (kind) => {
    const live = kind === "live";
    repeat.disabled = live;
    gap.disabled = live;
    if (live) repeat.value = "1";
    partsSection.hidden = live;
    textSection.hidden = kind === "text";
  };
  const audio = audioPicker(a?.audio ?? null, { onchange: (k) => { byKind(k); refresh(); } });
  byKind(audio.kind());
  refresh();

  const submit = (e) => {
    e.preventDefault();
    const chosen = zones.filter((z) => z.box.checked).map((z) => z.id);
    // Keep zones this form doesn't know about (none, unless edited by hand meanwhile).
    const unknown = (a?.zones ?? []).filter((id) => !state.config.zones.some((z) => z.id === id));
    save({
      name: name.value.trim(),
      audio: audio.value(),
      text: audio.kind() === "text" ? null : text.value.trim() || null,
      parts: audio.kind() === "live" ? [] : parts.value(),
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
      category: category.value || null,
      color: color.value || null,
      icon: icon.value.trim() || null,
      confirm: confirmBox.checked,
      tags: [...new Map(tags.value.split(",").map((t) => t.trim()).filter(Boolean).map((t) => [t.toLowerCase(), t])).values()],
    });
  };

  const el = h("form", { class: "sectioned", onsubmit: submit, oninput: refresh, onchange: refresh },
    field("Name", name),
    part("What it plays", audio.el),
    part("Where",
      zones.length
        ? h("div", { class: "checks" },
            zones.map((z) => h("label", { class: "check" }, z.box, z.name)),
            zones.length > 1 && h("button", { type: "button", class: "link", onclick: () => setZones(true) }, "all"),
            zones.length > 1 && h("button", { type: "button", class: "link", onclick: () => setZones(false) }, "none"))
        : h("p", { class: "muted" }, "Add a zone first, on the Zones tab.")),
    h("p", { class: "small muted more-head" }, "That's all it needs. The rest is set already, and can be left alone:"),
    sections,
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
  // A field that needs fixing in a folded section: unfold it, so the
  // browser can show which.
  el.addEventListener("invalid", (e) => { const s = e.target.closest?.("details"); if (s) s.open = true; }, true);
  return el;
}

export function mount(el) {
  const list = h("div");
  const catsBox = h("div");
  el.replaceChildren(list, h("h2", { style: { marginTop: "28px" } }, "Categories"), catsBox);
  mountEditor(catsBox, {
    kind: "category",
    plural: "categories",
    path: "/categories",
    items: () => state.config.categories ?? [],
    before: () => h("p", { class: "muted" }, "Categories are the headings on the Panel. Put an announcement in one from its editor, or drag it there with Customize on the Panel, which also sets their order."),
    columns: [
      ["Name", (c) => c.name],
      ["Announcements", (c) => String(state.config.announcements.filter((a) => a.category === c.id).length)],
      ["Look", (c) => [c.color && h("span", { class: `swatch color-${c.color}` }), [c.color, c.size].filter(Boolean).join(", ") || h("span", { class: "muted" }, "—")], true],
    ],
    form: (c, save, cancel) => {
      const name = h("input", { value: c?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Daily bells" });
      const color = colorSelect(c?.color, "None");
      const size = select([["", "The Panel's"], ...SIZES], c?.size ?? "");
      return h("form", { onsubmit: (e) => { e.preventDefault(); save({ name: name.value.trim(), color: color.value || null, size: size.value || null }); } },
        h("div", { class: "row" }, field("Name", name),
          field("Button color", color, "Its buttons can choose their own"), field("Button size", size)),
        h("div", { class: "buttons" },
          h("button", { type: "submit", class: "primary" }, "Save"),
          h("button", { type: "button", onclick: cancel }, "Cancel")));
    },
  });
  const cats = () => state.config.categories ?? [];
  mountEditor(list, {
    kind: "announcement",
    path: "/announcements",
    items: () => state.config.announcements,
    // Under their categories, as on the Panel, when there are any.
    sections: (items) => {
      if (!cats().length) return [{ head: null, items }];
      const known = (a) => cats().some((c) => c.id === a.category);
      return [...cats().map((c) => ({ head: c.name, items: items.filter((a) => a.category === c.id) })),
        { head: "Other", items: items.filter((a) => !known(a)) }].filter((g) => g.items.length);
    },
    columns: [
      ["Name", (a) => a.name],
      ["Plays", audioLabel, true],
      ["Chimes", chimesLabel, true],
      ["Zones", (a) => [a.zones, ...(a.parts ?? []).map((p) => p.zones)].flat().map(zoneName).join(", ")],
      ["Plays", (a) => (a.repeat > 1 ? `${a.repeat}×` : "once"), true],
      ["Priority", (a) => a.priority ?? "normal", true],
      ["Tags", (a) => (a.tags ?? []).join(", ") || h("span", { class: "muted" }, "—"), true],
    ],
    form,
  });
}
