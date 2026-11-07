// Schedules: announcements that play by themselves, weekly or once, in the
// panel's time zone; what's coming up; pausing them all; and days off.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, subscribe, announcementName, fireTime, fromNow, atTime } from "./store.js";
import { mountEditor, field, select } from "./editor.js";
import { putSettings } from "./settings.js";

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

// Where the times are, and pausing them all.
function status() {
  const paused = state.config.schedules_paused;
  return h("div", { class: paused ? "banner warn" : "tz" },
    paused
      ? h("span", {}, h("strong", {}, "All schedules are paused."), " Nothing plays by itself until they're resumed.")
      : h("span", {}, "Times are in ", h("strong", {}, state.upcoming.time_zone ?? "…"),
        state.config.time_zone ? "" : " (the server's)", " · ",
        h("a", { href: "#settings" }, "change")),
    h("span", { class: "spacer" }),
    h("button", {
      type: "button", class: paused ? "primary" : null,
      onclick: () => putSettings({ schedules_paused: !paused },
        paused ? "Schedules resumed" : "Schedules paused"),
    }, paused ? "Resume schedules" : "Pause all"));
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

// "Mon 21 Dec", or "Mon 21 Dec – Fri 1 Jan 2027" when it isn't this year.
function day(iso) {
  const [y, m, d] = iso.split("-").map(Number);
  const opts = { weekday: "short", day: "numeric", month: "short", timeZone: "UTC" };
  if (y !== new Date().getFullYear()) opts.year = "numeric";
  return new Date(Date.UTC(y, m - 1, d)).toLocaleDateString(undefined, opts);
}

const today = () => {
  const n = new Date();
  return `${n.getFullYear()}-${String(n.getMonth() + 1).padStart(2, "0")}-${String(n.getDate()).padStart(2, "0")}`;
};

// Days off: dates no schedule plays on. Kept as one list in the settings.
function daysOff(el) {
  const list = h("div");
  const name = h("input", { maxLength: 64, placeholder: "e.g. Winter break" });
  const from = h("input", { type: "date", required: true });
  const to = h("input", { type: "date" });
  from.onchange = () => { to.min = from.value; };

  const saveAll = (days, done) => putSettings({ days_off: days }, done);
  const all = () => [...(state.config.days_off ?? [])];

  const add = async (e) => {
    e.preventDefault();
    const entry = { from: from.value };
    if (name.value.trim()) entry.name = name.value.trim();
    if (to.value && to.value !== from.value) entry.to = to.value;
    const days = [...all(), entry].sort((a, b) => a.from.localeCompare(b.from));
    if (await saveAll(days, "Day off added")) { name.value = ""; from.value = ""; to.value = ""; }
  };

  const render = () => {
    const days = all();
    const past = (d) => (d.to ?? d.from) < today();
    replace(list,
      days.length
        ? h("div", { class: "scroll" }, h("table", {},
            h("thead", {}, h("tr", {}, h("th", {}, "Dates"), h("th", {}, "Name"), h("th"))),
            h("tbody", {}, days.map((d, i) => h("tr", { class: past(d) ? "off" : null },
              h("td", {}, d.to && d.to !== d.from ? `${day(d.from)} – ${day(d.to)}` : day(d.from)),
              h("td", {}, d.name || ""),
              h("td", { class: "actions" }, h("button", {
                class: "danger",
                onclick: () => saveAll(days.filter((_, j) => j !== i), "Day off removed"),
              }, "Remove")))))))
        : h("p", { class: "muted" }, "None yet."),
      days.some(past) && h("p", {}, h("button", {
        type: "button", class: "link",
        onclick: () => saveAll(days.filter((d) => !past(d)), "Past days off cleared"),
      }, "Clear past days off")));
  };
  subscribe(["config"], render);
  render();
  el.replaceChildren(
    h("p", { class: "muted" }, "No schedule plays on these dates (holidays, breaks). Playing from the panel still works."),
    list,
    h("form", { class: "editor-form", onsubmit: add },
      h("div", { class: "row" }, field("From", from), field("To", to, "Leave empty for one day"), field("Name", name)),
      h("div", { class: "buttons" }, h("button", { type: "submit", class: "primary" }, "Add day off"))));
}

export function mount(el, daysOffEl) {
  daysOff(daysOffEl);
  mountEditor(el, {
    kind: "schedule",
    path: "/schedules",
    items: () => state.config.schedules ?? [],
    label,
    parts: ["upcoming"],
    before: () => [status(), ...(comingUp() ?? [])],
    rowClass: (s) => (s.enabled && !state.config.schedules_paused && nextAt(s) !== null ? "" : "off"),
    columns: [
      ["Plays", label],
      ["When", (s) => describeWhen(s.when)],
      ["Next", (s) => {
        const at = nextAt(s);
        if (at === null) return "passed";
        if (at === undefined) return "";
        if (!s.enabled) return "off";
        return state.config.schedules_paused ? "paused" : atTime(at);
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
