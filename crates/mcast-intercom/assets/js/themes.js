// The theme switch in the header: System → Light → Dark, kept in this
// browser (js/theme.js applies it before the page draws).

const KEY = "mcast.theme";
const NAMES = { system: "System theme", light: "Light theme", dark: "Dark theme" };
// Small icons, drawn with the text color.
const ICONS = {
  system: [["circle", { cx: 8, cy: 8, r: 6, fill: "none", stroke: "currentColor", "stroke-width": 1.5 }],
    ["path", { d: "M8 2a6 6 0 0 1 0 12z", fill: "currentColor" }]],
  light: [["circle", { cx: 8, cy: 8, r: 3, fill: "currentColor" }],
    ...[0, 45, 90, 135, 180, 225, 270, 315].map((a) => ["line", {
      x1: 8 + 4.6 * Math.cos((a * Math.PI) / 180), y1: 8 + 4.6 * Math.sin((a * Math.PI) / 180),
      x2: 8 + 6.6 * Math.cos((a * Math.PI) / 180), y2: 8 + 6.6 * Math.sin((a * Math.PI) / 180),
      stroke: "currentColor", "stroke-width": 1.5, "stroke-linecap": "round",
    }])],
  dark: [["path", { d: "M13 10.2A5.6 5.6 0 0 1 5.8 3a5.6 5.6 0 1 0 7.2 7.2z", fill: "currentColor" }]],
};

function icon(kind) {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("width", "16");
  svg.setAttribute("height", "16");
  svg.setAttribute("aria-hidden", "true");
  for (const [tag, attrs] of ICONS[kind]) {
    const el = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
    svg.append(el);
  }
  return svg;
}

export function currentTheme() {
  try { return localStorage.getItem(KEY) || "system"; } catch { return "system"; }
}

export function setTheme(t) {
  try { t === "system" ? localStorage.removeItem(KEY) : localStorage.setItem(KEY, t); } catch {}
  const root = document.documentElement;
  if (t === "system") delete root.dataset.theme;
  else root.dataset.theme = t;
  const meta = document.querySelector('meta[name="color-scheme"]');
  if (meta) meta.content = t === "system" ? "light dark" : t;
}

// Make `button` show the theme and cycle through them.
export function themeButton(button) {
  const show = () => {
    const t = currentTheme();
    const next = { system: "light", light: "dark", dark: "system" }[t];
    button.replaceChildren(icon(t));
    button.title = `${NAMES[t]} (click for ${NAMES[next].toLowerCase()})`;
    button.setAttribute("aria-label", button.title);
  };
  button.onclick = () => {
    setTheme({ system: "light", light: "dark", dark: "system" }[currentTheme()]);
    show();
  };
  show();
}
