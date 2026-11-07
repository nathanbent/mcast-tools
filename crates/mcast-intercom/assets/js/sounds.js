// Sounds: the mcast server's media files, in folders. Upload (or drop
// files), record, listen, tag, search, rename or move (or drag onto a
// folder), hide from the panel's lists, delete; one at a time or many
// selected at once. A file an announcement uses can't be deleted, and
// renaming or moving it updates the announcement.

import { api, mediaUrl, upload } from "./api.js";
import { h, replace, toast, bytes, saved, seconds } from "./dom.js";
import { state, subscribe, refreshLibrary, can } from "./store.js";
import { field, select } from "./editor.js";
import { openRecorder } from "./record.js";

let folder = saved.get("sounds.folder", ""); // the folder being shown; "" is the top
let query = ""; // the search box: words match names and tags; #tag matches a tag
let sort = saved.get("sounds.sort", { key: "name", desc: false });
const selected = new Set(); // names of files and folders
let player = null;

const base = (name) => name.slice(name.lastIndexOf("/") + 1);
const parent = (name) => (name.includes("/") ? name.slice(0, name.lastIndexOf("/")) : "");
const join = (dir, name) => (dir ? `${dir}/${name}` : name);
const enc = (name) => name.split("/").map(encodeURIComponent).join("/");
const isFolder = (name) => state.library.folders.some((f) => f.name === name);

function open(dir) {
  folder = dir;
  query = "";
  search.value = "";
  selected.clear();
  saved.set("sounds.folder", dir);
  render();
}

function listen(name) {
  player?.pause();
  player = new Audio(mediaUrl(name));
  player.play().catch((e) => toast(`Can't play ${name}: ${e.message}`, "error"));
}

async function act(what, fn) {
  try {
    const r = await fn();
    if (what) toast(what, "ok");
    return r ?? true;
  } catch (e) {
    toast(`Couldn't do that: ${e.message}`, "error");
    return false;
  }
}

// ---- one at a time ----

const setHidden = (name, hidden) =>
  act(hidden ? `Hid ${base(name)} from the lists` : `${base(name)} is listed again`,
    () => api("PUT", "/media-hidden", { name, hidden }));

const remove = (name, folderToo) => {
  if (!confirm(`Delete ${folderToo ? "folder " : ""}"${name}"?`)) return;
  return act(`Deleted ${base(name)}`, () => api("DELETE", `/${folderToo ? "media-folders" : "media"}/${enc(name)}`));
};

// Folders as [value, label] options, leaving out `except` and what's in them.
function folderOptions(except = []) {
  return [["", "(top)"], ...state.library.folders
    .map((f) => f.name)
    .filter((f) => !except.some((x) => f === x || f.startsWith(`${x}/`)))
    .map((f) => [f, f])];
}

function dialog(title, body) {
  const d = h("dialog", { "aria-label": title });
  d.addEventListener("close", () => d.remove());
  replace(d, body(d));
  document.body.append(d);
  d.showModal();
  return d;
}

const head = (d, title) => h("div", { class: "dialog-head" }, h("h2", {}, title),
  h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => d.close() }, "×"));

// Rename or move one: a name and a folder to put it in.
function move(name, folderToo) {
  const where = select(folderOptions(folderToo ? [name] : []), parent(name));
  const newName = h("input", { value: base(name), required: true, maxLength: 255 });
  const d = dialog("Rename or move", (d) => h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      const n = newName.value.trim();
      if (n.includes("/")) return toast("A name can't contain '/'; choose the folder below.", "error");
      const to = join(where.value, n);
      if (to === name) return d.close();
      const r = await act(null, () => api("POST", "/media-move", { from: name, to }));
      if (!r) return;
      const also = r.updated?.length ? ` (updated ${r.updated.join(", ")})` : "";
      toast(`Moved to ${to}${also}`, "ok");
      d.close();
    },
  },
    head(d, `Rename or move ${folderToo ? "folder" : "file"}`),
    field("Name", newName),
    field("Folder", where),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
  newName.select();
  return d;
}

// Tags on one file: the box lists them, comma-separated.
function editTags(name) {
  const now = filesByName().get(name)?.tags ?? [];
  const input = h("input", { value: now.join(", "), list: "sound-tags", placeholder: "e.g. bells, school, fire" });
  dialog("Tags", (d) => h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      const want = input.value.split(",").map((t) => t.trim()).filter(Boolean);
      const lower = want.map((t) => t.toLowerCase());
      const remove = now.filter((t) => !lower.includes(t.toLowerCase()));
      if (await act("Tags saved", () => api("PUT", "/media-tags", { names: [name], add: want, remove }))) d.close();
    },
  },
    head(d, `Tags on ${base(name)}`),
    field("Tags", input, "Separate them with commas. Search for one with #tag."),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
  input.focus();
}

