// Push to talk: the microphone, streamed to the panel for a live
// announcement. Hold the button to talk and let go to end, or tap it to
// talk hands-free and tap again to end.
//
// Browsers only allow the microphone on HTTPS pages (or http://localhost).

import { token } from "./api.js";
import { toast } from "./dom.js";

// Only one page at a time from this browser.
let current = null;
const listeners = [];

// `fn(state)` hears about changes: state is null (idle) or
// { id, phase: "starting" | "live" | "ending", level: 0..1, latched }.
export function onTalk(fn) {
  listeners.push(fn);
}

function tell() {
  const s = current && { id: current.id, phase: current.phase, level: current.level, latched: current.latched };
  for (const fn of listeners) fn(s);
}

export const talking = () => current?.id ?? null;

// Why this page can't use the microphone, or null if it can.
export function micProblem() {
  if (!window.isSecureContext) {
    return "The microphone only works on an HTTPS page. Start the panel with --tls-self-signed (or a certificate), or open it as http://localhost on this machine.";
  }
  if (!navigator.mediaDevices?.getUserMedia) return "This browser can't use a microphone.";
  if (!window.AudioWorkletNode) return "This browser is too old for live pages.";
  return null;
}

// Start talking on announcement `id` (its name for messages).
export async function start(id, name, latched = false) {
  if (current) return;
  const problem = micProblem();
  if (problem) {
    toast(problem, "error");
    return;
  }
  const t = { id, name, phase: "starting", level: 0, latched, ws: null, stream: null, ctx: null, ended: false };
  current = t;
  tell();
  try {
    // Made here, during the click: made later (after the permission prompt)
    // a browser may keep it suspended, and no audio would flow.
    t.ctx = new AudioContext();
    const asked = performance.now();
    t.stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    if (t.ended) return finish(t);
    // Slow: the browser asked. Firefox asks every time unless told to remember.
    if (performance.now() - asked > 1500 && !promptHinted) {
      promptHinted = true;
      toast("Tip: tick \"Remember this decision\" when the browser asks about the microphone, so it doesn't ask every time.", "info");
    }
    if (t.ctx.state !== "running") await t.ctx.resume();
    await t.ctx.audioWorklet.addModule("js/mic-worklet.js");
    if (t.ended) return finish(t);
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const tk = token();
    t.ws = new WebSocket(`${proto}://${location.host}/api/intercom/v1/live/${encodeURIComponent(id)}${tk ? `?access_token=${encodeURIComponent(tk)}` : ""}`);
    t.ws.binaryType = "arraybuffer";
    t.ws.onmessage = (e) => {
      let m;
      try { m = JSON.parse(e.data); } catch { return; }
      if (m.type === "started") {
        t.phase = "live";
        tell();
      } else if (m.type === "ended") {
        if (m.reason && m.reason !== "stopped") toast(`"${name}": ${m.reason}`, "error");
        finish(t);
      }
    };
    t.ws.onclose = (e) => {
      // Closed before it started: say so, rather than silently going idle.
      if (t.phase === "starting" && !t.ended) {
        console.error("push to talk: the panel closed the socket", e.code, e.reason);
        toast(`Couldn't start "${name}": the panel closed the connection${e.reason ? ` (${e.reason})` : ""}. Is the panel's address the one in the browser, and the token right?`, "error");
      }
      finish(t);
    };
    t.ws.onerror = (e) => console.error("push to talk: WebSocket error", e);
    // Audio flows from the start; what comes during the chime plays after it.
    const src = t.ctx.createMediaStreamSource(t.stream);
    const node = new AudioWorkletNode(t.ctx, "mic-capture");
    node.port.onmessage = ({ data }) => {
      if (t.ws.readyState === WebSocket.OPEN && t.phase !== "ending") t.ws.send(data.pcm);
      t.level = data.peak;
      tell();
    };
    src.connect(node);
  } catch (e) {
    console.error("push to talk:", e);
    const why = {
      NotAllowedError: "The browser wasn't allowed to use the microphone (check the site's permissions).",
      NotFoundError: "No microphone was found.",
      NotReadableError: "The microphone is in use by another program, or couldn't be opened.",
    }[e?.name] ?? `Couldn't start talking: ${e?.name ?? ""} ${e?.message ?? e}`;
    toast(why, "error");
    finish(t);
  }
}

let promptHinted = false;

// Stop talking: what's said so far plays out, then the end chime.
export function stop() {
  const t = current;
  if (!t) return;
  if (t.ws?.readyState === WebSocket.OPEN) {
    t.phase = "ending";
    t.ws.send("end");
    tell();
    // The panel closes the socket once it has passed the end on.
    setTimeout(() => finish(t), 3000);
  } else {
    t.ended = true;
    finish(t);
  }
}

function finish(t) {
  t.ended = true;
  t.stream?.getTracks().forEach((tr) => tr.stop());
  t.ctx?.close().catch(() => {});
  if (t.ws && t.ws.readyState <= WebSocket.OPEN) t.ws.close();
  if (current === t) {
    current = null;
    tell();
  }
}

// The press in progress: which announcement, and since when. Kept here
// rather than on the button, which the panel may redraw mid-press.
let press = null;

function released() {
  if (!press) return;
  const { id, at } = press;
  press = null;
  if (current?.id !== id) return;
  // A tap, or let go while still starting (the permission prompt took the
  // mouse): talk hands-free until pressed again.
  if (performance.now() - at < 350 || current.phase === "starting") {
    current.latched = true;
    tell();
  } else {
    stop(); // held: let go to end
  }
}
window.addEventListener("pointerup", released);
window.addEventListener("pointercancel", released);
// Leaving the page ends any page in progress.
window.addEventListener("pagehide", () => stop());

// Wire a button for hold-to-talk and tap-to-latch: pressed briefly, it
// keeps talking until pressed again; held, it talks until let go.
export function pushToTalk(button, id, name) {
  button.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    if (current?.id === id) {
      // Pressed again: ends a hands-free page.
      if (current.latched) stop();
      return;
    }
    if (current) return toast("Already talking on another page", "info");
    press = { id, at: performance.now() };
    start(id, name);
  });
  // No long-press menu on touch screens.
  button.addEventListener("contextmenu", (e) => e.preventDefault());
  // Keyboard: Enter or Space starts and ends.
  button.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" && e.key !== " ") return;
    e.preventDefault();
    if (current?.id === id) stop();
    else if (!current) start(id, name, true);
  });
}
