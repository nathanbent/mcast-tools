// Sounds: the mcast server's media files, in folders. Upload, listen,
// rename or move, hide from the panel's lists, delete. A file an
// announcement uses can't be deleted, and renaming or moving it updates
// the announcement.

import { api, mediaUrl, upload } from "./api.js";
import { h, replace, toast, bytes, saved } from "./dom.js";
import { state, subscribe, refreshLibrary, can } from "./store.js";
import { field, select } from "./editor.js";

let folder = saved.get("sounds.folder", ""); // the folder being shown; "" is the top
let player = null;

const base = (name) => name.slice(name.lastIndexOf("/") + 1);
const parent = (name) => (name.includes("/") ? name.slice(0, name.lastIndexOf("/")) : "");
const join = (dir, name) => (dir ? `${dir}/${name}` : name);

function open(dir) {
  folder = dir;
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

const setHidden = (name, hidden) =>
  act(hidden ? `Hid ${base(name)} from the lists` : `${base(name)} is listed again`,
    () => api("PUT", "/media-hidden", { name, hidden }));

const remove = (name, isFolder) => {
  if (!confirm(`Delete ${isFolder ? "folder" : ""} "${name}"?`)) return;
  return act(`Deleted ${base(name)}`, () =>
    api("DELETE", `/${isFolder ? "media-folders" : "media"}/${name.split("/").map(encodeURIComponent).join("/")}`));
};

// Rename or move: a name and a folder to put it in.
let dialog = null;
function move(name, isFolder) {
  dialog?.remove();
  const folders = [["", "(top)"], ...state.library.folders
    .map((f) => f.name)
    // A folder can't go inside itself.
    .filter((f) => !isFolder || (f !== name && !f.startsWith(`${name}/`)))
    .map((f) => [f, f])];
  const where = select(folders, parent(name));
  const newName = h("input", { value: base(name), required: true, maxLength: 255 });
  dialog = h("dialog", { "aria-labelledby": "move-title" },
    h("form", {
      onsubmit: async (e) => {
        e.preventDefault();
        const n = newName.value.trim();
        if (n.includes("/")) return toast("A name can't contain '/'; choose the folder below.", "error");
        const to = join(where.value, n);
        if (to === name) return dialog.close();
        const r = await act(null, () => api("POST", "/media-move", { from: name, to }));
        if (!r) return;
        const also = r.updated?.length ? ` (updated ${r.updated.join(", ")})` : "";
        toast(`Moved to ${to}${also}`, "ok");
        dialog.close();
      },
    },
      h("div", { class: "dialog-head" },
        h("h2", { id: "move-title" }, `Rename or move ${isFolder ? "folder" : "file"}`),
        h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => dialog.close() }, "×")),
      field("Name", newName),
      field("Folder", where),
      h("div", { class: "buttons" },
        h("button", { type: "submit", class: "primary" }, "Save"),
        h("button", { type: "button", onclick: () => dialog.close() }, "Cancel"))));
  document.body.append(dialog);
  dialog.showModal();
  newName.select();
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

// Upload files into the folder shown, one after another.
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

function crumbs() {
  const parts = folder ? folder.split("/") : [];
  return h("nav", { class: "crumbs", "aria-label": "Folder" },
    h("button", { class: "link", onclick: () => open("") }, "Sounds"),
    parts.flatMap((p, i) => [" / ",
      i === parts.length - 1
        ? h("strong", {}, p)
        : h("button", { class: "link", onclick: () => open(parts.slice(0, i + 1).join("/")) }, p)]));
}

