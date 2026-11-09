// Settings: the panel-wide ones, a section each. Announcement defaults
// (chimes and sound, which announcements use unless they set their own),
// the time zone schedules are in, which chimes the lists show, and the
// connection to the mcast server.

import { api, download, postFile } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe } from "./store.js";
import { field, select, numberInput, num, mountEditor, SIZES } from "./editor.js";
import { chimeField, chooserLink, openChooser } from "./chimes.js";
import { MODES } from "./announcements.js";
import { ENCODINGS, encodingValue } from "./zones.js";
import * as tokens from "./tokens.js";
import * as webhooks from "./webhooks.js";
import * as inputs from "./inputs.js";
import { arrangeable } from "./layout.js";
import { LOOK_DEFAULTS } from "./panel.js";

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
    const inputGain = numberInput(d.input_gain_db, { min: -40, max: 40, placeholder: "0" });
    const normalize = select([["off", "Off"], ["on", "On"]], d.normalize ? "on" : "off");
    const leadIn = numberInput(d.lead_in_ms, { min: 0, placeholder: "0" });
    const encoding = select([["", "Server default (L16)"], ...ENCODINGS], encodingValue(d.encoding));
    const groups = state.config.zones.flatMap((z) => z.targets);
    const ownCodec = groups.filter((t) => t.encoding).length;
    const submit = async (e) => {
      e.preventDefault();
      const body = {
        chime: chime.value(), end_chime: endChime.value(), mode: mode.value || null,
        volume_db: num(volume), input_gain_db: num(inputGain),
        normalize: normalize.value === "on" ? true : null,
        lead_in_ms: num(leadIn), encoding: encoding.value || null,
      };
      for (const k of Object.keys(body)) if (body[k] == null) delete body[k];
      if (await putSettings({ defaults: body }, "Defaults saved")) dirty = false;
    };
    replace(el,
      h("p", { class: "muted" },
        "Announcements (and zone groups, for the codec) use these unless they set their own. To have no chime on one announcement while a default chime is set, choose None in it."),
      h("form", { oninput: () => { dirty = true; }, onchange: () => { dirty = true; }, onsubmit: submit },
        h("fieldset", { class: "part" }, h("legend", {}, "Chimes"),
          h("div", { class: "row" }, chime.el, endChime.el),
          h("p", { class: "small" }, chooserLink([chime, endChime]))),
        h("fieldset", { class: "part" }, h("legend", {}, "Sound"),
          h("div", { class: "row" },
            field("Processing", mode, usage("mode")),
            field("Volume (dB)", volume, usage("volume_db")),
            field("Lead-in (ms)", leadIn, "Silence first, to wake amplifiers")),
          h("div", { class: "row" },
            field("Input gain (dB)", inputGain, usage("input_gain_db")),
            field("Normalize loudness", normalize, usage("normalize")))),
        h("fieldset", { class: "part" }, h("legend", {}, "Codec"),
          h("div", { class: "row" },
            field("Codec", encoding, groups.length
              ? `Used by ${groups.length - ownCodec} of ${groups.length} zone group${groups.length > 1 ? "s" : ""}; a group can choose its own in Zones`
              : "For zone groups that don't choose their own"))),
        h("div", { class: "buttons" },
          h("button", { type: "submit", class: "primary" }, "Save defaults"),
          h("button", { type: "button", onclick: draw }, "Undo changes"))));
  };
  draw();
  subscribe(["config", "server"], () => { if (!dirty) draw(); });
  return el;
}

