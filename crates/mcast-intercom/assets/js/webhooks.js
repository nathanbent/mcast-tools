// Settings → Other systems: the trigger API's URLs for each announcement,
// and outbound webhooks (the panel POSTs JSON when something happens).

import { api } from "./api.js";
import { h, replace, toast, ago } from "./dom.js";
import { state, subscribe } from "./store.js";
import { field, select } from "./editor.js";

const EVENTS = [
  ["run_started", "An announcement started"],
  ["run_ended", "One finished or was stopped"],
  ["run_failed", "One couldn't play (all groups busy or failed)"],
  ["schedule_missed", "A schedule was missed"],
  ["backend_down", "The mcast server became unreachable"],
  ["backend_up", "It's back"],
];

let hooks = [];
let error = null;
let list = null;

async function refresh() {
  try {
    hooks = await api("GET", "/webhooks");
    error = null;
  } catch (e) {
    error = e.message;
  }
  render();
}

function dialog(title) {
  const d = h("dialog", { class: "wide", "aria-label": title });
  d.addEventListener("close", () => d.remove());
  document.body.append(d);
  return d;
}

const closeBtn = (d) => h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => d.close() }, "×");

function edit(w) {
  const title = w ? `Change ${w.name}` : "New webhook";
  const d = dialog(title);
  const name = h("input", { value: w?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Home Assistant" });
  const url = h("input", { type: "url", value: w?.url ?? "", required: true, placeholder: "https://example.com/hook" });
  const chosen = new Set(w?.events ?? []);
  const events = h("div", { class: "checks column" }, EVENTS.map(([e, label]) => h("label", { class: "check" },
    h("input", { type: "checkbox", checked: chosen.has(e), onchange: (ev) => { if (ev.target.checked) chosen.add(e); else chosen.delete(e); } }),
    ` ${label} `, h("code", { class: "small muted" }, e))));
  const sign = h("input", { type: "checkbox", checked: true });
  replace(d, h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      const body = { name: name.value.trim(), url: url.value.trim(), events: [...chosen] };
      try {
        if (w) {
          await api("PUT", `/webhooks/${encodeURIComponent(w.id)}`, body);
          toast(`Saved ${body.name}`, "ok");
          d.close();
        } else {
          const r = await api("POST", "/webhooks", { ...body, sign: sign.checked });
          d.close();
          if (r.secret) showSecret(r.webhook, r.secret);
          else toast(`Added ${body.name}`, "ok");
        }
        refresh();
      } catch (err) {
        toast(`Couldn't save: ${err.message}`, "error");
      }
    },
  },
    h("div", { class: "dialog-head" }, h("h2", {}, title), closeBtn(d)),
    field("Name", name),
    field("URL", url, "The panel POSTs JSON here"),
    h("fieldset", { class: "part" }, h("legend", {}, "Events"),
      h("p", { class: "muted small" }, "None ticked: all of them."), events),
    !w && h("label", { class: "check" }, sign, " Sign each delivery (X-Mcast-Signature), so the receiver can check it came from this panel"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, w ? "Save" : "Add webhook"),
      h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
  d.showModal();
  name.focus();
}

function showSecret(w, secret) {
  const d = dialog("Signing secret");
  const input = h("input", { class: "mono", value: secret, readOnly: true, onfocus: (e) => e.target.select() });
  replace(d, h("div", {},
    h("div", { class: "dialog-head" }, h("h2", {}, `"${w.name}" added`), closeBtn(d)),
    h("p", {}, h("strong", {}, "Copy its signing secret now: "), "it's only shown this once."),
    h("div", { class: "inline-form" }, input, h("button", {
      type: "button", class: "primary",
      onclick: async () => {
        try { await navigator.clipboard.writeText(secret); toast("Copied", "ok"); } catch { input.focus(); toast("Press Ctrl+C (or ⌘C) to copy it", "info"); }
      },
    }, "Copy")),
    h("p", { class: "muted small" },
      "Each delivery has X-Mcast-Timestamp and X-Mcast-Signature: sha256= and the hex HMAC-SHA256, with this secret, of the timestamp, a dot, and the body."),
    h("div", { class: "buttons" }, h("button", { type: "button", onclick: () => d.close() }, "Done"))));
  d.showModal();
  input.focus();
}

async function test(w) {
  toast(`Sending a test to ${w.name}…`, "info");
  try {
    const r = await api("POST", `/webhooks/${encodeURIComponent(w.id)}/test`);
    if (r.ok) toast(`${w.name} answered HTTP ${r.status} in ${r.took_ms} ms`, "ok");
    else toast(`${w.name}: ${r.error}`, "error");
  } catch (e) {
    toast(`Couldn't test: ${e.message}`, "error");
  }
  refresh();
}

async function toggle(w) {
  try {
    await api("PUT", `/webhooks/${encodeURIComponent(w.id)}`, { enabled: !w.enabled });
  } catch (e) {
    toast(`Couldn't change it: ${e.message}`, "error");
  }
  refresh();
}

async function remove(w) {
  if (!confirm(`Delete the webhook "${w.name}"?`)) return;
  try {
    await api("DELETE", `/webhooks/${encodeURIComponent(w.id)}`);
    toast(`Deleted ${w.name}`, "ok");
  } catch (e) {
    toast(`Couldn't delete: ${e.message}`, "error");
  }
  refresh();
}

function lastDelivery(d) {
  if (!d) return h("span", { class: "muted" }, "nothing sent yet");
  return [
    h("span", { class: `badge ${d.ok ? "done" : "failed"}` }, d.ok ? `HTTP ${d.status}` : "failed"),
    " ", h("span", { class: "muted" }, `${d.event}, ${ago(d.at_ms)}`),
    !d.ok && d.error && h("div", { class: "error small" }, d.error),
  ];
}

function render() {
  if (!list) return;
  replace(list,
    error && h("p", { class: "error" }, `Couldn't list the webhooks: ${error}`),
    hooks.length
      ? h("div", { class: "scroll" }, h("table", { class: "items" },
          h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", { class: "wide" }, "URL"),
            h("th", { class: "wide" }, "Events"), h("th", {}, "Last delivery"), h("th"))),
          h("tbody", {}, hooks.map((w) => h("tr", { class: w.enabled ? null : "off" },
            h("td", {}, w.name, !w.enabled && h("span", { class: "badge" }, "off")),
            h("td", { class: "wide small mono" }, w.url),
            h("td", { class: "wide small" }, w.events.length ? w.events.join(", ") : "all"),
            h("td", { class: "small" }, lastDelivery(w.last_delivery)),
            h("td", { class: "actions" }, h("div", { class: "buttons nowrap" },
              h("button", { onclick: () => test(w) }, "Test"),
              h("button", { onclick: () => toggle(w) }, w.enabled ? "Turn off" : "Turn on"),
              h("button", { onclick: () => edit(w) }, "Change"),
              h("button", { class: "danger", onclick: () => remove(w) }, "Delete"))))))))
      : !error && h("p", { class: "muted" }, "No webhooks yet."),
    h("div", { class: "buttons" }, h("button", { class: "primary", onclick: () => edit(null) }, "New webhook…")));
}

