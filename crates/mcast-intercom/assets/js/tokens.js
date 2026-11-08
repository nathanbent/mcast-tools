// Settings → Access tokens: named keys to the panel, each with a role. The
// secret is shown once, when a token is made; only its hash is kept.

import { api } from "./api.js";
import { h, replace, toast, ago } from "./dom.js";
import { state, subscribe, announcementName } from "./store.js";
import { field, select } from "./editor.js";

const ROLES = [
  ["operator", "Operator: use the panel (play, stop, push to talk), change nothing"],
  ["trigger", "Trigger: other systems play announcements through the API"],
  ["media", "Media: upload and manage sound files"],
  ["admin", "Admin: everything, tokens included"],
];

let tokens = [];
let error = null;
let el = null;

async function refresh() {
  try {
    tokens = await api("GET", "/tokens");
    error = null;
  } catch (e) {
    error = e.message;
  }
  render();
}

function closeOnly(d) {
  return h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => d.close() }, "×");
}

function dialog(title) {
  const d = h("dialog", { class: "wide", "aria-label": title });
  d.addEventListener("close", () => d.remove());
  document.body.append(d);
  return d;
}

// Make a token, or change one (`t`): name, announcements, expiry.
function edit(t) {
  const d = dialog(t ? `Change ${t.name}` : "New access token");
  const name = h("input", { value: t?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Front doorbell" });
  const role = select(ROLES, t?.role ?? "operator");
  role.disabled = !!t;
  const expires = h("input", { type: "date", value: t?.expires ?? "" });
  const chosen = new Set(t?.announcements ?? []);
  const anns = h("div", { class: "checks" },
    state.config.announcements.map((a) => h("label", { class: "check" },
      h("input", {
        type: "checkbox", checked: chosen.has(a.id),
        onchange: (e) => { if (e.target.checked) chosen.add(a.id); else chosen.delete(a.id); },
      }), ` ${a.name}`)));
  const annField = h("fieldset", { class: "part" },
    h("legend", {}, "Announcements it may play"),
    h("p", { class: "muted small" }, "None ticked: any announcement."),
    state.config.announcements.length ? anns : h("p", { class: "muted" }, "No announcements yet."));
  const cats = new Set(t?.categories ?? []);
  const catField = h("fieldset", { class: "part" },
    h("legend", {}, "Categories it shows and plays"),
    h("p", { class: "muted small" }, "None ticked: all of them. Ticked: only announcements in those categories, on any screen it's used on (a reception tablet, say)."),
    (state.config.categories ?? []).length
      ? h("div", { class: "checks" }, state.config.categories.map((c) => h("label", { class: "check" },
          h("input", { type: "checkbox", checked: cats.has(c.id), onchange: (e) => { if (e.target.checked) cats.add(c.id); else cats.delete(c.id); } }),
          ` ${c.name}`)))
      : h("p", { class: "muted" }, "No categories yet (Announcements tab)."));
  const showAnns = () => { annField.hidden = role.value !== "trigger"; catField.hidden = role.value !== "operator"; };
  role.onchange = showAnns;
  showAnns();
  replace(d, h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      const body = {
        name: name.value.trim(),
        announcements: role.value === "trigger" ? [...chosen] : [],
        categories: role.value === "operator" ? [...cats] : [],
        expires: expires.value || null,
      };
      try {
        if (t) {
          await api("PUT", `/tokens/${encodeURIComponent(t.id)}`, body);
          toast(`Saved ${body.name}`, "ok");
          d.close();
        } else {
          const r = await api("POST", "/tokens", { ...body, role: role.value });
          d.close();
          showSecret(r.token, r.secret);
        }
        refresh();
      } catch (err) {
        toast(`Couldn't save: ${err.message}`, "error");
      }
    },
  },
    h("div", { class: "dialog-head" }, h("h2", {}, t ? `Change ${t.name}` : "New access token"), closeOnly(d)),
    field("Name", name, "What or who it's for; shown in the history"),
    field("Role", role, t ? "A token's role can't change: make a new token instead" : null),
    annField,
    catField,
    field("Last day it works", expires, "Optional; it stops at the end of that day in the panel's time zone"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, t ? "Save" : "Make token"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
  d.showModal();
  name.focus();
}

// The new secret, once, with a way to copy it.
function showSecret(t, secret) {
  const d = dialog("Token made");
  const input = h("input", { class: "mono", value: secret, readOnly: true, onfocus: (e) => e.target.select() });
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(secret);
      toast("Copied", "ok");
    } catch {
      input.focus();
      toast("Press Ctrl+C (or ⌘C) to copy it", "info");
    }
  };
  const example = t.announcements[0] ?? state.config.announcements[0]?.id ?? "ANNOUNCEMENT";
  const base = `${location.origin}/api/intercom/v1/trigger/${encodeURIComponent(example)}`;
  const curl = `curl -X POST -H "Authorization: Bearer ${secret}" ${base}\n\nOr, from a device that can only fetch a URL:\n${base}?access_token=${secret}`;
  replace(d, h("div", {},
    h("div", { class: "dialog-head" }, h("h2", {}, `"${t.name}" is ready`), closeOnly(d)),
    h("p", {}, h("strong", {}, "Copy the secret now: "), "it's only shown this once. If it's lost, revoke the token and make a new one."),
    h("div", { class: "inline-form" }, input, h("button", { type: "button", class: "primary", onclick: copy }, "Copy")),
    t.role === "trigger" && [
      h("p", { class: "muted small" }, "For example, to play an announcement (Settings → Other systems has more):"),
      h("pre", { class: "mono small wrap" }, curl),
    ],
    t.role !== "trigger" && h("p", { class: "muted small" }, "Sign in to this page with it, or send it as ", h("code", {}, "Authorization: Bearer …"), "."),
    h("div", { class: "buttons" }, h("button", { type: "button", onclick: () => d.close() }, "Done"))));
  d.showModal();
  input.focus();
}