// How the Panel looks, for every screen.
function panelLook() {
  const el = h("div");
  let dirty = false;
  const draw = () => {
    dirty = false;
    const l = { ...LOOK_DEFAULTS, ...(state.config.panel ?? {}) };
    const size = select(SIZES, l.size);
    const mode = select([["tiles", "Tiles"], ["list", "List: one row each"]], l.mode);
    const box = (on, label) => {
      const b = h("input", { type: "checkbox", checked: on });
      return { b, el: h("label", { class: "check" }, b, ` ${label}`) };
    };
    const details = box(l.show_details, "Zones and repeats under each name");
    const next = box(l.show_next, "\"Next:\" (what's scheduled)");
    const playing = box(l.show_playing, "\"Now playing\"");
    const statsBox = box(l.show_stats, "Stats: plays today and over 7 days, busy or failed groups in the last 24 hours");
    const zonesBox = box(l.show_zones, "Zones: each one quiet, or what's playing on it");
    const confirmE = box(l.confirm_emergency, "Ask before playing an emergency announcement");
    replace(el,
      h("p", { class: "muted" }, "How the Panel looks on every screen. Categories can have their own size and color, and announcements their own color and icon (in their editors)."),
      h("form", {
        oninput: () => { dirty = true; }, onchange: () => { dirty = true; },
        onsubmit: async (e) => {
          e.preventDefault();
          const panel = { size: size.value, mode: mode.value, show_details: details.b.checked, show_next: next.b.checked,
            show_playing: playing.b.checked, show_stats: statsBox.b.checked, show_zones: zonesBox.b.checked,
            confirm_emergency: confirmE.b.checked };
          if (await putSettings({ panel }, "Panel look saved")) dirty = false;
        },
      },
        h("div", { class: "row" }, field("Button size", size), field("Layout", mode)),
        h("div", { class: "checks column" }, statsBox.el, zonesBox.el, details.el, next.el, playing.el, confirmE.el),
        h("div", { class: "buttons" },
          h("button", { type: "submit", class: "primary" }, "Save"),
          h("button", { type: "button", onclick: draw }, "Undo changes"))),
      kioskLink(),
      displayLink());
  };
  draw();
  subscribe(["config"], () => { if (!dirty) draw(); });
  return el;
}

// A link for one screen: kiosk (only the Panel, kept awake), and what it
// shows. What it shows isn't a lock: for that, sign the screen in with an
// operator token limited to categories.
function kioskLink() {
  const cats = state.config.categories ?? [];
  const chosen = new Set();
  const tags = [...new Set(state.config.announcements.flatMap((a) => a.tags ?? []))].sort();
  const kiosk = h("input", { type: "checkbox", checked: true });
  const tag = select([["", "Any"], ...tags.map((t) => [t, t])], "");
  const size = select([["", "The Panel's"], ...SIZES], "");
  const mode = select([["", "The Panel's"], ["tiles", "Tiles"], ["list", "List"]], "");
  const out = h("code", {});
  const url = () => {
    const q = new URLSearchParams();
    if (kiosk.checked) q.set("kiosk", "1");
    if (chosen.size) q.set("categories", [...chosen].join(","));
    if (tag.value) q.set("tag", tag.value);
    if (size.value) q.set("size", size.value);
    if (mode.value) q.set("mode", mode.value);
    const qs = q.toString().replace(/%2C/g, ",");
    return `${location.origin}${location.pathname}${qs ? `?${qs}` : ""}#panel`;
  };
  const update = () => { out.textContent = url(); };
  const box = h("fieldset", { class: "part", oninput: update, onchange: update },
    h("legend", {}, "A link for one screen"),
    h("p", { class: "muted small" },
      "Open this on a wall tablet or kiosk. Kiosk shows only the Panel and keeps the screen awake. Choosing categories or a tag only narrows what that screen shows; to make sure a screen can only play some categories, sign it in with an operator token limited to them (Access tokens)."),
    h("label", { class: "check" }, kiosk, " Kiosk: only the Panel, no tabs"),
    cats.length > 0 && h("div", { class: "checks" }, h("span", { class: "muted small" }, "Categories:"),
      [...cats, { id: "other", name: "Other (none)" }].map((c) => h("label", { class: "check" },
        h("input", { type: "checkbox", onchange: (e) => { if (e.target.checked) chosen.add(c.id); else chosen.delete(c.id); } }), ` ${c.name}`))),
    h("div", { class: "row" }, field("Tag", tag), field("Button size", size), field("Layout", mode)),
    h("div", { class: "kiosk-link" }, out,
      h("button", { type: "button", onclick: async () => {
        try { await navigator.clipboard.writeText(url()); toast("Copied", "ok"); } catch { toast("Select the link and copy it", "info"); }
      } }, "Copy"),
      h("a", { href: "#", class: "link", onclick: (e) => { e.preventDefault(); window.open(url(), "_blank", "noopener"); } }, "Open")));
  update();
  return box;
}

