// The display menu in the header: theme, accent color, density and text
// size, kept in this browser (js/theme.js applies them before the page
// draws, and holds the choices).

const display = window.mcastDisplay;

// [setting, its heading, [value, label]...], in the menu's order.
const GROUPS = [
  ["theme", "Theme", [["system", "System"], ["light", "Light"], ["dark", "Dark"]]],
  ["accent", "Accent color", [["blue", "Blue"], ["teal", "Teal"], ["green", "Green"],
    ["purple", "Purple"], ["orange", "Orange"], ["pink", "Pink"]]],
  ["density", "Density", [["comfortable", "Comfortable"], ["compact", "Compact"]]],
  ["text", "Text size", [["small", "Small"], ["medium", "Medium"], ["large", "Large"]]],
];

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

function el(tag, props = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") e.className = v;
    else e.setAttribute(k, v);
  }
  e.append(...children);
  return e;
}

// Make `button` show the theme and open the display menu.
export function themeButton(button) {
  const menu = el("div", { class: "display-menu", role: "dialog", "aria-label": "Display options", hidden: "" });
  const choices = [];
  for (const [key, title, values] of GROUPS) {
    const id = `display-${key}`;
    const row = el("div", { class: `choices ${key}`, role: "group", "aria-labelledby": id });
    for (const [value, label] of values) {
      const b = el("button", { type: "button", "data-value": value });
      if (key === "accent") {
        // A swatch of the color, named for screen readers and on hover.
        b.dataset.accent = value;
        b.title = label;
        b.setAttribute("aria-label", label);
      } else {
        b.textContent = label;
      }
      b.onclick = () => { display.set(key, value); show(); };
      choices.push([key, b]);
      row.append(b);
    }
    menu.append(el("div", { class: "display-group" }, el("span", { id, class: "display-label" }, title), row));
  }

  const show = () => {
    for (const [key, b] of choices) b.setAttribute("aria-pressed", String(display.get(key) === b.dataset.value));
    button.replaceChildren(icon(display.get("theme")));
  };
  const close = () => {
    if (menu.hidden) return;
    menu.hidden = true;
    button.setAttribute("aria-expanded", "false");
  };
  const open = () => {
    show();
    menu.hidden = false;
    button.setAttribute("aria-expanded", "true");
    // Fixed under the button, kept on the screen.
    const r = button.getBoundingClientRect();
    menu.style.top = `${r.bottom + 6}px`;
    menu.style.left = `${Math.max(8, Math.min(r.right - menu.offsetWidth, window.innerWidth - menu.offsetWidth - 8))}px`;
    menu.querySelector('[aria-pressed="true"]')?.focus();
  };

  button.title = "Display options";
  button.setAttribute("aria-label", "Display options");
  button.setAttribute("aria-haspopup", "dialog");
  button.setAttribute("aria-expanded", "false");
  button.onclick = (e) => { e.stopPropagation(); menu.hidden ? open() : close(); };
  document.addEventListener("click", (e) => { if (!menu.contains(e.target)) close(); });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !menu.hidden) { close(); button.focus(); }
  });
  window.addEventListener("resize", close);
  document.body.append(menu);
  show();
}
