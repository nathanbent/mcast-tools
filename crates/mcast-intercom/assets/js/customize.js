// Customizing the Panel (admins): its groups (categories) and the buttons in
// each, in order. Drag buttons between groups and groups by their handle,
// or use the keyboard; make, rename, color, size and delete groups; undo.
// Nothing changes for anyone until Save, which saves it all in one go
// (PUT /panel-layout), and is refused if someone else changed the Panel
// meanwhile rather than undoing their work.

import { api } from "./api.js";
import { h, replace, toast } from "./dom.js";
import { state, refreshConfig } from "./store.js";
import { colorSelect, select, SIZES } from "./editor.js";
import { sortable, dragging } from "./drag.js";

const OTHER = "other";
const MAX_UNDO = 100;

// The layout being edited: { revision, groups: [{ key, id, name, color,
// size, anns }], other: [ids] }, or null when not editing.
let draft = null;
let saved = "";       // the draft as loaded, to tell if anything changed
let undos = [];
let picked = null;    // keyboard: { kind: "tile" | "group", key, before }
let stale = false;    // the Panel changed elsewhere since loading
let busy = false;     // saving
let problem = null;   // why the last save failed
let host = null;
let opts = null;      // { look(), onDone() }
let seq = 0;
let refs = {};

export const editing = () => !!draft;

// ---- the model ----

const snapshot = () => JSON.stringify({ groups: draft.groups, other: draft.other });
const restore = (s) => Object.assign(draft, JSON.parse(s));
const dirty = () => !!draft && snapshot() !== saved;
const annById = (id) => state.config.announcements.find((a) => a.id === id);
const lists = () => [...draft.groups.map((g) => ({ key: g.key, name: g.name.trim() || "Untitled group", anns: g.anns })),
  { key: OTHER, name: "Other", anns: draft.other }];
const listOf = (key) => lists().find((l) => l.key === key);
const whereIs = (id) => {
  const ls = lists();
  const li = ls.findIndex((l) => l.anns.includes(id));
  return { ls, li, list: ls[li], i: ls[li]?.anns.indexOf(id) ?? -1 };
};
const plural = (n, one) => `${n} ${one}${n === 1 ? "" : "s"}`;

// Remember how things were, before a change.
function remember(before = snapshot()) {
  if (undos.at(-1) === before) return;
  undos.push(before);
  if (undos.length > MAX_UNDO) undos.shift();
}

function undo() {
  if (!undos.length) return;
  restore(undos.pop());
  picked = null;
  draw();
  say("Undone");
}

// ---- screen reader messages ----

let live = null;
function say(text) {
  if (!live) {
    live = h("div", { class: "sr-only", "aria-live": "assertive", "aria-atomic": "true" });
    document.body.append(live);
  }
  live.textContent = "";
  // A fresh node, so the same words twice are read twice.
  setTimeout(() => { live.textContent = text; }, 30);
}

const position = (id) => {
  const { list, i } = whereIs(id);
  return `${list.name}, ${i + 1} of ${list.anns.length}`;
};

// ---- starting and stopping ----

export async function begin(el, o) {
  host = el;
  opts = o;
  let l;
  try {
    l = await api("GET", "/panel-layout");
  } catch (e) {
    toast(`Can't customize the Panel: ${e.message}`, "error");
    return;
  }
  load(l);
  window.addEventListener("beforeunload", leaving);
  document.addEventListener("keydown", shortcuts);
  draw();
  refs.title?.focus();
}

function load(l) {
  draft = {
    revision: l.revision,
    groups: l.groups.map((g) => ({ key: g.id, id: g.id, name: g.name, color: g.color ?? "", size: g.size ?? "", anns: g.announcements })),
    other: l.other,
  };
  saved = snapshot();
  undos = [];
  picked = null;
  stale = false;
  problem = null;
}

function finish() {
  draft = null;
  undos = [];
  picked = null;
  window.removeEventListener("beforeunload", leaving);
  document.removeEventListener("keydown", shortcuts);
  opts.onDone();
}