// The trigger API's URLs for one announcement, to copy.
function triggerUrls() {
  const el = h("div");
  let chosen = null;
  const show = () => {
    const anns = state.config.announcements.filter((a) => a.audio !== "live");
    if (!anns.some((a) => a.id === chosen)) chosen = anns[0]?.id ?? null;
    const base = `${location.origin}/api/intercom/v1`;
    const pick = select(anns.map((a) => [a.id, a.name]), chosen ?? "");
    pick.onchange = () => { chosen = pick.value; show(); };
    const id = encodeURIComponent(chosen ?? "ID");
    replace(el,
      h("p", { class: "muted" },
        "Other systems play an announcement with a trigger token (make one in Access tokens): a POST with the token in an Authorization header, or, for devices that can only fetch a URL, a GET with it in the URL. Names work as well as ids. Add zones=… to play to only some of its zones, or repeat=… to change how often it plays."),
      anns.length ? field("Announcement", pick) : h("p", { class: "muted" }, "No announcements to trigger yet."),
      anns.length && h("pre", { class: "mono small wrap" },
        `curl -X POST -H "Authorization: Bearer TOKEN" ${base}/trigger/${id}\n\n`
        + `${base}/trigger/${id}?access_token=TOKEN\n\n`
        + `Stop it:  ${base}/trigger/${id}/stop\nStop all: ${base}/stop-all`));
  };
  show();
  subscribe(["config"], show);
  return el;
}

export function mount() {
  list = h("div");
  refresh();
  subscribe(["config"], refresh);
  return h("div", {},
    triggerUrls(),
    h("h3", {}, "Webhooks"),
    h("p", { class: "muted" }, "The panel POSTs JSON to these when announcements start, end or fail, when a schedule is missed, and when the mcast server goes away or comes back. Deliveries time out after 5 s and are tried once more on a network error or a 5xx."),
    list);
}
