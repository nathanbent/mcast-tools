// Uploaded audio: drop files in, page them, delete them.

import { api, upload } from "./api.js";
import { h, replace, bytes, toast } from "./dom.js";
import { state, subscribe, refresh } from "./store.js";
import { useFile } from "./page.js";

// Server-side names are plain file names without a leading dot.
function safeName(name) {
  return name.replace(/[/\\\u0000-\u001f]/g, "_").replace(/^\.+/, "") || "audio";
}

async function add(files, status) {
  for (const file of files) {
    const name = safeName(file.name);
    if (state.media.some((m) => m.name === name) && !confirm(`Replace “${name}”?`)) continue;
    try {
      await upload(name, file, (p) => replace(status, `Uploading ${name}… ${Math.round(p * 100)} %`));
      toast(`Uploaded ${name}`, "ok");
    } catch (e) {
      toast(e.message, "error");
    }
  }
  replace(status);
  refresh("media");
}

async function remove(name) {
  if (!confirm(`Delete “${name}”?`)) return;
  try {
    await api("DELETE", `/media/${encodeURIComponent(name)}`);
  } catch (e) {
    toast(e.message, "error");
  }
  refresh("media");
}

function render(list) {
  replace(list, state.media.length
    ? h("ul", { class: "media" }, state.media.map((m) => h("li", {},
        h("span", { class: "name" }, m.name),
        h("span", { class: "muted small" }, bytes(m.bytes)),
        h("button", { onclick: () => useFile(m.name) }, "Page this"),
        h("button", { class: "danger subtle", "aria-label": `Delete ${m.name}`, onclick: () => remove(m.name) }, "Delete"))))
    : h("p", { class: "muted" }, "No files yet."));
}

export function mount(el) {
  const status = h("div", { class: "muted small", "aria-live": "polite" });
  const input = h("input", { type: "file", accept: "audio/*,.wav,.mp3,.flac,.ogg,.m4a,.aac", multiple: true,
    hidden: true, onchange: (e) => { add([...e.target.files], status); e.target.value = ""; } });
  const drop = h("div", { class: "drop", tabindex: 0, role: "button",
      onclick: () => input.click(),
      onkeydown: (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), input.click()),
      ondragover: (e) => { e.preventDefault(); drop.classList.add("over"); },
      ondragleave: () => drop.classList.remove("over"),
      ondrop: (e) => { e.preventDefault(); drop.classList.remove("over"); add([...e.dataTransfer.files], status); } },
    "Drop audio files here, or click to choose", h("small", {}, "WAV, MP3, FLAC, Ogg, AAC/M4A"));
  const list = h("div");
  el.append(drop, input, status, list);
  render(list);
  subscribe((what) => what === "media" && render(list));
}