function leaving(e) {
  if (dirty()) { e.preventDefault(); e.returnValue = ""; }
}

function shortcuts(e) {
  if (!draft || host.closest("[hidden]")) return;
  const typing = e.target.closest?.("input, select, textarea");
  if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "z" && !typing) {
    e.preventDefault();
    undo();
  }
}

function cancel() {
  if (dirty() && !confirm("Discard your changes to the Panel?")) return;
  finish();
}

async function startOver() {
  if (dirty() && !confirm("Start over from the Panel as it is now? Your changes here will be lost.")) return;
  try {
    load(await api("GET", "/panel-layout"));
    draw();
    say("Loaded the Panel as it is now");
  } catch (e) {
    toast(`Couldn't load the Panel: ${e.message}`, "error");
  }
}

// The config changed (anyone's edit, maybe ours): if the Panel's groups or
// buttons changed, saving would be refused, so say so now.
export async function configChanged() {
  if (!draft || busy || stale) return;
  const rev = draft.revision;
  try {
    const l = await api("GET", "/panel-layout");
    if (draft && !busy && draft.revision === rev && l.revision !== rev) {
      stale = true;
      drawNotice();
    }
  } catch {}
}

// ---- saving ----

function check() {
  const seen = new Map();
  for (const g of draft.groups) {
    const name = g.name.trim();
    if (!name) return [g, "Every group needs a name"];
    const k = name.toLowerCase();
    if (seen.has(k)) return [g, `Two groups are called "${name}"`];
    seen.set(k, g);
  }
  return null;
}

async function save() {
  if (busy) return;
  const bad = check();
  if (bad) {
    problem = bad[1];
    drawNotice();
    const input = host.querySelector(`[data-focus="n:${CSS.escape(bad[0].key)}"]`);
    input?.setAttribute("aria-invalid", "true");
    input?.focus();
    return;
  }
  busy = true;
  problem = null;
  drawBar();
  try {
    await api("PUT", "/panel-layout", {
      revision: draft.revision,
      groups: draft.groups.map((g) => ({
        id: g.id ?? undefined,
        name: g.name.trim(),
        color: g.color || undefined,
        size: g.size || undefined,
        announcements: g.anns,
      })),
      other: draft.other,
    });
    busy = false;
    toast("Saved the Panel: every screen shows it now", "ok");
    await refreshConfig();
    finish();
  } catch (e) {
    busy = false;
    if (e.status === 409) stale = true;
    problem = e.status === 409 ? null : `Couldn't save: ${e.message}`;
    drawBar();
    drawNotice();
  }
}

// ---- changes ----

function addGroup() {
  remember();
  const names = new Set(draft.groups.map((g) => g.name.trim().toLowerCase()));
  let name = "New group";
  for (let n = 2; names.has(name.toLowerCase()); n++) name = `New group ${n}`;
  const key = `new-${++seq}`;
  draft.groups.push({ key, id: null, name, color: "", size: "", anns: [] });
  draw();
  const input = host.querySelector(`[data-focus="n:${CSS.escape(key)}"]`);
  input?.scrollIntoView({ block: "center", behavior: "smooth" });
  input?.focus();
  input?.select();
  say(`Added ${name}. Type its name, then drag buttons into it.`);
}

function removeGroup(g) {
  const n = g.anns.length;
  const name = g.name.trim() || "this group";
  if (n && !confirm(`Delete "${name}"? Its ${plural(n, "button")} move to Other. (Operator tokens limited to this group won't see them any more.)`)) return;
  remember();
  draft.other.push(...g.anns);
  draft.groups.splice(draft.groups.indexOf(g), 1);
  draw();
  say(`Deleted ${name}${n ? `; its buttons are in Other` : ""}`);
  refs.add?.focus();
}

