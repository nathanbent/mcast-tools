// AudioWorklet: the microphone, resampled to 48 kHz mono and posted as
// 20 ms chunks of 16-bit little-endian PCM, with the chunk's peak level.
// (The page's AudioContext runs at whatever rate the browser likes.)

const OUT_RATE = 48000;
const CHUNK = 960; // 20 ms at 48 kHz

class MicCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.step = sampleRate / OUT_RATE; // input samples per output sample
    this.pos = 0; // position in the input, relative to `last`
    this.last = 0; // the input sample before this block
    this.out = new Int16Array(CHUNK);
    this.n = 0;
    this.peak = 0;
  }

  process(inputs) {
    const channels = inputs[0];
    if (!channels || channels.length === 0) return true;
    const len = channels[0].length;
    // Mono: the average of the channels.
    const mono = new Float32Array(len);
    for (const ch of channels) for (let i = 0; i < len; i++) mono[i] += ch[i] / channels.length;
    // Linear interpolation between input samples (-1 is `last`).
    while (this.pos < len - 1) {
      const i = Math.floor(this.pos);
      const f = this.pos - i;
      const a = i < 0 ? this.last : mono[i];
      const b = mono[i + 1];
      const s = Math.max(-1, Math.min(1, a + (b - a) * f));
      this.peak = Math.max(this.peak, Math.abs(s));
      this.out[this.n++] = s < 0 ? s * 0x8000 : s * 0x7fff;
      if (this.n === CHUNK) {
        const chunk = this.out.slice();
        this.port.postMessage({ pcm: chunk.buffer, peak: this.peak }, [chunk.buffer]);
        this.n = 0;
        this.peak = 0;
      }
      this.pos += this.step;
    }
    this.last = mono[len - 1];
    this.pos -= len;
    return true;
  }
}

registerProcessor("mic-capture", MicCapture);