async function newFolder() {
  const n = prompt(`New folder in ${folder || "the top"}:`)?.trim();
  if (!n) return;
  if (n.includes("/")) return toast("A folder name can't contain '/'.", "error");
  if (await act(`Made folder ${n}`, () => api("POST", "/media-folders", { name: join(folder, n) }))) {
    await refreshLibrary();
    open(join(folder, n));
  }
}

// ---- many at once ----

// Move `names` into folder `dest`, one by one; says how it went.
async function moveAll(names, dest) {
  const todo = names.filter((n) => parent(n) !== dest
    // Not a folder into itself.
    && !(dest === n || dest.startsWith(`${n}/`)));
  if (!todo.length) return;
  let moved = 0;
  const failed = [];
  for (const n of todo) {
    try {
      await api("POST", "/media-move", { from: n, to: join(dest, base(n)) });
      moved++;
    } catch (e) {
      failed.push(`${base(n)}: ${e.message}`);
    }
  }
  selected.clear();
  if (moved) toast(`Moved ${moved} to ${dest || "the top"}`, "ok");
  if (failed.length) toast(`Couldn't move ${failed.join("; ")}`, "error");
}

function moveSelected() {
  const names = [...selected];
  const where = select(folderOptions(names.filter(isFolder)), folder);
  dialog("Move", (d) => h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      d.close();
      await moveAll(names, where.value);
    },
  },
    head(d, `Move ${names.length} item${names.length === 1 ? "" : "s"}`),
    field("To folder", where),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Move"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
}

function tagSelected(adding) {
  const names = [...selected].filter((n) => !isFolder(n));
  if (!names.length) return toast("Tags go on files; select some files.", "info");
  const have = [...new Set(names.flatMap((n) => filesByName().get(n)?.tags ?? []))];
  const input = adding
    ? h("input", { list: "sound-tags", placeholder: "e.g. bells, school", required: true })
    : select(have.map((t) => [t, t]), have[0] ?? "");
  if (!adding && !have.length) return toast("None of those files has a tag.", "info");
  dialog(adding ? "Add tags" : "Remove a tag", (d) => h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      const tags = adding ? input.value.split(",").map((t) => t.trim()).filter(Boolean) : [input.value];
      const body = adding ? { names, add: tags } : { names, remove: tags };
      const n = `${names.length} file${names.length === 1 ? "" : "s"}`;
      if (await act(adding ? `Tagged ${n}` : `Untagged ${n}`, () => api("PUT", "/media-tags", body))) d.close();
    },
  },
    head(d, adding ? `Add tags to ${names.length} file${names.length === 1 ? "" : "s"}` : "Remove a tag"),
    field(adding ? "Tags" : "Tag", input, adding ? "Separate them with commas" : null),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, adding ? "Add" : "Remove"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
  if (adding) input.focus();
}

async function hideSelected(hidden) {
  const names = [...selected];
  const r = await act(null, async () => {
    for (const name of names) await api("PUT", "/media-hidden", { name, hidden });
  });
  if (r) toast(`${hidden ? "Hid" : "Listed"} ${names.length} item${names.length === 1 ? "" : "s"}`, "ok");
}

async function deleteSelected() {
  const files = filesByName();
  const names = [...selected];
  const skip = names.filter((n) => files.get(n)?.used_by?.length || (isFolder(n) && count(n) > 0));
  const go = names.filter((n) => !skip.includes(n));
  if (!go.length) return toast("Those are in use, or folders that aren't empty: nothing to delete.", "info");
  const note = skip.length ? `\n\n${skip.length} more can't be deleted (in use, or a folder that isn't empty) and will be left.` : "";
  if (!confirm(`Delete ${go.length} item${go.length === 1 ? "" : "s"}?${note}`)) return;
  let done = 0;
  const failed = [];
  for (const n of go) {
    try {
      await api("DELETE", `/${isFolder(n) ? "media-folders" : "media"}/${enc(n)}`);
      done++;
    } catch (e) {
      failed.push(`${base(n)}: ${e.message}`);
    }
  }
  selected.clear();
  if (done) toast(`Deleted ${done}`, "ok");
  if (failed.length) toast(`Couldn't delete ${failed.join("; ")}`, "error");
  if (skip.length) toast(`Left ${skip.map(base).join(", ")}`, "info");
}

// ---- uploads ----

