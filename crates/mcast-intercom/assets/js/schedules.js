// Schedules: announcements that play by themselves, weekly or once, in the
// panel's time zone; plus that time zone, and what's coming up.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, announcementName, fireTime, fromNow, atTime } from "./store.js";
import { mountEditor, field, select } from "./editor.js";

const DAYS = [["mon", "Mon"], ["tue", "Tue"], ["wed", "Wed"], ["thu", "Thu"], ["fri", "Fri"], ["sat", "Sat"], ["sun", "Sun"]];
const WEEKDAYS = DAYS.slice(0, 5).map(([d]) => d);

export function describeWhen(when) {
  if (when.once) return `Once, ${when.once.replace("T", " ")}`;
  const { days, times } = when.weekly;
  const on = (d) => days.includes(d);
  let which;
  if (DAYS.every(([d]) => on(d))) which = "Every day";
  else if (WEEKDAYS.every(on) && !on("sat") && !on("sun")) which = "Mon–Fri";
  else which = DAYS.filter(([d]) => on(d)).map(([, l]) => l).join(", ");
  return `${which} at ${times.join(", ")}`;
}

const label = (s) => s.name || announcementName(s.announcement);

// When it next plays (ms), null if never again, undefined if not known yet.
const nextAt = (s) => state.upcoming.next?.[s.id];

async function setEnabled(s, enabled) {
  try {
    await api("PUT", `/schedules/${encodeURIComponent(s.id)}`, { ...s, enabled });
  } catch (e) {
    toast(`Couldn't change "${label(s)}": ${e.message}`, "error");
  }
}

function form(s, save, cancel) {
  const weekly = s?.when.weekly ?? { days: WEEKDAYS, times: [] };
  const name = h("input", { value: s?.name ?? "", maxLength: 64, placeholder: "Optional" });
  const announcement = select(
    state.config.announcements.map((a) => [a.id, a.name]),
    s?.announcement ?? state.config.announcements[0]?.id ?? "");
  const kind = select([["weekly", "Every week"], ["once", "Once"]], s?.when.once ? "once" : "weekly");
  const days = DAYS.map(([d, l]) => ({ d, box: h("input", { type: "checkbox", checked: weekly.days.includes(d) }), l }));
  const setDays = (which) => { for (const x of days) x.box.checked = which.includes(x.d); };
  const times = h("input", {
    value: weekly.times.join(", "),
    placeholder: "08:30, 12:00, 15:15",
    pattern: "\\s*\\d{2}:\\d{2}(:\\d{2})?\\s*(,\\s*\\d{2}:\\d{2}(:\\d{2})?\\s*)*",
    title: "24-hour times, separated by commas",
  });
  const once = h("input", { type: "datetime-local", value: s?.when.once?.slice(0, 16) ?? "" });
  const enabled = h("input", { type: "checkbox", checked: s ? s.enabled : true });

  const weeklyPart = h("div", { class: "row" },
    h("fieldset", {}, h("legend", {}, "Days"),
      h("div", { class: "days" },
        days.map((x) => h("label", { class: "check" }, x.box, x.l)),
        h("button", { type: "button", class: "link", onclick: () => setDays(WEEKDAYS) }, "weekdays"),
        h("button", { type: "button", class: "link", onclick: () => setDays(DAYS.map(([d]) => d)) }, "every day"))),
    field("Times", times, "24-hour, separated by commas"));
  const oncePart = field("Date and time", once);
  const showKind = () => {
    weeklyPart.hidden = kind.value !== "weekly";
    oncePart.hidden = kind.value !== "once";
    times.required = kind.value === "weekly";
    once.required = kind.value === "once";
  };
  kind.onchange = showKind;
  showKind();

  const submit = (e) => {
    e.preventDefault();
    const when = kind.value === "once"
      ? { once: once.value }
      : {
          weekly: {
            days: days.filter((x) => x.box.checked).map((x) => x.d),
            times: times.value.split(",").map((t) => t.trim()).filter(Boolean),
          },
        };
    save({ name: name.value.trim(), announcement: announcement.value, enabled: enabled.checked, when });
  };

  if (!state.config.announcements.length) {
    return h("p", { class: "muted" }, "Add an announcement first.");
  }
  return h("form", { onsubmit: submit },
    h("div", { class: "row" }, field("Plays", announcement), field("Name", name), field("When", kind)),
    weeklyPart,
    oncePart,
    h("label", { class: "check" }, enabled, "On"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

// The time zone line, with a way to change it. Made once, so a refresh
// doesn't throw away what's being typed.
function timeZone() {
  const el = h("div", { class: "tz" });
  let editing = false;
  const show = () => {
    editing = false;
    replace(el,
      h("span", {}, "Times are in ", h("strong", {}, state.upcoming.time_zone ?? "…"),
        state.config.time_zone ? "" : " (the server's)"),
      h("button", { class: "link", onclick: edit }, "Change"));
  };
  const edit = () => {
    editing = true;
    const input = h("input", { value: state.config.time_zone ?? "", list: "time-zones", placeholder: "e.g. Europe/London (empty: the server's)" });
    replace(el, h("form", {
      onsubmit: async (e) => {
        e.preventDefault();
        try {
          await api("PUT", "/settings", { time_zone: input.value.trim() || null });
          toast("Time zone saved", "ok");
          show();
        } catch (err) {
          toast(`Couldn't change the time zone: ${err.message}`, "error");
        }
      },
    }, input,
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: show }, "Cancel")));
    input.focus();
  };
  show();
  subscribe(["config", "upcoming"], () => { if (!editing) show(); });
  return el;
}

function comingUp() {
  const fires = state.upcoming.fires;
  if (!fires.length) return null;
  return [
    h("h2", {}, "Coming up"),
    h("div", { class: "upcoming" },
      fires.flatMap((f) => [
        h("span", {}, fireTime(f)),
        h("span", {}, f.name),
        h("span", { class: "muted small" }, fromNow(f.at_ms)),
      ])),
    h("h2", {}, "Schedules"),
  ];
}

export function mount(el) {
  // Names for the time zone field, where the browser knows them.
  try {
    replace(document.getElementById("time-zones"),
      Intl.supportedValuesOf("timeZone").map((z) => h("option", { value: z })));
  } catch {}

  const tz = timeZone();
  mountEditor(el, {
    kind: "schedule",
    path: "/schedules",
    items: () => state.config.schedules ?? [],
    label,
    parts: ["upcoming"],
    before: () => [tz, ...(comingUp() ?? [])],
    rowClass: (s) => (s.enabled && nextAt(s) !== null ? "" : "off"),
    columns: [
      ["Plays", label],
      ["When", (s) => describeWhen(s.when)],
      ["Next", (s) => {
        const at = nextAt(s);
        if (at === null) return "passed";
        if (at === undefined) return "";
        return s.enabled ? atTime(at) : "off";
      }, true],
      ["On", (s) => h("input", {
        type: "checkbox",
        checked: s.enabled,
        "aria-label": `${label(s)} on`,
        onchange: (e) => setEnabled(s, e.target.checked),
      })],
    ],
    form,
  });
}