// A link for a sign: the display page, showing the text pages of what
// plays to the chosen zones (all of them if none are chosen).
function displayLink() {
  const chosen = new Set();
  const out = h("code", {});
  const url = () => {
    const q = new URLSearchParams({ display: "1" });
    if (chosen.size) q.set("zones", [...chosen].join(","));
    return `${location.origin}${location.pathname}?${q.toString().replace(/%2C/g, ",")}`;
  };
  const update = () => { out.textContent = url(); };
  const box = h("fieldset", { class: "part", onchange: update },
    h("legend", {}, "Display page"),
    h("p", { class: "muted small" },
      "A full-screen sign for a TV or tablet: it shows the text pages of playlists as they play to its zones, and the time in between. Sign it in with an operator token. Zones can also send text pages to text displays (Zones → Text displays)."),
    h("div", { class: "checks" }, h("span", { class: "muted small" }, "Zones:"),
      state.config.zones.map((z) => h("label", { class: "check" },
        h("input", { type: "checkbox", onchange: (e) => { if (e.target.checked) chosen.add(z.id); else chosen.delete(z.id); } }), ` ${z.name}`))),
    h("div", { class: "kiosk-link" }, out,
      h("button", { type: "button", onclick: async () => {
        try { await navigator.clipboard.writeText(url()); toast("Copied", "ok"); } catch { toast("Select the link and copy it", "info"); }
      } }, "Copy"),
      h("a", { href: "#", class: "link", onclick: (e) => { e.preventDefault(); window.open(url(), "_blank", "noopener"); } }, "Open")));
  update();
  return box;
}

// How long the play history is kept.
function historyDays() {
  const el = h("div");
  const show = () => {
    const days = h("input", { type: "number", min: 0, max: 3650, step: 1, value: String(state.config.history_days ?? 90) });
    replace(el,
      h("p", { class: "muted" }, "Every play is kept on disk (the History tab), in a file a day beside the config. Older days are deleted."),
      h("form", {
        class: "inline-form",
        onsubmit: async (e) => {
          e.preventDefault();
          await putSettings({ history_days: Number(days.value) }, "History setting saved");
        },
      }, field("Days to keep", days, "0 keeps it all"), h("button", { type: "submit", class: "primary" }, "Save")));
  };
  show();
  subscribe(["config"], show);
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

// Sites: places with their own zones and schedules. Their days off and
// pause switches are on the Schedules tab.
function sitesEditor() {
  const el = h("div");
  const count = (list, id) => String((list ?? []).filter((x) => x.site === id).length);
  mountEditor(el, {
    kind: "site",
    path: "/sites",
    items: () => state.config.sites ?? [],
    before: () => h("p", { class: "muted" },
      "Sites are places with their own zones and schedules, like buildings or campuses. A site's schedules play only to its zones, in its time zone, and it has its own days off and pause switch (on the Schedules tab). Without sites, everything is one place."),
    columns: [
      ["Name", (s) => s.name],
      ["Time zone", (s) => s.time_zone ?? h("span", { class: "muted" }, "the panel's"), true],
      ["Zones", (s) => count(state.config.zones, s.id), true],
      ["Schedules", (s) => count(state.config.schedules, s.id), true],
    ],
    form: (site, save, cancel) => {
      const name = h("input", { value: site?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. North campus" });
      const tz = h("input", { value: site?.time_zone ?? "", list: "time-zones", placeholder: "Empty: the panel's" });
      return h("form", {
        onsubmit: (e) => {
          e.preventDefault();
          // Keep its days off and pause switch, which are edited elsewhere.
          save({ ...(site ?? {}), id: undefined, name: name.value.trim(), time_zone: tz.value.trim() || null });
        },
      },
        h("div", { class: "row" }, field("Name", name), field("Time zone", tz, "An IANA name, e.g. America/New_York")),
        h("p", { class: "small muted" }, "Then put zones at the site (Zones tab) and give it schedules (Schedules tab)."),
        h("div", { class: "buttons" },
          h("button", { type: "submit", class: "primary" }, "Save"),
          h("button", { type: "button", onclick: cancel }, "Cancel")));
    },
  });
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
      "The panel's address, startup token and config file are set when it starts (mcast intercom serve --help)."));
  };
  show();
  subscribe(["server"], show);
  return el;
}

// What a restore would change, a line each.
function planLines(p) {
  const LIST = { zones: "Zones", announcements: "Announcements", schedules: "Schedules", playlists: "Playlists",
    categories: "Categories", sites: "Sites", schedule_groups: "Schedule groups", inputs: "Buttons and contacts" };
  const lines = Object.entries(p.lists).map(([k, c]) => [LIST[k] ?? k,
    [["added", c.added], ["changed", c.changed], ["removed", c.removed]]
      .filter(([, n]) => n.length).map(([w, n]) => `${w} ${n.join(", ")}`).join("; ")]);
  if (p.settings.length) lines.push(["Settings", p.settings.map((s) => s.replaceAll("_", " ")).join(", ")]);
  if (p.sounds_in_backup) {
    lines.push(["Sounds", `${p.sounds_in_backup} in the backup; ${p.upload.length ? `${p.upload.length} to upload (${p.upload.join(", ")})` : "all already on the mcast server"}`]);
  }
  return lines;
}