async function revoke(t) {
  if (!confirm(`Revoke "${t.name}"? Anything using it stops working at once.`)) return;
  try {
    await api("DELETE", `/tokens/${encodeURIComponent(t.id)}`);
    toast(`Revoked ${t.name}`, "ok");
  } catch (e) {
    toast(`Couldn't revoke: ${e.message}`, "error");
  }
  refresh();
}

function render() {
  if (!el) return;
  const used = (t) => (t.last_used_ms ? `${ago(t.last_used_ms)} · ${t.uses} use${t.uses === 1 ? "" : "s"}` : "never");
  const catName = (id) => (state.config.categories ?? []).find((c) => c.id === id)?.name ?? id;
  const limits = (t) => t.role === "trigger"
    ? (t.announcements.length ? t.announcements.map(announcementName).join(", ") : "any announcement")
    : t.role === "operator" && t.categories?.length ? `categories: ${t.categories.map(catName).join(", ")}`
    : h("span", { class: "muted" }, "—");
  replace(el,
    h("p", { class: "muted" },
      "Give each person, screen or system its own token, so it gets only what it needs and can be revoked on its own. The startup token (",
      h("code", {}, "--panel-token"), ") is an admin token that isn't listed here."),
    error && h("p", { class: "error" }, `Couldn't list the tokens: ${error}`),
    tokens.length
      ? h("div", { class: "scroll" }, h("table", { class: "items" },
          h("thead", {}, h("tr", {},
            h("th", {}, "Name"), h("th", {}, "Role"), h("th", { class: "wide" }, "Plays"),
            h("th", { class: "wide" }, "Last used"), h("th", { class: "wide" }, "Expires"), h("th"))),
          h("tbody", {}, tokens.map((t) => h("tr", { class: t.expired ? "off" : null },
            h("td", {}, t.name),
            h("td", {}, t.role),
            h("td", { class: "wide small" }, limits(t)),
            h("td", { class: "wide small" }, used(t)),
            h("td", { class: "wide small" }, t.expired
              ? h("span", { class: "badge failed" }, `expired after ${t.expires}`)
              : t.expires ?? h("span", { class: "muted" }, "never")),
            h("td", { class: "actions" }, h("div", { class: "buttons nowrap" },
              h("button", { onclick: () => edit(t) }, "Change"),
              h("button", { class: "danger", onclick: () => revoke(t) }, "Revoke"))))))))
      : !error && h("p", { class: "muted" }, "No access tokens yet."),
    h("div", { class: "buttons" }, h("button", { class: "primary", onclick: () => edit(null) }, "New token…")));
}

export function mount() {
  el = h("div");
  refresh();
  // Token changes come as config_changed.
  subscribe(["config"], refresh);
  return el;
}
