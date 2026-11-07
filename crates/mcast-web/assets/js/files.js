// Files and pictures: send media files, receive into the media list.

import { api } from "./api.js";
import { bytes, h, replace, saved, toast } from "./dom.js";
import { state, subscribe, putJob } from "./store.js";

const DEFAULT_GROUP = "239.69.2.1:5004";

let root;
let picked = new Set();
let opts = { group: "", rate: 2000, loop: false, ...saved.get("files", {}) };

// Pick a media file to send (from the Media panel).
export function useFile(name) {
  picked.add(name);
  render();
  root.scrollIntoView({ behavior: "smooth", block: "start" });
}

// Start receiving. Also used by the streams panel.
export async function receive(request) {
  try {
    putJob(await api("POST", "/file-receivers", request));
    toast("Receiving files; they'll appear under Media", "ok");
  } catch (e) {
    toast(e.message, "error");
  }
}

async function send(group, rate, loop, button) {
  const files = state.media.map((m) => m.name).filter((n) => picked.has(n));
  if (!files.length) return toast("Pick at least one file to send.", "error");
  opts = { group, rate, loop };
  saved.set("files", opts);
  button.disabled = true;
  try {
    putJob(await api("POST", "/file-senders", {
      group: group || DEFAULT_GROUP, files, rate_kbps: rate || 2000, loop,
    }));
    toast(`Sending ${files.length} file${files.length === 1 ? "" : "s"}`, "ok");
    picked.clear();
    render();
  } catch (e) {
    toast(e.message, "error");
  } finally {
    button.disabled = false;
  }
}

function render() {
  // Drop picks for files that are gone.
  const names = new Set(state.media.map((m) => m.name));
  picked = new Set([...picked].filter((n) => names.has(n)));
  const group = h("input", { value: opts.group, placeholder: DEFAULT_GROUP, list: "phonebook-groups" });
  const rate = h("input", { type: "number", min: 50, step: 50, value: opts.rate });
  const loop = h("input", { type: "checkbox", checked: opts.loop });
  const button = h("button", { type: "submit", class: "primary" }, "Send files");
  const list = state.media.length
    ? h("ul", { class: "pick" }, state.media.map((m) =>
        h("li", {}, h("label", { class: "check" },
          h("input", { type: "checkbox", checked: picked.has(m.name),
            onchange: (e) => (e.target.checked ? picked.add(m.name) : picked.delete(m.name)) }),
          ` ${m.name} `, h("span", { class: "muted small" }, bytes(m.bytes))))))
    : h("p", { class: "muted" }, "Upload files under Media first.");
  const recvGroup = h("input", { placeholder: DEFAULT_GROUP, "aria-label": "Receive on group", list: "phonebook-groups" });
  replace(root,
    h("form", { onsubmit: (e) => {
        e.preventDefault();
        send(group.value.trim(), Number(rate.value), loop.checked, button);
      } },
      h("fieldset", {}, h("legend", {}, "Files to send"), list),
      h("div", { class: "row" },
        h("label", { class: "field" }, h("span", {}, "Group"), group),
        h("label", { class: "field" }, h("span", {}, "Rate (kbit/s)"), rate)),
      h("label", { class: "check" }, loop, " Keep sending the last file (for late joiners)"),
      button),
    h("form", { class: "inline-form", onsubmit: (e) => {
        e.preventDefault();
        receive({ group: recvGroup.value.trim() || null });
      } },
      recvGroup, h("button", { type: "submit" }, "Receive files")));
}

export function mount(el) {
  root = el;
  render();
  subscribe((what) => {
    if (what === "media") render();
    if (what?.received) {
      const f = what.received;
      toast(`Received ${f.name} (${bytes(f.bytes)})`, "ok");
    }
  });
}