function moveTile(id, key, index) {
  const before = snapshot();
  const from = whereIs(id);
  if (!from.list) return draw();
  from.list.anns.splice(from.i, 1);
  const to = listOf(key);
  to.anns.splice(Math.max(0, Math.min(index, to.anns.length)), 0, id);
  if (snapshot() !== before) remember(before);
  draw();
  say(`${annById(id)?.name ?? id}: ${position(id)}`);
}

function moveGroup(key, index) {
  const before = snapshot();
  const i = draft.groups.findIndex((g) => g.key === key);
  if (i < 0) return draw();
  const [g] = draft.groups.splice(i, 1);
  draft.groups.splice(Math.max(0, Math.min(index, draft.groups.length)), 0, g);
  if (snapshot() !== before) remember(before);
  draw();
  say(`${g.name}: group ${draft.groups.indexOf(g) + 1} of ${draft.groups.length}`);
}

// ---- the keyboard: pick up, move with the arrows, put down ----

function pick(kind, key, label) {
  picked = { kind, key, before: snapshot() };
  draw();
  say(`Picked up ${label}. ${kind === "tile" ? "Arrow keys move it, Up and Down to another group" : "Up and Down move it"}; Space puts it down, Escape cancels.`);
}

function putDown() {
  const p = picked;
  picked = null;
  if (snapshot() !== p.before) remember(p.before);
  draw();
  say(p.kind === "tile" ? `Put down in ${position(p.key)}` : "Put down");
}

function cancelPick() {
  restore(picked.before);
  picked = null;
  draw();
  say("Cancelled: it's back where it was");
}

function stepTile(id, key) {
  const { ls, li, list, i } = whereIs(id);
  const moveTo = (l, at) => { list.anns.splice(i, 1); l.anns.splice(Math.max(0, Math.min(at, l.anns.length)), 0, id); };
  switch (key) {
    case "ArrowLeft":
      if (i > 0) moveTo(list, i - 1);
      else if (li > 0) moveTo(ls[li - 1], ls[li - 1].anns.length);
      else return;
      break;
    case "ArrowRight":
      if (i < list.anns.length - 1) moveTo(list, i + 1);
      else if (li < ls.length - 1) moveTo(ls[li + 1], 0);
      else return;
      break;
    case "ArrowUp": if (li > 0) moveTo(ls[li - 1], i); else return; break;
    case "ArrowDown": if (li < ls.length - 1) moveTo(ls[li + 1], i); else return; break;
    case "Home": moveTo(list, 0); break;
    case "End": moveTo(list, list.anns.length); break;
    default: return;
  }
  draw();
  say(position(id));
}

function stepGroup(key, dir) {
  const i = draft.groups.findIndex((g) => g.key === key);
  const j = i + dir;
  if (j < 0 || j >= draft.groups.length) return;
  const [g] = draft.groups.splice(i, 1);
  draft.groups.splice(j, 0, g);
  draw();
  say(`Group ${j + 1} of ${draft.groups.length}`);
}

const MOVES = ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"];

function tileKeys(id) {
  return (e) => {
    const mine = picked?.kind === "tile" && picked.key === id;
    if (e.key === " " || e.key === "Enter") {
      e.preventDefault();
      if (mine) putDown(); else pick("tile", id, annById(id)?.name ?? id);
    } else if (mine && e.key === "Escape") {
      e.preventDefault();
      cancelPick();
    } else if (mine && e.key === "Tab") {
      putDown();
    } else if (mine && MOVES.includes(e.key)) {
      e.preventDefault();
      stepTile(id, e.key);
    }
  };
}

function groupKeys(g) {
  return (e) => {
    const mine = picked?.kind === "group" && picked.key === g.key;
    if (e.key === " " || e.key === "Enter") {
      e.preventDefault();
      if (mine) putDown(); else pick("group", g.key, g.name.trim() || "the group");
    } else if (mine && e.key === "Escape") {
      e.preventDefault();
      cancelPick();
    } else if (mine && e.key === "Tab") {
      putDown();
    } else if (mine && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      stepGroup(g.key, e.key === "ArrowUp" ? -1 : 1);
    }
  };
}

