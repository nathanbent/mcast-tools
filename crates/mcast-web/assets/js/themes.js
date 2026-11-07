// The theme switch in the header: System → Light → Dark, kept in this
// browser (js/theme.js applies it before the page draws).

const KEY = "mcast.theme";
const NAMES = { system: "System theme", light: "Light theme", dark: "Dark theme" };
const ICONS = { system: "◐", light: "☀", dark: "☾" };

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
    button.textContent = ICONS[t];
    button.title = `${NAMES[t]} (click for ${NAMES[next].toLowerCase()})`;
    button.setAttribute("aria-label", button.title);
  };
  button.onclick = () => {
    setTheme({ system: "light", light: "dark", dark: "system" }[currentTheme()]);
    show();
  };
  show();
}
