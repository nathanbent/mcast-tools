// Schedules: announcements that play by themselves, weekly or once.
//
// With sites, a bar picks what to look at: all sites at once (a card for
// each, then every schedule), or one site (its time zone, pause switch,
// what's coming up there, its schedules and its own days off). Schedules
// are listed under their groups, each with an on/off switch.

import { api } from "./api.js";
import { h, replace, toast, saved } from "./dom.js";
import { state, subscribe, announcementName, siteName, fireTime, fromNow, atTime } from "./store.js";
import { mountEditor, field, select } from "./editor.js";
import { putSettings } from "./settings.js";

const DAYS = [["mon", "Mon"], ["tue", "Tue"], ["wed", "Wed"], ["thu", "Thu"], ["fri", "Fri"], ["sat", "Sat"], ["sun", "Sun"]];
const WEEKDAYS = DAYS.slice(0, 5).map(([d]) => d);
const ALL = "*"; // the "all sites" view; "" is schedules with no site

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
const sites = () => state.config.sites ?? [];
const groups = () => state.config.schedule_groups ?? [];
const siteOf = (id) => sites().find((s) => s.id === id);
const groupOf = (id) => groups().find((g) => g.id === id);

// Which site is shown: ALL, a site's id, or "" for schedules without one.
let view = saved.get("schedules.site", ALL);
const currentView = () => (!sites().length ? ALL : view === ALL || view === "" || siteOf(view) ? view : ALL);

// When it next plays (ms), null if never again, undefined if not known yet.
const nextAt = (s) => state.upcoming.next?.[s.id];

// Why it can't play, if it can't: it, its group or its site is off, or
// everything's paused.
function offReason(s) {
  if (!s.enabled) return "off";
  if (state.config.schedules_paused) return "all paused";
  if (s.group && groupOf(s.group)?.enabled === false) return "group off";
  if (s.site && siteOf(s.site)?.paused) return "site paused";
  return null;
}
const isOn = (s) => !offReason(s);

async function put(path, body, done) {
  try {
    await api("PUT", path, body);
    if (done) toast(done, "ok");
    return true;
  } catch (e) {
    toast(`Couldn't save: ${e.message}`, "error");
    return false;
  }
}

const setEnabled = (s, enabled) => put(`/schedules/${encodeURIComponent(s.id)}`, { ...s, enabled });
const setGroup = (g, enabled) =>
  put(`/schedule-groups/${encodeURIComponent(g.id)}`, { ...g, enabled }, `${g.name} ${enabled ? "on" : "off"}`);