// ---- drawing ----

function tile(id, group) {
  const a = annById(id);
  // Deleted meanwhile: saving will say so.
  if (!a) return null;
  const color = a.color ?? (group?.color || null);
  const on = picked?.kind === "tile" && picked.key === id;
  return h("div", {
    class: ["tile", "edit-tile", a.priority ?? "normal", color && `colored color-${color}`, on && "picked"].filter(Boolean).join(" "),
    "data-ann": id,
    role: "listitem",
  },
    h("button", {
      type: "button",
      class: "play",
      "data-focus": `t:${id}`,
      "aria-roledescription": "movable button",
      "aria-describedby": "customize-help",
      "aria-pressed": String(on),
      onkeydown: tileKeys(id),
    },
      h("span", { class: "drag-handle tile-handle", "aria-hidden": "true", title: "Drag to move" }, "⠿"),
      a.icon && h("span", { class: "icon-big", "aria-hidden": "true" }, a.icon),
      h("span", { class: "name" }, a.name),
      a.color && h("span", { class: "meta" }, "Has its own color")));
}

function zone(key, name, anns, group) {
  const look = opts.look();
  const size = group?.size || look.size;
  const tiles = anns.map((id) => tile(id, group)).filter(Boolean);
  return h("div", {
    class: `tiles size-${size} mode-${look.mode} drop-zone${tiles.length ? "" : " empty"}`,
    "data-group": key,
    role: "list",
    "aria-label": `Buttons in ${name}`,
  },
    tiles,
    h("p", { class: "drop-hint" }, "Drag buttons here"));
}

function groupSection(g) {
  const look = opts.look();
  const on = picked?.kind === "group" && picked.key === g.key;
  const label = g.name.trim() || "Untitled group";
  let before = null;
  const name = h("input", {
    class: "group-name",
    value: g.name,
    maxlength: "64",
    "aria-label": "Group name",
    "data-focus": `n:${g.key}`,
    onfocus: () => { before = snapshot(); },
    oninput: (e) => {
      g.name = e.target.value;
      e.target.removeAttribute("aria-invalid");
      if (problem) { problem = null; drawNotice(); }
      drawBar();
    },
    onchange: () => { if (before && before !== snapshot()) remember(before); before = snapshot(); drawBar(); },
    onkeydown: (e) => { if (e.key === "Enter") { e.preventDefault(); e.target.blur(); } },
  });
  const color = colorSelect(g.color, "No color");
  color.setAttribute("aria-label", `Color of ${label}`);
  color.dataset.focus = `c:${g.key}`;
  color.addEventListener("change", () => { remember(); g.color = color.value; draw(); });
  const size = select([["", `Panel's (${look.size})`], ...SIZES], g.size);
  size.setAttribute("aria-label", `Button size in ${label}`);
  size.dataset.focus = `s:${g.key}`;
  size.addEventListener("change", () => { remember(); g.size = size.value; draw(); });
  return h("section", { class: `panel-group edit-group${on ? " picked" : ""}`, "data-key": g.key, "aria-label": label },
    h("div", { class: "edit-group-head" },
      h("button", {
        type: "button",
        class: "drag-handle group-handle",
        "data-focus": `g:${g.key}`,
        "aria-label": `Move ${label}`,
        "aria-roledescription": "movable group",
        "aria-describedby": "customize-help",
        "aria-pressed": String(on),
        title: "Drag to move the group (or press Space, then Up and Down)",
        onkeydown: groupKeys(g),
      }, "⠿"),
      name,
      h("label", { class: "inline" }, h("span", { class: "muted small" }, "Color"), color),
      h("label", { class: "inline" }, h("span", { class: "muted small" }, "Size"), size),
      h("span", { class: "muted small count" }, plural(g.anns.length, "button")),
      h("span", { class: "spacer" }),
      h("button", { type: "button", class: "subtle danger small-button", "data-focus": `d:${g.key}`, onclick: () => removeGroup(g) }, "Delete group")),
    zone(g.key, label, g.anns, g));
}