const progress = h("span", { class: "muted small" });
async function uploadFiles(files) {
  const all = [...files];
  for (const [i, f] of all.entries()) {
    const name = join(folder, f.name);
    const exists = state.library.files.some((x) => x.name === name);
    if (exists && !confirm(`Replace ${name}?`)) continue;
    try {
      await upload(name, f, (p) => {
        progress.textContent = `Uploading ${f.name} (${i + 1} of ${all.length}): ${Math.round(p * 100)}%`;
      });
    } catch (e) {
      toast(`Couldn't upload ${f.name}: ${e.message}`, "error");
    }
  }
  progress.textContent = "";
  refreshLibrary();
}

// ---- dragging onto folders ----

const DRAG_TYPE = "application/x-mcast-sounds";

// What a row drags: the selection if it's in it, else just it.
function dragStart(e, name) {
  const names = selected.has(name) ? [...selected] : [name];
  e.dataTransfer.setData(DRAG_TYPE, JSON.stringify(names));
  e.dataTransfer.setData("text/plain", names.join("\n"));
  e.dataTransfer.effectAllowed = "move";
}

// Make `node` take dropped rows into folder `dest`.
function dropTarget(node, dest) {
  if (!can.changeSounds()) return node;
  node.addEventListener("dragover", (e) => {
    if (!e.dataTransfer.types.includes(DRAG_TYPE)) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = "move";
    node.classList.add("drop-here");
  });
  node.addEventListener("dragleave", () => node.classList.remove("drop-here"));
  node.addEventListener("drop", (e) => {
    if (!e.dataTransfer.types.includes(DRAG_TYPE)) return;
    e.preventDefault();
    e.stopPropagation();
    node.classList.remove("drop-here");
    moveAll(JSON.parse(e.dataTransfer.getData(DRAG_TYPE)), dest);
  });
  return node;
}

// ---- the list ----

const filesByName = () => new Map(state.library.files.map((f) => [f.name, f]));
const count = (dir) => state.library.files.filter((f) => parent(f.name) === dir).length
  + state.library.folders.filter((f) => parent(f.name) === dir).length;

function crumbs() {
  const parts = folder ? folder.split("/") : [];
  return h("nav", { class: "crumbs", "aria-label": "Folder" },
    dropTarget(h("button", { class: "link", onclick: () => open("") }, "Sounds"), ""),
    parts.flatMap((p, i) => {
      const path = parts.slice(0, i + 1).join("/");
      return [" / ", i === parts.length - 1
        ? h("strong", {}, p)
        : dropTarget(h("button", { class: "link", onclick: () => open(path) }, p), path)];
    }));
}

// "1:05", or why the mcast server can't play it.
function length(audio, error) {
  if (audio) return h("span", { title: `${audio.codec}, ${audio.rate} Hz, ${audio.channels === 1 ? "mono" : `${audio.channels} channels`}` },
    audio.duration_s < 0.1 ? "under 0.1 s" : audio.duration_s < 10 ? `${audio.duration_s.toFixed(1)} s` : seconds(audio.duration_s));
  if (error) return h("span", { class: "badge failed", title: error }, "can't play");
  return h("span", { class: "muted" }, "—");
}

function tagChips(tags) {
  return (tags ?? []).map((t) => h("button", {
    class: "tag", title: `Show everything tagged ${t}`,
    onclick: (e) => { e.stopPropagation(); setQuery(`#${t}`); },
  }, t));
}

// One row: a file `f`, or a folder `f` (with `folder: true`). `path`: show
// the whole path (in search results).
function row(f, { asFolder = false, path = false } = {}) {
  const name = f.name;
  const label = path ? name : base(name);
  const used = f.used_by ?? [];
  const n = asFolder ? count(name) : 0;
  const changing = can.changeSounds();
  const tr = h("tr", {
    class: [f.hidden && "off", selected.has(name) && "selected"].filter(Boolean).join(" ") || null,
    draggable: changing ? "true" : null,
    ondragstart: changing ? (e) => dragStart(e, name) : null,
  },
    changing && h("td", { class: "check-cell" }, h("input", {
      type: "checkbox", checked: selected.has(name), "aria-label": `Select ${label}`,
      onchange: (e) => { if (e.target.checked) selected.add(name); else selected.delete(name); render(); },
    })),
    h("td", { class: "icon-cell" }, asFolder
      ? h("span", { "aria-hidden": "true" }, "📁")
      : h("button", { class: "icon", title: "Listen", "aria-label": `Listen to ${label}`, onclick: () => listen(name) }, "▶")),
    h("td", {},
      asFolder ? h("button", { class: "link name", onclick: () => open(name) }, label) : label,
      f.hidden && h("span", { class: "badge" }, "hidden"),
      !asFolder && h("span", { class: "tags" }, tagChips(f.tags))),
    h("td", { class: "small" }, asFolder ? "" : length(f.audio, f.audio_error)),
    h("td", { class: "wide muted small" }, asFolder ? `${n} item${n === 1 ? "" : "s"}` : bytes(f.bytes)),
    h("td", { class: "wide small" }, used.length ? used.join(", ") : h("span", { class: "muted" }, "—")),
    h("td", { class: "actions" }, changing && h("div", { class: "buttons nowrap" },
      !asFolder && h("button", { onclick: () => editTags(name) }, "Tags"),
      h("button", { onclick: () => move(name, asFolder) }, "Rename/move"),
      h("button", { onclick: () => setHidden(name, !f.hidden), title: "Hidden files are left out of the announcement lists" },
        f.hidden ? "Unhide" : "Hide"),
      h("button", {
        class: "danger",
        disabled: used.length > 0 || (asFolder && n > 0),
        title: used.length ? `Used by ${used.join(", ")}` : asFolder && n > 0 ? "Empty it first" : null,
        onclick: () => remove(name, asFolder),
      }, "Delete"))));
  return asFolder ? dropTarget(tr, name) : tr;
}

