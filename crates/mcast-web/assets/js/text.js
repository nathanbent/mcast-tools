// Real-time text: join a channel, read along, type into it.

import { api } from "./api.js";
import { h, replace, saved, toast } from "./dom.js";
import { state, subscribe, putJob } from "./store.js";

const DEFAULT_GROUP = "239.69.1.1:5004";
const ACTIVE = new Set(["starting", "running", "stopping"]);

let root;
let current = null; // the channel shown
let join = { group: "", name: "", listen: false, ...saved.get("text", {}) };

const channels = () =>
  [...state.jobs.values()].filter((j) => j.kind === "text" && ACTIVE.has(j.state)).sort((a, b) => b.id - a.id);

// Join (or listen on) a channel. Also used by the streams panel.
export async function joinChannel(request) {
  try {
    const job = await api("POST", "/texts", request);
    putJob(job);
    current = job.id;
    render();
    root.scrollIntoView({ behavior: "smooth", block: "start" });
  } catch (e) {
    toast(e.message, "error");
  }
}

async function leave(id) {
  try { putJob(await api("DELETE", `/jobs/${id}`)); } catch (e) { toast(e.message, "error"); }
}

async function say(id, input) {
  const text = input.value;
  if (!text.trim()) return;
  input.disabled = true;
  try {
    await api("POST", `/jobs/${id}/text`, { text });
    input.value = "";
  } catch (e) {
    toast(e.message, "error");
  } finally {
    input.disabled = false;
    input.focus();
  }
}

function joinForm() {
  const group = h("input", { value: join.group, placeholder: DEFAULT_GROUP });
  const name = h("input", { value: join.name, placeholder: "your name (host name if blank)" });
  const listen = h("input", { type: "checkbox", checked: join.listen });
  return h("form", {
      onsubmit: (e) => {
        e.preventDefault();
        join = { group: group.value.trim(), name: name.value.trim(), listen: listen.checked };
        saved.set("text", join);
        joinChannel({ group: join.group || null, name: join.name || null, send: !join.listen });
      } },
    h("div", { class: "row" },
      h("label", { class: "field" }, h("span", {}, "Group"), group),
      h("label", { class: "field" }, h("span", {}, "Name"), name)),
    h("div", { class: "row" },
      h("label", { class: "check" }, listen, " Listen only"),
      h("button", { type: "submit" }, "Join channel")));
}

function log(lines) {
  if (!lines.length) return h("p", { class: "muted" }, "Nothing said yet.");
  return h("ol", { class: "chat" }, lines.map((l) =>
    h("li", { class: l.own ? "own" : "" },
      h("span", { class: "who" }, l.from),
      h("span", { class: "said" }, l.text),
      h("time", { class: "muted small" }, new Date(l.at_ms).toLocaleTimeString()))));
}

function render() {
  const open = channels();
  if (!open.some((j) => j.id === current)) current = open[0]?.id ?? null;
  const job = state.jobs.get(current);
  if (!job) return replace(root, joinForm());
  const sending = job.request?.send !== false;
  const input = h("input", { placeholder: sending ? "Type a message, Enter sends" : "listening only",
    disabled: !sending, autocomplete: "off", "aria-label": "Message" });
  const picker = open.length > 1 && h("select", { "aria-label": "Channel",
      onchange: (e) => { current = Number(e.target.value); render(); } },
    open.map((j) => h("option", { value: j.id, selected: j.id === current }, `${j.name} · ${j.group}`)));
  const body = log(state.texts.get(job.id) ?? []);
  replace(root,
    h("div", { class: "chat-head" },
      h("div", {}, h("strong", {}, job.name ?? "Text"), h("div", { class: "muted small" }, job.summary)),
      picker,
      h("button", { class: "danger", onclick: () => leave(job.id) }, "Leave")),
    h("div", { class: "chat-log", role: "log", "aria-live": "polite" }, body),
    sending && h("form", { class: "chat-say", onsubmit: (e) => { e.preventDefault(); say(job.id, input); } },
      input, h("button", { type: "submit" }, "Send")),
    h("details", {}, h("summary", {}, "Join another channel"), joinForm()));
  const box = root.querySelector(".chat-log");
  box.scrollTop = box.scrollHeight;
}

export function mount(el) {
  root = el;
  render();
  // Keep a half-typed message when the log redraws.
  subscribe((what) => {
    if (what !== "texts" && what !== "jobs") return;
    const input = root.querySelector(".chat-say input");
    const draft = input?.value ?? "";
    const focused = document.activeElement === input;
    // Jobs update every second; only redraw when the channel list changed.
    if (what === "jobs") {
      const ids = channels().map((j) => j.id).join();
      if (ids === root.dataset.ids) return;
      root.dataset.ids = ids;
    }
    render();
    const again = root.querySelector(".chat-say input");
    if (again) {
      again.value = draft;
      if (focused) again.focus();
    }
  });
}