function otherSection() {
  const name = draft.groups.length ? "Other" : "Buttons";
  return h("section", { class: "panel-group edit-group other", "data-key": OTHER, "aria-label": name },
    h("div", { class: "edit-group-head" },
      h("span", { class: "other-title" }, name),
      h("span", { class: "muted small" }, draft.groups.length ? "Buttons in no group. Shown last." : "Add a group, then drag buttons into it.")),
    zone(OTHER, name, draft.other, null));
}

function drawBar() {
  if (!refs.save) return;
  refs.save.disabled = busy || !dirty();
  refs.save.textContent = busy ? "Saving…" : "Save";
  refs.undo.disabled = busy || !undos.length;
  refs.state.textContent = busy ? "" : dirty() ? "Unsaved changes" : "No changes yet";
}

function drawNotice() {
  if (!refs.notice) return;
  replace(refs.notice,
    stale && h("p", { class: "notice warn", role: "alert" },
      "Someone else changed the Panel's buttons or groups since you started, so saving would undo their work. ",
      h("button", { type: "button", class: "link", onclick: startOver }, "Start over with their changes")),
    problem && h("p", { class: "notice error", role: "alert" }, problem));
}

function draw() {
  if (!draft || !host) return;
  // Mid-drag the page is the model; the drop redraws.
  if (dragging()) return;
  const focus = document.activeElement?.dataset?.focus;
  // The bar sticks just under the page's header.
  document.documentElement.style.setProperty("--header-h", `${document.querySelector("header")?.offsetHeight ?? 0}px`);
  const narrowed = state.view.categories.length || state.view.tag || (state.me.categories ?? []).length;
  refs = {};
  refs.title = h("h2", { tabindex: "-1", class: "customize-title" }, "Customize the Panel");
  refs.save = h("button", { type: "button", class: "primary", onclick: save }, "Save");
  refs.undo = h("button", { type: "button", onclick: undo, title: "Undo (Ctrl+Z)" }, "Undo");
  refs.add = h("button", { type: "button", onclick: addGroup }, "+ New group");
  refs.state = h("span", { class: "muted small", "aria-live": "polite" });
  refs.notice = h("div", { class: "customize-notices" });
  const groups = h("div", { class: "edit-groups" }, draft.groups.map(groupSection), otherSection());
  replace(host,
    h("div", { class: "customize-bar" },
      h("div", { class: "customize-head" },
        refs.title,
        h("p", { id: "customize-help", class: "muted small" },
          "Drag buttons between groups (on a touch screen, press and hold first), and groups by their ⠿ handle. ",
          "With a keyboard: Space picks one up, the arrow keys move it, Space puts it down, Escape cancels. ",
          "Nobody else sees the changes until you save.",
          narrowed ? " Every button is shown here, whatever this screen is narrowed to." : "")),
      h("div", { class: "customize-actions" },
        refs.add, refs.undo, h("span", { class: "spacer" }), refs.state,
        h("button", { type: "button", onclick: cancel }, "Cancel"), refs.save),
      refs.notice),
    groups);
  drawBar();
  drawNotice();
  sortable(groups, {
    item: ".edit-tile",
    containers: ".drop-zone",
    // Over a group's heading: into that group.
    near: (el) => el?.closest(".edit-group")?.querySelector(".drop-zone"),
    onDrop: (el, c, i) => moveTile(el.dataset.ann, c.dataset.group, i),
    onCancel: draw,
  });
  sortable(groups, {
    item: ".edit-group:not(.other)",
    containers: ".edit-groups",
    handle: ".group-handle",
    axis: "y",
    onDrop: (el, c, i) => moveGroup(el.dataset.key, i),
    onCancel: draw,
  });
  if (focus) host.querySelector(`[data-focus="${CSS.escape(focus)}"]`)?.focus({ preventScroll: false });
}
