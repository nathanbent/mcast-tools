// The display page (?display=1&zones=a,b): a full-screen sign that shows
// the text pages of what plays to its zones, as the playlist reaches them,
// and the time while there's nothing to show.

import { h, replace } from "./dom.js";
import { state, subscribe, zoneName } from "./store.js";

// The page to show now: the newest one still up.
const current = () => {
  const now = Date.now();
  return state.pages.find((p) => p.until_ms == null || p.until_ms > now) ?? null;
};

export function mount(el) {
  const clock = () => new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const where = () => (state.view.zones.length ? state.view.zones.map(zoneName).join(", ") : "All zones");
  let shown = undefined;
  let timer = null;
  const draw = () => {
    const page = current();
    const key = page ? `${page.run}:${page.at_ms}` : null;
    if (key !== shown) {
      shown = key;
      if (page) {
        replace(el, h("div", { class: "display-page", role: "status", "aria-live": "assertive" },
          h("p", { class: "display-from" }, page.name),
          page.text.map((line) => h("p", { class: "display-text" }, line))));
      } else {
        replace(el, h("div", { class: "display-idle" },
          h("p", { class: "display-clock" }, clock()),
          h("p", { class: "display-from" }, where())));
      }
    } else if (!page) {
      el.querySelector(".display-clock").textContent = clock();
      el.querySelector(".display-from").textContent = where();
    }
    // Again when this page comes down, or for the clock.
    clearTimeout(timer);
    const until = page?.until_ms;
    timer = setTimeout(draw, until ? Math.max(50, Math.min(until - Date.now() + 20, 1000)) : 1000);
  };
  subscribe(["display", "config"], draw);
  draw();
}