// Download the config (and sounds), or put a backup back after seeing what
// it would change.
function backupRestore() {
  const el = h("div");
  const result = h("div");
  const today = () => new Date().toISOString().slice(0, 10);
  const get = (sounds) => {
    toast(sounds ? "Gathering the sounds…" : "Downloading…", "info");
    download(sounds ? "/backup?sounds=1" : "/backup", `intercom-${today()}.${sounds ? "tar.gz" : "json"}`)
      .catch((e) => toast(`Couldn't download the backup: ${e.message}`, "error"));
  };
  const file = h("input", { type: "file", accept: ".json,.gz,.tgz,application/json,application/gzip",
    "aria-label": "Backup file to restore", onchange: () => preview() });

  const preview = async () => {
    const f = file.files[0];
    if (!f) return;
    replace(result, h("p", { class: "muted" }, `Reading ${f.name}…`));
    let v;
    try {
      v = await postFile("/restore?dry_run=1", f);
    } catch (e) {
      replace(result, h("p", { class: "error" }, `Can't restore ${f.name}: ${e.message}`));
      return;
    }
    const p = v.plan;
    const cancel = () => { file.value = ""; replace(result); };
    const apply = async (btn) => {
      btn.disabled = true;
      btn.textContent = "Restoring…";
      try {
        const done = await postFile("/restore", f);
        const up = done.uploaded.length;
        toast(`Restored ${f.name}${up ? `, with ${up} sound${up > 1 ? "s" : ""}` : ""}. The config before is kept as ${done.kept ?? "nothing (there was none)"}.`, "ok");
        cancel();
      } catch (e) {
        btn.disabled = false;
        btn.textContent = "Restore";
        toast(`Couldn't restore: ${e.message}`, "error");
      }
    };
    replace(result, h("div", { class: "restore-plan" },
      h("h3", {}, `Restoring ${f.name} would…`),
      p.same
        ? h("p", {}, "change nothing: it's the same as now.")
        : h("dl", { class: "facts" }, planLines(p).map(([k, line]) => [h("dt", {}, k), h("dd", {}, line)])),
      p.missing.length > 0 && h("p", { class: "warn" },
        `${p.missing.join(", ")} ${p.missing.length > 1 ? "are" : "is"} used but neither on the mcast server nor in the backup, so ${p.missing.length > 1 ? "they" : "it"} won't play.`),
      v.server_error && h("p", { class: "warn" }, `Couldn't ask the mcast server about its sounds: ${v.server_error}`),
      !p.same && h("p", { class: "muted small" }, "It replaces the whole config: anything made since the backup goes. The config now is kept as intercom.json.bak beside it."),
      h("div", { class: "buttons" },
        !p.same && h("button", { class: "danger", onclick: (e) => apply(e.currentTarget) }, "Restore"),
        h("button", { onclick: cancel }, p.same ? "Close" : "Cancel"))));
  };

  replace(el,
    h("p", { class: "muted small" },
      "A backup holds the config: zones, announcements, schedules, playlists, sites and these settings. Access tokens, webhooks and the play history are never in it."),
    h("div", { class: "buttons" },
      h("button", { onclick: () => get(false) }, "Download the config"),
      h("button", { onclick: () => get(true) }, "Download with the sounds")),
    h("h3", { style: { marginTop: "16px" } }, "Restore"),
    h("p", { class: "muted small" }, "Choose a backup (.json or .tar.gz): you'll see what it changes before anything does."),
    file, result);
  return el;
}

export function mount(el) {
  // Names for the time zone field, where the browser knows them.
  try {
    replace(document.getElementById("time-zones"),
      Intl.supportedValuesOf("timeZone").map((z) => h("option", { value: z })));
  } catch {}
  const sections = [
    ["panel", "Panel", panelLook],
    ["defaults", "Announcement defaults", defaults],
    ["time-zone", "Time zone", timeZone],
    ["sites", "Sites", sitesEditor],
    ["chimes", "Chime lists", chimes],
    ["history", "History", historyDays],
    ["tokens", "Access tokens", tokens.mount],
    ["other-systems", "Other systems", webhooks.mount],
    ["inputs", "Buttons and contacts", inputs.mount],
    ["backup", "Backup and restore", backupRestore],
    ["connection", "Connection", connection],
  ].map(([id, title, make]) => ({ id, title, el: card(title, make()) }));
  el.replaceChildren(arrangeable("settings", sections));
}
