// Record a sound from the browser's microphone, listen to it, and save it
// to the Sounds folder shown. 48 kHz mono 16-bit WAV, as push to talk sends.

import { upload } from "./api.js";
import { h, replace, toast, seconds } from "./dom.js";
import { micProblem } from "./talk.js";
import { field } from "./editor.js";

const RATE = 48000;
const MAX_S = 300; // five minutes

// 16-bit mono PCM chunks as a WAV file.
function wav(chunks) {
  const samples = chunks.reduce((n, c) => n + c.length, 0);
  const buf = new ArrayBuffer(44 + samples * 2);
  const v = new DataView(buf);
  const text = (at, s) => [...s].forEach((ch, i) => v.setUint8(at + i, ch.charCodeAt(0)));
  text(0, "RIFF"); v.setUint32(4, 36 + samples * 2, true); text(8, "WAVE");
  text(12, "fmt "); v.setUint32(16, 16, true); v.setUint16(20, 1, true); v.setUint16(22, 1, true);
  v.setUint32(24, RATE, true); v.setUint32(28, RATE * 2, true); v.setUint16(32, 2, true); v.setUint16(34, 16, true);
  text(36, "data"); v.setUint32(40, samples * 2, true);
  let at = 44;
  for (const c of chunks) {
    new Int16Array(buf, at, c.length).set(c);
    at += c.length * 2;
  }
  return new Blob([buf], { type: "audio/wav" });
}

function defaultName() {
  const d = new Date();
  const p = (n) => String(n).padStart(2, "0");
  return `recording-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}.wav`;
}

// Open the recorder; `folder` is where it saves; `exists(name)` says
// whether that file is there already; `saved()` after saving.
export function openRecorder(folder, exists, saved) {
  const problem = micProblem();
  if (problem) return toast(problem, "error");
  const d = h("dialog", { class: "wide", "aria-label": "Record a sound" });
  document.body.append(d);
  let rec = null; // { ctx, stream, chunks, started, timer }
  let blob = null;
  let url = null;

  const meter = h("div", { class: "meter" }, h("span"));
  const time = h("span", { class: "mono" }, "0:00");
  const status = h("p", { class: "muted small" }, "Press Record, speak, then Stop. Up to five minutes.");
  const player = h("audio", { controls: true, hidden: true });
  const name = h("input", { value: defaultName(), required: true, maxLength: 255 });
  const recordBtn = h("button", { type: "button", class: "primary" }, "Record");
  const saveBtn = h("button", { type: "submit", class: "primary", disabled: true }, "Save");

  const level = (peak) => { meter.firstChild.style.width = `${Math.min(100, Math.round(peak * 100))}%`; };

  async function start() {
    blob = null;
    player.hidden = true;
    saveBtn.disabled = true;
    const r = { ctx: new AudioContext(), stream: null, chunks: [], started: performance.now(), timer: null };
    rec = r;
    recordBtn.textContent = "Stop";
    status.textContent = "Starting the microphone…";
    try {
      r.stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: false, noiseSuppression: true, autoGainControl: true },
      });
      if (r.ctx.state !== "running") await r.ctx.resume();
      await r.ctx.audioWorklet.addModule("js/mic-worklet.js");
      if (rec !== r) return close(r);
      const node = new AudioWorkletNode(r.ctx, "mic-capture");
      node.port.onmessage = ({ data }) => {
        if (rec !== r) return;
        r.chunks.push(new Int16Array(data.pcm));
        level(data.peak);
      };
      r.ctx.createMediaStreamSource(r.stream).connect(node);
      r.started = performance.now();
      status.textContent = "Recording…";
      r.timer = setInterval(() => {
        const s = (performance.now() - r.started) / 1000;
        time.textContent = seconds(s);
        if (s >= MAX_S) stop();
      }, 200);
    } catch (e) {
      const why = {
        NotAllowedError: "The browser wasn't allowed to use the microphone (check the site's permissions).",
        NotFoundError: "No microphone was found.",
        NotReadableError: "The microphone is in use by another program, or couldn't be opened.",
      }[e?.name] ?? `Couldn't record: ${e?.message ?? e}`;
      toast(why, "error");
      rec = null;
      close(r);
      recordBtn.textContent = "Record";
      status.textContent = "";
    }
  }

  function close(r) {
    clearInterval(r.timer);
    r.stream?.getTracks().forEach((t) => t.stop());
    r.ctx.close().catch(() => {});
  }

  function stop() {
    const r = rec;
    if (!r) return;
    rec = null;
    close(r);
    level(0);
    recordBtn.textContent = "Record again";
    const samples = r.chunks.reduce((n, c) => n + c.length, 0);
    if (samples < RATE / 10) {
      status.textContent = "Nothing was recorded.";
      return;
    }
    blob = wav(r.chunks);
    if (url) URL.revokeObjectURL(url);
    url = URL.createObjectURL(blob);
    player.src = url;
    player.hidden = false;
    time.textContent = seconds(samples / RATE);
    status.textContent = "Listen back, then save it, or record again.";
    saveBtn.disabled = false;
  }

  recordBtn.onclick = () => (rec ? stop() : start());
  d.addEventListener("close", () => {
    if (rec) { close(rec); rec = null; }
    if (url) URL.revokeObjectURL(url);
    d.remove();
  });

  replace(d, h("form", {
    onsubmit: async (e) => {
      e.preventDefault();
      if (!blob) return;
      let n = name.value.trim();
      if (n.includes("/")) return toast("A name can't contain '/'.", "error");
      if (!/\.wav$/i.test(n)) n += ".wav";
      const full = folder ? `${folder}/${n}` : n;
      if (exists(full) && !confirm(`Replace ${full}?`)) return;
      saveBtn.disabled = true;
      try {
        await upload(full, blob);
        toast(`Saved ${full}`, "ok");
        d.close();
        saved();
      } catch (err) {
        toast(`Couldn't save: ${err.message}`, "error");
        saveBtn.disabled = false;
      }
    },
  },
    h("div", { class: "dialog-head" }, h("h2", {}, "Record a sound"),
      h("button", { type: "button", class: "close", "aria-label": "Close", onclick: () => d.close() }, "×")),
    h("div", { class: "recorder" }, recordBtn, meter, time),
    status,
    player,
    field("Name", name, `Saved in ${folder || "the top folder"}`),
    h("div", { class: "buttons" }, saveBtn, h("button", { type: "button", onclick: () => d.close() }, "Cancel"))));
  d.showModal();
}
