// Tiny DOM helpers (as in mcast-web). Text always goes in as text nodes,
// never as HTML: names here can come from anyone who can edit the config.

export function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    // An object, not a string: the CSP forbids inline style attributes.
    else if (k === "style") Object.assign(el.style, v);
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k in el && typeof v !== "string") el[k] = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const c of children.flat(Infinity)) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

export function replace(el, ...children) {
  el.replaceChildren();
  append(el, children);
}

export function seconds(s) {
  s = Math.max(0, Math.floor(s));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

export function bytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1 << 20) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / (1 << 20)).toFixed(1)} MB`;
}

export function ago(ms) {
  const s = (Date.now() - ms) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  return new Date(ms).toLocaleTimeString();
}

const toasts = () => document.getElementById("toasts");

// Brief message in the corner. `action` is an optional [label, fn] button.
export function toast(message, kind = "info", action = null) {
  const el = h("div", { class: `toast ${kind}`, role: kind === "error" ? "alert" : "status" },
    h("span", {}, message),
    action && h("button", { class: "link", onclick: () => { action[1](); el.remove(); } }, action[0]),
    h("button", { class: "close", "aria-label": "Dismiss", onclick: () => el.remove() }, "×"));
  toasts().append(el);
  setTimeout(() => el.remove(), kind === "error" ? 10000 : 4000);
}

// Remember form values between visits.
export const saved = {
  get(key, fallback) {
    try { return JSON.parse(localStorage.getItem(`mcast-intercom.${key}`)) ?? fallback; } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem(`mcast-intercom.${key}`, JSON.stringify(value)); } catch {}
  },
};