const setSite = (site, changes, done) => put(`/sites/${encodeURIComponent(site.id)}`, { ...site, ...changes }, done);

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
  // A new one goes to the site being looked at.
  const v = currentView();
  const site = select([["", "No site (all its zones)"], ...sites().map((x) => [x.id, x.name])],
    s ? s.site ?? "" : v === ALL ? "" : v);
  const group = select([["", "No group"], ...groups().map((g) => [g.id, g.name])], s?.group ?? "");

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
    save({
      name: name.value.trim(), announcement: announcement.value, enabled: enabled.checked, when,
      site: site.value || null, group: group.value || null,
    });
  };

  if (!state.config.announcements.length) {
    return h("p", { class: "muted" }, "Add an announcement first.");
  }
  return h("form", { onsubmit: submit },
    h("div", { class: "row" }, field("Plays", announcement), field("Name", name), field("When", kind)),
    weeklyPart,
    oncePart,
    (sites().length > 0 || groups().length > 0) && h("div", { class: "row" },
      sites().length > 0 && field("Site", site, "A site's schedule plays only to the announcement's zones there, in the site's time zone"),
      groups().length > 0 && field("Group", group, "It plays only while its group is on")),
    h("label", { class: "check" }, enabled, "On"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

// ---- the site bar and overview ----

function siteBar(rerender) {
  if (!sites().length) return null;
  const v = currentView();
  const noSite = (state.config.schedules ?? []).some((s) => !s.site);
  const tab = (id, text) => h("button", {
    type: "button", "aria-pressed": String(v === id),
    onclick: () => { view = id; saved.set("schedules.site", id); rerender(); },
  }, text);
  return h("nav", { class: "sitebar", "aria-label": "Sites" },
    tab(ALL, "All sites"), sites().map((s) => tab(s.id, s.name)), noSite && tab("", "No site"));
}

// Times are each in their own site's time zone; `withSite` names the site.
function firesList(fires, limit, withSite = false) {
  const shown = limit ? fires.slice(0, limit) : fires;
  if (!shown.length) return h("p", { class: "muted small" }, "Nothing coming up.");
  return h("div", { class: `upcoming${withSite ? " with-site" : ""}` },
    shown.flatMap((f) => [
      h("span", {}, fireTime(f)),
      h("span", {}, f.name),
      withSite && h("span", { class: "muted small" }, f.site ? siteName(f.site) : "no site"),
      h("span", { class: "muted small" }, fromNow(f.at_ms)),
    ]));
}

// A card per site: where it is in time, whether it's paused, what's next.
function overview(rerender) {
  const card = (id, name, tz, paused, pause) => h("div", { class: `site-card${paused ? " paused" : ""}` },
    h("div", { class: "site-head" },
      h("button", { class: "link name", onclick: () => { view = id; saved.set("schedules.site", id); rerender(); } }, name),
      paused && h("span", { class: "badge waiting" }, "paused")),
    h("p", { class: "muted small" }, tz),
    firesList(state.siteUpcoming[id]?.fires ?? [], 3),
    pause && h("div", { class: "buttons" }, pause));
  const noSite = (state.config.schedules ?? []).some((s) => !s.site);
  return h("div", { class: "site-cards" },
    sites().map((s) => card(s.id, s.name, state.upcoming.sites?.[s.id]?.time_zone ?? "…", s.paused,
      h("button", { type: "button", onclick: () => setSite(s, { paused: !s.paused }, `${s.name} ${s.paused ? "resumed" : "paused"}`) },
        s.paused ? "Resume" : "Pause"))),
    noSite && card("", "No site", `${state.upcoming.time_zone ?? "…"} (the panel's)`, false, null));
}

// Where the times are, and the pause switches that apply here.
function status(v) {
  const all = state.config.schedules_paused;
  const site = v !== ALL && v !== "" ? siteOf(v) : null;
  const tz = site ? state.upcoming.sites?.[site.id]?.time_zone : state.upcoming.time_zone;
  return [
    all && h("div", { class: "banner warn" },
      h("span", {}, h("strong", {}, "All schedules are paused."), " Nothing plays by itself until they're resumed."),
      h("span", { class: "spacer" }),
      h("button", { type: "button", class: "primary", onclick: () => putSettings({ schedules_paused: false }, "Schedules resumed") },
        "Resume all")),
    site?.paused && h("div", { class: "banner warn" },
      h("span", {}, h("strong", {}, `${site.name} is paused.`), " Its schedules don't play until it's resumed."),
      h("span", { class: "spacer" }),
      h("button", { type: "button", class: "primary", onclick: () => setSite(site, { paused: false }, `${site.name} resumed`) },
        "Resume")),
    h("div", { class: "tz" },
      h("span", {}, site ? `Times at ${site.name} are in ` : "Times are in ", h("strong", {}, tz ?? "…"),
        site && !site.time_zone ? " (the panel's)" : "", " · ",
        h("a", { href: "#settings" }, "change")),
      h("span", { class: "spacer" }),
      site && !site.paused && h("button", { type: "button", onclick: () => setSite(site, { paused: true }, `${site.name} paused`) },
        `Pause ${site.name}`),
      !all && h("button", { type: "button", onclick: () => putSettings({ schedules_paused: true }, "Schedules paused") },
        sites().length ? "Pause all sites" : "Pause all")),
  ];
}

// ---- days off: the panel's own, or a site's ----

// "Mon 21 Dec", or "Mon 21 Dec 2027" when it isn't this year.
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

function daysOff(el) {
  const list = h("div");
  const intro = h("p", { class: "muted" });
  const name = h("input", { maxLength: 64, placeholder: "e.g. Winter break" });
  const from = h("input", { type: "date", required: true });
  const to = h("input", { type: "date" });
  from.onchange = () => { to.min = from.value; };

  // The panel's list, or the site being looked at.
  const site = () => { const v = currentView(); return v !== ALL && v !== "" ? siteOf(v) : null; };
  const all = () => [...((site() ? site().days_off : state.config.days_off) ?? [])];
  const saveAll = (days, done) => site()
    ? setSite(site(), { days_off: days }, done)
    : putSettings({ days_off: days }, done);

  const add = async (e) => {
    e.preventDefault();
    const entry = { from: from.value };
    if (name.value.trim()) entry.name = name.value.trim();
    if (to.value && to.value !== from.value) entry.to = to.value;
    const days = [...all(), entry].sort((a, b) => a.from.localeCompare(b.from));
    if (await saveAll(days, "Day off added")) { name.value = ""; from.value = ""; to.value = ""; }
  };

  const render = () => {
    const s = site();
    replace(intro, s
      ? `${s.name}'s own days off, when none of its schedules plays. The panel's days off (on the All sites view) apply here too.`
      : sites().length
        ? "Days off for every site (holidays, breaks): no schedule plays. Each site can add its own. Playing from the panel still works."
        : "No schedule plays on these dates (holidays, breaks). Playing from the panel still works.");
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
  el.replaceChildren(intro, list,
    h("form", { class: "editor-form", onsubmit: add },
      h("div", { class: "row" }, field("From", from), field("To", to, "Leave empty for one day"), field("Name", name)),
      h("div", { class: "buttons" }, h("button", { type: "submit", class: "primary" }, "Add day off"))));
  return render;
}

// ---- schedule groups ----

function groupForm(g, save, cancel) {
  const name = h("input", { value: g?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Exam week" });
  const enabled = h("input", { type: "checkbox", checked: g ? g.enabled : true });
  return h("form", { onsubmit: (e) => { e.preventDefault(); save({ name: name.value.trim(), enabled: enabled.checked }); } },
    field("Name", name),
    h("label", { class: "check" }, enabled, "On"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

const groupSwitch = (g) => h("label", { class: "check switch" },
  h("input", { type: "checkbox", checked: g.enabled, onchange: (e) => setGroup(g, e.target.checked) }),
  g.enabled ? "On" : "Off");

export function mount(el, daysOffEl, groupsEl) {
  const redrawDaysOff = daysOff(daysOffEl);
  let editor = null;
  const rerender = () => { editor?.render(); redrawDaysOff(); };

  editor = mountEditor(el, {
    kind: "schedule",
    path: "/schedules",
    items: () => {
      const v = currentView();
      const all = state.config.schedules ?? [];
      return v === ALL ? all : all.filter((s) => (s.site ?? "") === v);
    },
    label,
    parts: ["upcoming"],
    before: () => {
      const v = currentView();
      const fires = v === ALL ? state.upcoming.fires : state.siteUpcoming[v]?.fires ?? [];
      return [
        siteBar(rerender),
        status(v),
        v === ALL && sites().length > 0 && overview(rerender),
        h("h2", {}, v === ALL && sites().length ? "Coming up everywhere" : "Coming up"),
        firesList(fires, 0, v === ALL && sites().length > 0),
        h("h2", {}, "Schedules"),
      ];
    },
    // Under their groups, each with its switch; the rest after them.
    sections: (items) => {
      if (!groups().length) return [{ head: null, items }];
      const secs = groups().map((g) => ({
        head: h("div", { class: "group-head" }, h("strong", {}, g.name), groupSwitch(g),
          !g.enabled && h("span", { class: "muted small" }, "its schedules don't play")),
        items: items.filter((s) => s.group === g.id),
      }));
      secs.push({ head: h("strong", { class: "muted" }, "Not in a group"), items: items.filter((s) => !groupOf(s.group)) });
      return secs.filter((s) => s.items.length);
    },
    rowClass: (s) => (isOn(s) && nextAt(s) !== null ? "" : "off"),
    columns: () => [
      ["Plays", label],
      ["When", (s) => describeWhen(s.when)],
      ...(currentView() === ALL && sites().length
        ? [["Site", (s) => (s.site ? siteName(s.site) : "—"), true]]
        : []),
      ["Next", (s) => {
        const at = nextAt(s);
        if (at === null) return "passed";
        if (at === undefined) return "";
        return offReason(s) ?? atTime(at, s.site);
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

  mountEditor(groupsEl, {
    kind: "group",
    path: "/schedule-groups",
    items: () => groups(),
    before: () => h("p", { class: "muted" },
      "Groups switch sets of schedules on and off together: \"Normal bells\", \"Exam week\", \"Summer hours\". Put a schedule in a group from its form."),
    columns: [
      ["Name", (g) => g.name],
      ["Schedules", (g) => String((state.config.schedules ?? []).filter((s) => s.group === g.id).length), true],
      ["On", groupSwitch],
    ],
    form: groupForm,
  });
}
