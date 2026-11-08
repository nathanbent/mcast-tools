// Buttons and contacts on GPIO pins (a Pi's header), each playing an
// announcement: a panic button, a door contact, a bell push. Shown with
// each pin's state now, and a Test button.

import { api } from "./api.js";
import { h, toast, ago } from "./dom.js";
import { state, refreshInputs } from "./store.js";
import { mountEditor, field, select, numberInput, num } from "./editor.js";

// A Pi's 40-pin header: BCM GPIO number → physical pin.
const HEADER = { 2: 3, 3: 5, 4: 7, 17: 11, 27: 13, 22: 15, 10: 19, 9: 21, 11: 23, 0: 27, 5: 29, 6: 31, 13: 33,
  19: 35, 26: 37, 14: 8, 15: 10, 18: 12, 23: 16, 24: 18, 25: 22, 8: 24, 7: 26, 1: 28, 12: 32, 16: 36, 20: 38, 21: 40 };

const chipOf = (i) => i.chip || "gpiochip0";

function pinLabel(i) {
  const pin = chipOf(i) === "gpiochip0" && HEADER[i.line];
  return pin ? `GPIO${i.line} (pin ${pin})` : `${chipOf(i)} line ${i.line}`;
}

function stateBadge(i) {
  const s = state.inputs.find((x) => x.id === i.id);
  if (i.enabled === false) return h("span", { class: "badge" }, "off");
  if (!s) return h("span", { class: "muted small" }, "…");
  if (s.error) return h("span", { class: "badge failed", title: s.error }, "can't read");
  if (s.pressed == null) return h("span", { class: "muted small" }, "…");
  return h("span", { title: s.changed_ms ? `changed ${ago(s.changed_ms)}` : "" },
    h("span", { class: `badge ${s.pressed ? "playing" : ""}` }, s.pressed ? "pressed" : "released"));
}

async function test(i) {
  try {
    const v = await api("POST", `/inputs/${encodeURIComponent(i.id)}/test`);
    toast(`"${i.name}" pressed${v.run ? "" : " (its announcement is still playing)"}`, "ok");
  } catch (e) {
    toast(`Couldn't test "${i.name}": ${e.message}`, "error");
  }
}

function form(i, save, cancel) {
  const anns = state.config.announcements.filter((a) => a.audio !== "live");
  const name = h("input", { value: i?.name ?? "", required: true, maxLength: 64, placeholder: "e.g. Panic button" });
  const ann = select(anns.map((a) => [a.id, a.name]), i?.announcement ?? anns[0]?.id ?? "");
  const line = numberInput(i?.line, { min: 0, max: 1023, step: 1, required: true, placeholder: "17" });
  const chip = h("input", { value: i?.chip ?? "", placeholder: "gpiochip0" });
  const hint = h("span");
  const showPin = () => {
    const n = num(line);
    const pin = (chip.value.trim() || "gpiochip0") === "gpiochip0" && HEADER[n];
    hint.textContent = n == null ? "" : pin ? `GPIO${n} is header pin ${pin}` : "Not a header pin on a Pi";
  };
  line.addEventListener("input", showPin);
  chip.addEventListener("input", showPin);
  showPin();
  const pressedWhen = select([["low", "Low (to ground)"], ["high", "High (to 3.3 V)"]],
    i?.active_low === false ? "high" : "low");
  const bias = select([["pull_up", "Pull-up"], ["pull_down", "Pull-down"], ["none", "None (driven from outside)"]],
    i?.bias ?? "pull_up");
  pressedWhen.addEventListener("change", () => { bias.value = pressedWhen.value === "high" ? "pull_down" : "pull_up"; });
  const debounce = numberInput(i?.debounce_ms, { min: 0, max: 1000, step: 1, placeholder: "50" });
  const held = h("input", { type: "checkbox", checked: !!i?.while_held });
  const enabled = h("input", { type: "checkbox", checked: i?.enabled !== false });
  return h("form", {
    onsubmit: (e) => {
      e.preventDefault();
      save({
        name: name.value.trim(), announcement: ann.value, line: num(line),
        chip: chip.value.trim() || null, active_low: pressedWhen.value === "low", bias: bias.value,
        debounce_ms: num(debounce), while_held: held.checked, enabled: enabled.checked,
      });
    },
  },
    h("div", { class: "row" }, field("Name", name), field("Plays", ann)),
    !anns.length && h("p", { class: "warn small" }, "Make an announcement first (not a live one)."),
    h("div", { class: "row" },
      field("GPIO line", line, hint), field("Chip", chip, "gpiochip0 is a Pi's header"),
      field("Pressed when the pin is", pressedWhen), field("Pull resistor", bias),
      field("Debounce (ms)", debounce, "Ignores switch bounce; 50 unless set")),
    h("label", { class: "check" }, held, " Play while held: stop when let go (otherwise a press plays it through)"),
    h("label", { class: "check" }, enabled, " On"),
    h("div", { class: "buttons" },
      h("button", { type: "submit", class: "primary" }, "Save"),
      h("button", { type: "button", onclick: cancel }, "Cancel")));
}

export function mount() {
  const el = h("div");
  refreshInputs();
  const annName = (id) => state.config.announcements.find((a) => a.id === id)?.name ?? id;
  mountEditor(el, {
    kind: "input",
    path: "/inputs",
    parts: ["inputs"],
    items: () => state.config.inputs ?? [],
    before: () => h("p", { class: "muted" },
      "Buttons and contacts wired to the panel's own GPIO pins (on a Raspberry Pi, its header) play an announcement when pressed. Pressing again while it plays does nothing; a contact that's already closed when the panel starts doesn't page."),
    columns: [
      ["Name", (i) => i.name],
      ["Pin", (i) => pinLabel(i), true],
      ["Plays", (i) => [annName(i.announcement), i.while_held && h("span", { class: "muted small" }, " · while held")]],
      ["Now", (i) => stateBadge(i)],
      ["", (i) => i.enabled !== false && h("button", { onclick: () => test(i), title: "Press it from here" }, "Test")],
    ],
    form,
  });
  return el;
}
