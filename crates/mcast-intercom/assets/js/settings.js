// Settings: the panel-wide ones, a section each. Announcement defaults
// (chimes and sound, which announcements use unless they set their own),
// the time zone schedules are in, which chimes the lists show, and the
// connection to the mcast server.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe } from "./store.js";
import { field, select, numberInput, num } from "./editor.js";
import { chimeField, chooserLink, openChooser } from "./chimes.js";
import { MODES } from "./announcements.js";

export async function putSettings(body, done) {
  try {
    await api("PUT", "/settings", body);
    if (done) toast(done, "ok");
    return true;
  } catch (e) {
    toast(`Couldn't save: ${e.message}`, "error");
    return false;
  }
}

const card = (title, ...children) => h("section", {}, h("h2", {}, title), ...children);

// How many announcements leave a setting to the default.
function usage(key) {
  const anns = state.config.announcements;
  const n = anns.filter((a) => a[key] == null).length;
  return anns.length ? `Used by ${n} of ${anns.length} announcement${anns.length > 1 ? "s" : ""}` : null;
}

// The defaults form. Made afresh when the config changes, unless it has
// unsaved changes.
function defaults() {
  const el = h("div");
  let dirty = false;
  const draw = () => {
    dirty = false;
    const d = state.config.defaults ?? {};
    const chime = chimeField("Chime before", d.chime, "chime", { hint: usage("chime") });
    const endChime = chimeField("Chime after", d.end_chime, "end_chime", { hint: usage("end_chime") });
    const mode = select([["", "Server default"], ...MODES], d.mode ?? "");
    const volume = numberInput(d.volume_db, { placeholder: "0" });
    const leadIn = numberInput(d.lead_in_ms, { min: 0, placeholder: "0" });
    const submit = async (e) => {
      e.preventDefault();
      const body = {
        chime: chime.value(), end_chime: endChime.value(), mode: mode.value || null,
        volume_db: num(volume), lead_in_ms: num(leadIn),
      };
      for (const k of Object.keys(body)) if (body[k] == null) delete body[k];
      if (await putSettings({ defaults: body }, "Defaults saved")) dirty = false;
    };
    replace(el,
      h("p", { class: "muted" },
        "Announcements use these unless they set their own. To have no chime on one announcement while a default chime is set, choose None in it."),
      h("form", { oninput: () => { dirty = true; }, onchange: () => { dirty = true; }, onsubmit: submit },
        h("fieldset", { class: "part" }, h("legend", {}, "Chimes"),
          h("div", { class: "row" }, chime.el, endChime.el),
          h("p", { class: "small" }, chooserLink([chime, endChime]))),
        h("fieldset", { class: "part" }, h("legend", {}, "Sound"),
          h("div", { class: "row" },
            field("Processing", mode, usage("mode")),
            field("Volume (dB)", volume, usage("volume_db")),
            field("Lead-in (ms)", leadIn, "Silence first, to wake amplifiers"))),
        h("div", { class: "buttons" },
          h("button", { type: "submit", class: "primary" }, "Save defaults"),
          h("button", { type: "button", onclick: draw }, "Undo changes"))));
  };
  draw();
  subscribe(["config", "server"], () => { if (!dirty) draw(); });
  return el;
}

// The time zone, with a way to change it.
function timeZone() {
  const el = h("div");
  let editing = false;
  const show = () => {
    if (editing) return;
    const input = h("input", {
      value: state.config.time_zone ?? "", list: "time-zones",
      placeholder: "Empty: the server's", oninput: () => { editing = true; },
    });
    replace(el,
      h("p", { class: "muted" }, "Schedules' times are wall-clock times here. Now: ",
        h("strong", {}, state.upcoming.time_zone ?? "…"), state.config.time_zone ? "" : " (the server's)."),
      h("form", {
        class: "inline-form",
        onsubmit: async (e) => {
          e.preventDefault();
          if (await putSettings({ time_zone: input.value.trim() || null }, "Time zone saved")) {
            editing = false;
            show();
          }
        },
      }, field("Time zone", input, "An IANA name, e.g. Europe/London"),
        h("button", { type: "submit", class: "primary" }, "Save")));
  };
  show();
  subscribe(["config", "upcoming"], show);
  return el;
}

function chimes() {
  const el = h("div");
  const show = () => {
    const hidden = state.chimes.filter((c) => c.hidden).length;
    replace(el,
      h("p", { class: "muted" },
        state.chimes.length
          ? `The mcast server has ${state.chimes.length} built-in chimes; the lists show ${state.chimes.length - hidden}. `
          : "The mcast server didn't list its chimes. ",
        "Hiding one only takes it out of the lists (here and in mcast serve's own pages); announcements that use it still play it."),
      h("div", { class: "buttons" },
        h("button", { type: "button", onclick: () => openChooser(show) }, "Choose which chimes to list…")));
  };
  show();
  subscribe(["server"], show);
  return el;
}

function connection() {
  const el = h("div");
  let version = null;
  api("GET", "/health").then((v) => { version = v.version; show(); }).catch(() => {});
  const show = () => {
    const b = state.backend;
    replace(el, h("dl", { class: "facts" },
      h("dt", {}, "mcast server"), h("dd", {}, b ? b.url : "…"),
      h("dt", {}, "Status"), h("dd", {}, !b ? "…" : b.connected
        ? h("span", { class: "badge done" }, "connected")
        : [h("span", { class: "badge failed" }, "unreachable"), " ", h("span", { class: "muted small" }, b.error ?? "")]),
      h("dt", {}, "Media files"), h("dd", {}, String(state.media.length)),
      h("dt", {}, "Phonebook names"), h("dd", {}, String(state.phonebook.length)),
      h("dt", {}, "Panel version"), h("dd", {}, version ?? "…")),
    h("p", { class: "muted small" },
      "The panel's address, token and config file are set when it starts (mcast intercom serve --help)."));
  };
  show();
  subscribe(["server"], show);
  return el;
}

export function mount(el) {
  // Names for the time zone field, where the browser knows them.
  try {
    replace(document.getElementById("time-zones"),
      Intl.supportedValuesOf("timeZone").map((z) => h("option", { value: z })));
  } catch {}
  el.replaceChildren(
    card("Announcement defaults", defaults()),
    card("Time zone", timeZone()),
    card("Chime lists", chimes()),
    card("Connection", connection()));
}