function row({ name, isFolder, hidden, size, used, count }) {
  const label = base(name);
  return h("tr", { class: hidden ? "off" : null },
    h("td", { class: "icon-cell" }, isFolder
      ? h("span", { "aria-hidden": "true" }, "📁")
      : h("button", { class: "icon", title: "Listen", "aria-label": `Listen to ${label}`, onclick: () => listen(name) }, "▶")),
    h("td", {},
      isFolder ? h("button", { class: "link name", onclick: () => open(name) }, label) : label,
      hidden && h("span", { class: "badge" }, "hidden")),
    h("td", { class: "wide muted small" }, isFolder ? `${count} item${count === 1 ? "" : "s"}` : bytes(size)),
    h("td", { class: "wide small" }, used?.length ? used.join(", ") : h("span", { class: "muted" }, "—")),
    h("td", { class: "actions" }, can.changeSounds() && h("div", { class: "buttons nowrap" },
      h("button", { onclick: () => move(name, isFolder) }, "Rename/move"),
      h("button", { onclick: () => setHidden(name, !hidden), title: "Hidden files are left out of the announcement lists" },
        hidden ? "Unhide" : "Hide"),
      h("button", {
        class: "danger",
        disabled: !!used?.length || (isFolder && count > 0),
        title: used?.length ? `Used by ${used.join(", ")}` : isFolder && count > 0 ? "Empty it first" : null,
        onclick: () => remove(name, isFolder),
      }, "Delete"))));
}

let el = null;
let fileInput = null;

function render() {
  const { files, folders } = state.library;
  // A folder that's gone (moved or deleted elsewhere) shows as empty; an
  // upload there makes it again.
  const inHere = (n) => parent(n) === folder;
  const subfolders = folders.filter((f) => inHere(f.name));
  const here = files.filter((f) => inHere(f.name));
  const count = (dir) => files.filter((f) => parent(f.name) === dir).length
    + folders.filter((f) => parent(f.name) === dir).length;
  replace(el,
    h("div", { class: "toolbar" },
      crumbs(),
      h("span", { class: "spacer" }),
      can.changeSounds() && h("button", { class: "primary", onclick: () => fileInput.click() }, "Upload…"),
      can.changeSounds() && h("button", { onclick: newFolder }, "New folder")),
    progress,
    state.libraryError && h("p", { class: "error" }, `Couldn't list the files: ${state.libraryError}`),
    subfolders.length || here.length
      ? h("div", { class: "scroll" }, h("table", { class: "items files" },
          h("thead", {}, h("tr", {}, h("th"), h("th", {}, "Name"), h("th", { class: "wide" }, "Size"),
            h("th", { class: "wide" }, "Used by"), h("th"))),
          h("tbody", {},
            folder && h("tr", {}, h("td", { class: "icon-cell" }, "↰"),
              h("td", { colspan: 4 }, h("button", { class: "link", onclick: () => open(parent(folder)) }, "Up a folder"))),
            subfolders.map((f) => row({ name: f.name, isFolder: true, hidden: f.hidden, count: count(f.name) })),
            here.map((f) => row({ name: f.name, hidden: f.hidden, size: f.bytes, used: f.used_by })))))
      : h("p", { class: "muted" }, folder ? "This folder is empty." : "No sound files yet: upload some."),
    !can.changeSounds() && h("p", { class: "muted small" }, "This token can listen to sounds but not change them."),
    h("p", { class: "muted small" },
      "Files live on the mcast server, so its own pages see the same folders. Hidden files and folders are only left out of the panel's lists; announcements that use one still play it."));
}

export function mount(target) {
  el = target;
  fileInput = h("input", {
    type: "file", multiple: true, accept: "audio/*,.wav,.mp3,.ogg,.flac,.m4a", hidden: true,
    onchange: () => { uploadFiles(fileInput.files); fileInput.value = ""; },
  });
  document.body.append(fileInput);
  // Drop files anywhere on the tab to upload them here.
  if (can.changeSounds()) el.addEventListener("dragover", (e) => { e.preventDefault(); el.classList.add("dropping"); });
  if (can.changeSounds()) el.addEventListener("dragleave", () => el.classList.remove("dropping"));
  if (can.changeSounds()) el.addEventListener("drop", (e) => {
    e.preventDefault();
    el.classList.remove("dropping");
    if (e.dataTransfer?.files?.length) uploadFiles(e.dataTransfer.files);
  });
  subscribe(["library", "config"], render);
  render();
}
