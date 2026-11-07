// Recent runs, newest first (the panel keeps the last 50 that ended).

import { h, replace } from "./dom.js";
import { state, subscribe } from "./store.js";
import { runCard } from "./runs.js";

export function mount(el) {
  const render = () => {
    const runs = [...state.runs.values()].sort((a, b) => b.id - a.id);
    replace(el, runs.length
      ? h("ul", { class: "runs" }, runs.map(runCard))
      : h("p", { class: "muted" }, "Nothing played since the panel started."));
  };
  subscribe(["runs"], render);
  // "5 min ago" moves on by itself.
  setInterval(render, 30000);
  render();
}
