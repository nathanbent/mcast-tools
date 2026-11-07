// Streams announced via SAP, with a button to play one here.

import { api } from "./api.js";
import { h, replace, saved, toast } from "./dom.js";
import { state, subscribe, putJob } from "./store.js";

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

function row(s) {
  const a = s.audio;
  // G.722's SDP clock rate is 8000 (RFC 3551), but its audio is 16 kHz.
  const khz = a && (a.encoding.toUpperCase() === "G722" ? 16 : a.sample_rate / 1000);
  const format = a ? `${a.encoding} ${khz} kHz ${a.channels === 1 ? "mono" : `${a.channels} ch`}` : "no audio";
  return h("tr", {},
    h("td", {}, h("strong", {}, s.name), h("div", { class: "muted small" }, `from ${s.origin}`)),
    h("td", { class: "mono" }, a?.group ?? "–"),
    h("td", {}, format),
    h("td", { class: "actions" },
      h("button", { disabled: !a?.playable, title: a?.playable ? "" : "can't decode this stream",
        onclick: () => listen(s) }, "Listen")));
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
  subscribe((what) => (what === "streams" || what === "devices") && render(body));
}