// Does file `f` match the search? Every word must: `#tag` a tag exactly,
// any other word part of the name or of a tag.
function matches(f, words) {
  const tags = (f.tags ?? []).map((t) => t.toLowerCase());
  const name = f.name.toLowerCase();
  return words.every((w) => w.startsWith("#")
    ? tags.includes(w.slice(1))
    : name.includes(w) || tags.some((t) => t.includes(w)));
}

function sorted(files) {
  const key = {
    name: (f) => f.name.toLowerCase(),
    length: (f) => f.audio?.duration_s ?? -1,
    size: (f) => f.bytes,
    date: (f) => f.modified_ms ?? 0,
  }[sort.key] ?? ((f) => f.name);
  return [...files].sort((a, b) => {
    const x = key(a), y = key(b);
    return (x < y ? -1 : x > y ? 1 : 0) * (sort.desc ? -1 : 1);
  });
}

function sortHeader(label, key, cls) {
  const on = sort.key === key;
  return h("th", { class: cls, "aria-sort": on ? (sort.desc ? "descending" : "ascending") : null },
    h("button", {
      class: "link sort",
      onclick: () => {
        sort = { key, desc: on ? !sort.desc : false };
        saved.set("sounds.sort", sort);
        render();
      },
    }, label, on ? (sort.desc ? " ↓" : " ↑") : ""));
}

function setQuery(q) {
  query = q;
  search.value = q;
  render();
}

let el = null;
let fileInput = null;
let search = null;
let crumbsBox = null;
let tagsBox = null;
let selectionBox = null;
let listBox = null;

function renderTags() {
  const all = state.library.tags ?? [];
  replace(tagsBox, all.length > 0 && [
    h("span", { class: "muted small" }, "Tags:"),
    all.map((t) => h("button", {
      class: `tag${query.toLowerCase() === `#${t.tag.toLowerCase()}` ? " on" : ""}`,
      onclick: () => setQuery(query.toLowerCase() === `#${t.tag.toLowerCase()}` ? "" : `#${t.tag}`),
    }, t.tag, h("span", { class: "muted" }, ` ${t.files}`))),
  ]);
  replace(document.getElementById("sound-tags"), all.map((t) => h("option", { value: t.tag })));
}

function renderSelection() {
  // Forget what's gone.
  const names = new Set([...state.library.files, ...state.library.folders].map((f) => f.name));
  for (const n of selected) if (!names.has(n)) selected.delete(n);
  const n = selected.size;
  selectionBox.hidden = n === 0;
  if (!n) return;
  const anyHidden = [...selected].some((s) => [...state.library.files, ...state.library.folders].find((f) => f.name === s)?.hidden);
  replace(selectionBox,
    h("strong", {}, `${n} selected`),
    h("button", { onclick: moveSelected }, "Move to…"),
    h("button", { onclick: () => tagSelected(true) }, "Add tags…"),
    h("button", { onclick: () => tagSelected(false) }, "Remove a tag…"),
    h("button", { onclick: () => hideSelected(!anyHidden) }, anyHidden ? "Unhide" : "Hide"),
    h("button", { class: "danger", onclick: deleteSelected }, "Delete…"),
    h("span", { class: "spacer" }),
    h("button", { class: "link", onclick: () => { selected.clear(); render(); } }, "Clear"));
}

