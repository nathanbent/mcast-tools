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
  const t = { id, name, phase: "starting", level: 0, latched, ws: null, stream: null, ctx: null, ended: false, heard: 0 };
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
    // The microphone first: the page (and its chime) only starts once audio
    // is really coming, rather than chiming around silence.
    const node = await capture(t);
    if (t.ended) return finish(t);
    const early = [];
    node.port.onmessage = ({ data }) => {
      if (t.ws?.readyState === WebSocket.OPEN) {
        if (t.phase !== "ending") t.ws.send(data.pcm);
      } else if (early.length < EARLY_MAX) {
        early.push(data.pcm); // said while connecting: plays after the chime
      }
      t.heard = performance.now();
      t.level = data.peak;
      tell();
    };
    await firstAudio(t);
    if (t.ended) return finish(t);
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const tk = token();
    t.ws = new WebSocket(`${proto}://${location.host}/api/intercom/v1/live/${encodeURIComponent(id)}${tk ? `?access_token=${encodeURIComponent(tk)}` : ""}`);
    t.ws.binaryType = "arraybuffer";
    t.ws.onopen = () => {
      for (const pcm of early.splice(0)) t.ws.send(pcm);
    };
    t.ws.onmessage = (e) => {
      let m;
      try { m = JSON.parse(e.data); } catch { return; }
      if (m.type === "started") {
        t.phase = "live";
        tell();
        watchAudio(t);
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
  } catch (e) {
    console.error("push to talk:", e);
    const why = {
      NoAudio: "The microphone isn't giving any audio, so nothing was paged. Check the browser's microphone permission and input device, then try again.",
      NotAllowedError: "The browser wasn't allowed to use the microphone (check the site's permissions).",
      NotFoundError: "No microphone was found.",
      NotReadableError: "The microphone is in use by another program, or couldn't be opened.",
    }[e?.name] ?? `Couldn't start talking: ${e?.name ?? ""} ${e?.message ?? e}`;
    toast(why, "error");
    finish(t);
  }
}

let promptHinted = false;

// Chunks (20 ms each) kept while the socket connects: a few seconds' worth.
const EARLY_MAX = 250;
// How long the microphone has to start giving audio.
const FIRST_AUDIO_MS = 3000;
// Silence from the microphone this long during a page is worth saying.
const STALL_MS = 3000;

// The capture graph: microphone → worklet (20 ms chunks of PCM) → a muted
// gain → the speakers. Nothing is heard, but the connection to the speakers
// matters: some browsers (Safari) only run nodes that lead to them.
async function capture(t) {
  await t.ctx.audioWorklet.addModule("js/mic-worklet.js");
  let src;
  try {
    src = t.ctx.createMediaStreamSource(t.stream);
  } catch (e) {
    // Firefox can't connect a microphone whose rate differs from the
    // context's (a headset at 16 kHz, say): make one at the microphone's rate.
    const rate = t.stream.getAudioTracks()[0]?.getSettings().sampleRate;
    if (e?.name !== "NotSupportedError" || !rate) throw e;
    console.warn(`push to talk: the microphone runs at ${rate} Hz; using an AudioContext at that rate`);
    t.ctx.close().catch(() => {});
    t.ctx = new AudioContext({ sampleRate: rate });
    if (t.ctx.state !== "running") await t.ctx.resume();
    await t.ctx.audioWorklet.addModule("js/mic-worklet.js");
    src = t.ctx.createMediaStreamSource(t.stream);
  }
  const node = new AudioWorkletNode(t.ctx, "mic-capture");
  const mute = t.ctx.createGain();
  mute.gain.value = 0;
  src.connect(node);
  node.connect(mute).connect(t.ctx.destination);
  return node;
}

// Wait for the first chunk from the microphone; fail if none comes.
function firstAudio(t) {
  return new Promise((resolve, reject) => {
    const started = performance.now();
    const look = () => {
      if (t.heard || t.ended) return resolve();
      if (performance.now() - started > FIRST_AUDIO_MS) {
        const track = t.stream.getAudioTracks()[0];
        console.error("push to talk: no audio from the microphone", {
          context: t.ctx.state,
          rate: t.ctx.sampleRate,
          track: track && { label: track.label, state: track.readyState, muted: track.muted, enabled: track.enabled },
        });
        const e = new Error("no audio from the microphone");
        e.name = "NoAudio";
        return reject(e);
      }
      setTimeout(look, 50);
    };
    look();
  });
}

// During a page: say so once if the microphone stops giving audio (the
// panel ends a page that gets none for 20 seconds).
function watchAudio(t) {
  let warned = false;
  const timer = setInterval(() => {
    if (t.ended || current !== t) return clearInterval(timer);
    const quiet = performance.now() - t.heard;
    if (quiet > STALL_MS && !warned && t.phase === "live") {
      warned = true;
      console.warn("push to talk: the microphone has stopped giving audio", t.ctx.state);
      toast(`"${t.name}": no audio is coming from the microphone. The page ends after 20 seconds without any.`, "error");
    } else if (quiet < STALL_MS) {
      warned = false;
    }
  }, 500);
}

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
