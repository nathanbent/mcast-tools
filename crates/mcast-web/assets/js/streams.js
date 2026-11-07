// Streams announced via SAP, with a button to play one here.

import { api } from "./api.js";
import { h, replace, saved, toast } from "./dom.js";
import { state, subscribe, putJob, bookName } from "./store.js";
import { joinChannel } from "./text.js";
import { receive } from "./files.js";

let output = saved.get("output", "");

async function listen(stream) {
  const a = stream.audio;
  try {
    const job = await api("POST", "/receivers", {
      group: a.group, encoding: a.encoding, rate: a.sample_rate, channels: a.channels,
      sink: { type: "play", device: output || null },
    });
    putJob(job);
    toast(`Playing “${stream.name}”`, "ok");
  } catch (e) {
    toast(e.message, "error");
  }
}

function outputPicker() {
  const outputs = state.devices.filter((d) => d.kind === "output");
  return h("label", { class: "field inline" }, h("span", {}, "Play on"),
    h("select", { onchange: (e) => { output = e.target.value; saved.set("output", output); } },
      h("option", { value: "", selected: !output }, "Default output"),
      outputs.map((d) => h("option", { value: d.name, selected: d.name === output }, d.name))));
}

// What the stream carries, and the button that uses it.
function kind(s) {
  const a = s.audio;
  if (a) {
    // G.722's SDP clock rate is 8000 (RFC 3551), but its audio is 16 kHz.
    const khz = a.encoding.toUpperCase() === "G722" ? 16 : a.sample_rate / 1000;
    return {
      group: a.group,
      format: `${a.encoding} ${khz} kHz ${a.channels === 1 ? "mono" : `${a.channels} ch`}`,
      action: h("button", { disabled: !a.playable, title: a.playable ? "" : "can't decode this stream",
        onclick: () => listen(s) }, "Listen"),
    };
  }
  if (s.text) {
    const t = s.text;
    return {
      group: t.group,
      format: "real-time text",
      action: h("button", { onclick: () => joinChannel({
        group: t.group, t140_pt: t.t140_pt, red_pt: t.red_pt ?? 100, redundancy: t.red_pt != null,
      }) }, "Join"),
    };
  }
  if (s.file) {
    return {
      group: s.file.group,
      format: "files",
      action: h("button", { onclick: () => receive({ group: s.file.group, payload_type: s.file.payload_type }) },
        "Receive"),
    };
  }
  return { group: "–", format: "unknown", action: null };
}

// The address, and its phonebook name if it has one.
export function groupCell(group) {
  const name = bookName(group);
  return h("td", {}, h("span", { class: "mono" }, group), name && h("div", { class: "muted small" }, name));
}

function row(s) {
  const k = kind(s);
  return h("tr", {},
    h("td", {}, h("strong", {}, s.name), h("div", { class: "muted small" }, `from ${s.origin}`)),
    groupCell(k.group),
    h("td", {}, k.format),
    h("td", { class: "actions" }, k.action));
}

function render(el) {
  replace(el,
    outputPicker(),
    state.streams.length
      ? h("table", {}, h("thead", {}, h("tr", {}, ["Name", "Group", "Format", ""].map((c) => h("th", {}, c)))),
          h("tbody", {}, state.streams.map(row)))
      : h("p", { class: "muted" }, "No streams announced. Senders announce with SAP every few seconds."));
}

export function mount(el) {
  const body = h("div");
  el.append(body);
  render(body);
  subscribe((what) => (what === "streams" || what === "devices" || what === "phonebook") && render(body));
}
