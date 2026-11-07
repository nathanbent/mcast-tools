// The "Page" form: what to send, where, and how it's processed.

import { api, ApiError } from "./api.js";
import { h, replace, saved, toast } from "./dom.js";
import { state, subscribe, putJob } from "./store.js";

const DEFAULTS = {
  group: "239.192.2.1:10000", name: "", source: "file", file: "", tone: 440,
  device: "", encoding: "pcmu", mode: "voice", chime: true, lead_in_ms: 800,
  tail_ms: 300, volume_db: 0, ef: true, loop: false, duration_s: "", ttl: 16,
};

const CODECS = [
  ["pcmu", "G.711 µ-law (phones, US)"], ["pcma", "G.711 A-law (phones, EU)"],
  ["g722", "G.722 wideband"], ["opus", "Opus"], ["l16", "L16 (AES67)"], ["l24", "L24 (AES67)"],
];

const AUDIO = /\.(wav|mp3|flac|ogg|oga|m4a|aac|mp4|alac)$/i;

let form;
let values = { ...DEFAULTS, ...saved.get("page", {}) };

function field(label, input, hint) {
  return h("label", { class: "field" }, h("span", {}, label), input, hint && h("small", {}, hint));
}

function bind(key, props = {}) {
  const type = props.type || "text";
  const input = h("input", { ...props, type, name: key });
  if (type === "checkbox") input.checked = !!values[key];
  else input.value = values[key];
  return input;
}

function select(key, options) {
  const el = h("select", { name: key },
    options.map(([v, label]) => h("option", { value: v, selected: v === values[key] }, label)));
  if (!options.some(([v]) => v === values[key]) && options.length) el.value = options[0][0];
  return el;
}

function read() {
  const v = { ...values };
  for (const el of form.elements) {
    if (!el.name) continue;
    if (el.type === "checkbox") v[el.name] = el.checked;
    else if (el.type === "radio") { if (el.checked) v[el.name] = el.value; }
    else if (el.type === "number") v[el.name] = el.value === "" ? "" : Number(el.value);
    else v[el.name] = el.value.trim();
  }
  return v;
}

function request(v) {
  const source =
    v.source === "file" ? { type: "file", path: v.file, loop: v.loop }
    : v.source === "capture" ? { type: "capture", device: v.device || null }
    : v.source === "pad" ? { type: "pad" }
    : { type: "tone", frequency: v.tone };
  return {
    group: v.group, encoding: v.encoding, mode: v.mode, source,
    name: v.name || null, chime: v.chime ? "default" : null,
    lead_in_ms: v.lead_in_ms || 0, tail_ms: v.tail_ms || 0,
    volume_db: v.volume_db ? v.volume_db : null, tos: v.ef ? 184 : null,
    duration_s: v.duration_s === "" ? null : v.duration_s, ttl: v.ttl || 16,
  };
}

async function submit(e) {
  e.preventDefault();
  values = read();
  saved.set("page", values);
  if (values.source === "file" && !values.file) {
    toast("Upload or pick an audio file first.", "error");
    return;
  }
  const button = form.querySelector("button[type=submit]");
  button.disabled = true;
  button.textContent = "Starting…";
  try {
    putJob(await api("POST", "/senders", request(values)));
    toast(`Paging ${values.group}`, "ok");
  } catch (err) {
    if (err instanceof ApiError && err.status === 409) {
      toast(err.message, "error", [`Stop job ${err.job}`, () => stopJob(err.job)]);
    } else {
      toast(err.message, "error");
    }
  } finally {
    button.disabled = false;
    button.textContent = "Start page";
  }
}

async function stopJob(id) {
  try { putJob(await api("DELETE", `/jobs/${id}`)); } catch (e) { toast(e.message, "error"); }
}

function sourceFields() {
  // Only audio can be paged; pictures and other files are sent under Files.
  const files = state.media.filter((m) => AUDIO.test(m.name)).map((m) => [m.name, m.name]);
  const inputs = state.devices.filter((d) => d.kind === "input").map((d) => [d.name, d.name]);
  switch (values.source) {
    case "file":
      return [
        files.length
          ? field("File", select("file", files))
          : h("p", { class: "muted" }, "No audio files yet; add some under Media."),
        field("", h("span", { class: "check" }, bind("loop", { type: "checkbox" }), " Loop")),
      ];
    case "capture":
      return field("Input", select("device", [["", "Default input"], ...inputs]));
    case "pad":
      return h("p", { class: "muted" }, "A soft, slowly changing chord: something pleasant to test speakers and levels with.");
    default:
      return field("Frequency (Hz)", bind("tone", { type: "number", min: 20, max: 20000 }));
  }
}

// Groups to suggest: announced streams and ones paged before.
function groupOptions() {
  const groups = new Set([
    ...state.streams.map((s) => s.audio?.group).filter(Boolean),
    ...[...state.jobs.values()].filter((j) => j.kind === "send").map((j) => j.group).filter(Boolean),
  ]);
  return [...groups].map((g) => h("option", { value: g }));
}

function render() {
  values = form ? read() : values;
  const radio = (v, label) => h("label", { class: "check" },
    h("input", { type: "radio", name: "source", value: v, checked: values.source === v,
      onchange: render }), ` ${label}`);
  replace(form,
    h("div", { class: "row" },
      field("Group", bind("group", { list: "known-groups", required: true, placeholder: "239.192.2.1:10000" })),
      field("Name", bind("name", { placeholder: "announced via SAP" }))),
    h("datalist", { id: "known-groups" }, groupOptions()),
    h("fieldset", {}, h("legend", {}, "Source"),
      h("div", { class: "radios" }, radio("file", "Audio file"), radio("capture", "Live input"), radio("tone", "Test tone"), radio("pad", "Chord pad")),
      sourceFields()),
    h("div", { class: "row" },
      field("Codec", select("encoding", CODECS)),
      field("Processing", select("mode", [["voice", "Voice"], ["music", "Music"], ["none", "None"]]))),
    h("div", { class: "row" },
      field("", h("span", { class: "check" }, bind("chime", { type: "checkbox" }), " Chime first")),
      field("", h("span", { class: "check" }, bind("ef", { type: "checkbox" }), " Voice QoS (DSCP EF)"))),
    h("details", {}, h("summary", {}, "More options"),
      h("div", { class: "row" },
        field("Lead-in (ms)", bind("lead_in_ms", { type: "number", min: 0, step: 100 })),
        field("Tail (ms)", bind("tail_ms", { type: "number", min: 0, step: 100 }))),
      h("div", { class: "row" },
        field("Volume (dB)", bind("volume_db", { type: "number", step: 1, min: -40, max: 20 })),
        field("Stop after (s)", bind("duration_s", { type: "number", min: 0, step: 1, placeholder: "end of source" }))),
      h("div", { class: "row" }, field("TTL", bind("ttl", { type: "number", min: 1, max: 255 })))),
    h("button", { type: "submit", class: "primary" }, "Start page"));
}

// Pick a media file from elsewhere (the Media panel's "Page this").
export function useFile(name) {
  values = { ...read(), source: "file", file: name };
  render();
  form.scrollIntoView({ behavior: "smooth", block: "start" });
}

export function mount(el) {
  form = h("form", { onsubmit: submit, autocomplete: "off" });
  el.append(form);
  render();
  // Re-render only for what the form shows (jobs change every second), and
  // leave the inputs alone when only the suggestions changed.
  subscribe((what) => {
    if (what === "media" || what === "devices") render();
    if (what === "streams") replace(form.querySelector("#known-groups"), groupOptions());
  });
}