function render() {
  if (!el) return;
  const { files, folders } = state.library;
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const searching = words.length > 0;
  // Searching: every file that matches, in any folder. Otherwise the folder
  // shown (a folder that's gone shows as empty; an upload there makes it again).
  const inHere = (n) => parent(n) === folder;
  const subfolders = searching ? [] : folders.filter((f) => inHere(f.name)).sort((a, b) => a.name.localeCompare(b.name));
  const shown = sorted(searching ? files.filter((f) => matches(f, words)) : files.filter((f) => inHere(f.name)));
  const visible = [...subfolders, ...shown].map((f) => f.name);
  const changing = can.changeSounds();
  const allOn = visible.length > 0 && visible.every((n) => selected.has(n));

  replace(crumbsBox, searching
    ? h("span", {}, `${shown.length} match${shown.length === 1 ? "" : "es"} in all folders · `,
      h("button", { class: "link", onclick: () => setQuery("") }, "clear search"))
    : crumbs());
  renderTags();
  renderSelection();
  replace(listBox,
    state.libraryError && h("p", { class: "error" }, `Couldn't list the files: ${state.libraryError}`),
    subfolders.length || shown.length || (folder && !searching)
      ? h("div", { class: "scroll" }, h("table", { class: "items files" },
          h("thead", {}, h("tr", {},
            changing && h("th", { class: "check-cell" }, h("input", {
              type: "checkbox", checked: allOn, "aria-label": "Select all shown",
              onchange: (e) => { for (const n of visible) e.target.checked ? selected.add(n) : selected.delete(n); render(); },
            })),
            h("th"),
            sortHeader("Name", "name"), sortHeader("Length", "length"), sortHeader("Size", "size", "wide"),
            h("th", { class: "wide" }, "Used by"), h("th"))),
          h("tbody", {},
            folder && !searching && dropTarget(h("tr", {}, changing && h("td"), h("td", { class: "icon-cell" }, "↰"),
              h("td", { colspan: 5 }, h("button", { class: "link", onclick: () => open(parent(folder)) }, "Up a folder"))), parent(folder)),
            subfolders.map((f) => row(f, { asFolder: true })),
            shown.map((f) => row(f, { path: searching })))))
      : h("p", { class: "muted" }, searching ? "Nothing matches." : "No sound files yet: upload or record some."),
    !changing && h("p", { class: "muted small" }, "This token can listen to sounds but not change them."),
    h("p", { class: "muted small" },
      changing ? "Drag files onto a folder (or the path above) to move them; select several to move, tag, hide or delete them together. " : "",
      "Files live on the mcast server, so its own pages see the same folders. Hidden files and folders are only left out of the panel's lists; announcements that use one still play it."));
}

export function mount(target) {
  el = target;
  fileInput = h("input", {
    type: "file", multiple: true, accept: "audio/*,.wav,.mp3,.ogg,.flac,.m4a", hidden: true,
    onchange: () => { uploadFiles(fileInput.files); fileInput.value = ""; },
  });
  document.body.append(fileInput, h("datalist", { id: "sound-tags" }));
  search = h("input", {
    type: "search", class: "search", placeholder: "Search names and tags, or #tag", "aria-label": "Search sounds",
    oninput: () => { query = search.value; render(); },
  });
  crumbsBox = h("div", { class: "crumbs-box" });
  tagsBox = h("div", { class: "tag-bar" });
  selectionBox = h("div", { class: "selection-bar", hidden: true });
  listBox = h("div");
  const changing = can.changeSounds();
  replace(el,
    h("div", { class: "toolbar" },
      crumbsBox,
      h("span", { class: "spacer" }),
      search,
      changing && h("button", { class: "primary", onclick: () => fileInput.click() }, "Upload…"),
      changing && h("button", {
        onclick: () => openRecorder(folder, (n) => state.library.files.some((x) => x.name === n), refreshLibrary),
      }, "Record…"),
      changing && h("button", { onclick: newFolder }, "New folder")),
    progress, tagsBox, selectionBox, listBox);
  // Drop files from the computer anywhere on the tab to upload them here.
  const fromComputer = (e) => e.dataTransfer?.types?.includes("Files");
  if (changing) {
    el.addEventListener("dragover", (e) => { if (fromComputer(e)) { e.preventDefault(); el.classList.add("dropping"); } });
    el.addEventListener("dragleave", () => el.classList.remove("dropping"));
    el.addEventListener("drop", (e) => {
      if (!fromComputer(e)) return;
      e.preventDefault();
      el.classList.remove("dropping");
      if (e.dataTransfer.files.length) uploadFiles(e.dataTransfer.files);
    });
  }
  subscribe(["library", "config"], render);
  render();
}
