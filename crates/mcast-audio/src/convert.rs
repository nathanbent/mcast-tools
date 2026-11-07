//! Channel mapping and sample-rate conversion for sources feeding a stream.
//!
//! The resampler is a streaming windowed-sinc (Blackman) interpolator. Its
//! cutoff follows the lower of the two rates, so downsampling (e.g. 48 kHz
//! to 8 kHz for G.711) is properly band-limited rather than aliased.

use std::f64::consts::PI;

/// Zero crossings of the sinc kernel on each side (at the lower rate).
const ZERO_CROSSINGS: f64 = 16.0;
/// Cutoff as a fraction of the lower Nyquist frequency.
const ROLLOFF: f64 = 0.92;

pub struct Converter {
    in_channels: usize,
    out_channels: usize,
    resample: Option<Resampler>,
    mapped: Vec<f32>,
}

impl Converter {
    pub fn new(in_rate: u32, in_channels: u16, out_rate: u32, out_channels: u16) -> Self {
        Self {
            in_channels: in_channels as usize,
            out_channels: out_channels as usize,
            resample: (in_rate != out_rate)
                .then(|| Resampler::new(in_rate, out_rate, out_channels as usize)),
            mapped: Vec::new(),
        }
    }

    /// Convert interleaved `input`, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        match &mut self.resample {
            None => map_channels(input, self.in_channels, self.out_channels, out),
            Some(r) => {
                self.mapped.clear();
                map_channels(input, self.in_channels, self.out_channels, &mut self.mapped);
                r.process(&self.mapped, out);
            }
        }
    }

    /// Emit what the resampler is still holding (call at end of input).
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if let Some(r) = &mut self.resample {
            r.flush(out);
        }
    }
}

/// Downmix to mono by averaging; otherwise output channel `c` takes input `c % n`.
pub fn map_channels(input: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
    if from == to {
        out.extend_from_slice(input);
        return;
    }
    for frame in input.chunks_exact(from) {
        if to == 1 {
            out.push(frame.iter().sum::<f32>() / from as f32);
        } else {
            out.extend((0..to).map(|c| frame[c % from]));
        }
    }
}

/// Largest precomputed kernel table, in coefficients (1 MiB of f32). Rate
/// pairs that would need more fall back to evaluating the kernel directly.
const MAX_TABLE: usize = 1 << 18;

/// Polyphase resampler. Output positions are exact rationals (`base` +
/// `phase`/`phases` input frames), so there is no drift from accumulating a
/// floating-point step, and for the usual rate pairs (44.1k/48k, 48k/8k,
/// 48k/16k...) every output reuses one of a few precomputed kernel phases.
struct Resampler {
    channels: usize,
    /// Kernel half-width in input frames; each output uses `2 * half` taps.
    half: usize,
    /// Kernel phases per input frame (`to / gcd`).
    phases: usize,
    /// Input frames advanced per output, in phase units (`from / gcd`).
    advance: usize,
    /// Cutoff in cycles per input sample.
    cutoff: f64,
    /// `phases` rows of `2 * half` taps, or empty if too large to precompute.
    table: Vec<f32>,
    /// Scratch row for the direct (untabled) path.
    weights: Vec<f32>,
    /// Interleaved input history.
    buf: Vec<f32>,
    /// Next output position: input frame `base` (from the start of `buf`)
    /// plus `phase / phases`.
    base: usize,
    phase: usize,
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

impl Resampler {
    fn new(from: u32, to: u32, channels: usize) -> Self {
        let ratio = (to as f64 / from as f64).min(1.0);
        let half = (ZERO_CROSSINGS / ratio).ceil() as usize;
        let g = gcd(from as usize, to as usize);
        let (phases, advance) = (to as usize / g, from as usize / g);
        let mut r = Self {
            channels,
            half,
            phases,
            advance,
            cutoff: 0.5 * ratio * ROLLOFF,
            table: Vec::new(),
            weights: vec![0.0; 2 * half],
            // Start with `half` frames of silence so the first output is centred on input 0.
            buf: vec![0.0; half * channels],
            base: half,
            phase: 0,
        };
        let taps = 2 * half;
        if phases * taps <= MAX_TABLE {
            let mut table = vec![0.0; phases * taps];
            for (p, row) in table.chunks_exact_mut(taps).enumerate() {
                r.fill_row(p, row);
            }
            r.table = table;
        }
        r
    }

    /// Weights for an output at fractional position `phase / phases` past
    /// frame `base`; tap `i` applies to input frame `base + 1 - half + i`.
    fn fill_row(&self, phase: usize, row: &mut [f32]) {
        let frac = phase as f64 / self.phases as f64;
        for (i, w) in row.iter_mut().enumerate() {
            *w = kernel(
                self.half,
                self.cutoff,
                frac + (self.half - 1) as f64 - i as f64,
            ) as f32;
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let ch = self.channels;
        let taps = 2 * self.half;
        let frames = self.buf.len() / ch;
        while self.base + self.half < frames {
            let first = self.base + 1 - self.half;
            let weights = if self.table.is_empty() {
                let mut row = std::mem::take(&mut self.weights);
                self.fill_row(self.phase, &mut row);
                self.weights = row;
                &self.weights[..]
            } else {
                &self.table[self.phase * taps..(self.phase + 1) * taps]
            };
            let window = &self.buf[first * ch..(first + taps) * ch];
            if ch == 1 {
                out.push(dot(window, weights));
            } else {
                for c in 0..ch {
                    let acc = window[c..]
                        .iter()
                        .step_by(ch)
                        .zip(weights)
                        .map(|(x, w)| x * w)
                        .sum();
                    out.push(acc);
                }
            }
            self.phase += self.advance;
            self.base += self.phase / self.phases;
            self.phase %= self.phases;
        }
        // Drop history the next output no longer needs.
        let keep_from = (self.base + 1).saturating_sub(self.half).min(frames);
        self.buf.drain(..keep_from * ch);
        self.base -= keep_from;
    }

    fn flush(&mut self, out: &mut Vec<f32>) {
        let zeros = vec![0.0; self.half * self.channels];
        self.process(&zeros, out);
    }
}

/// Blackman-windowed sinc, `half` input frames wide on each side, scaled for
/// `cutoff` (cycles per input sample), evaluated `x` input frames from its centre.
fn kernel(half: usize, cutoff: f64, x: f64) -> f64 {
    let u = x / half as f64;
    if u.abs() >= 1.0 {
        return 0.0;
    }
    let window = 0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos();
    let arg = 2.0 * cutoff * x;
    let sinc = if arg.abs() < 1e-12 {
        1.0
    } else {
        (PI * arg).sin() / (PI * arg)
    };
    2.0 * cutoff * sinc * window
}

/// Dot product with eight independent accumulators, which the compiler can
/// keep in SIMD registers (NEON on a Raspberry Pi, SSE/AVX on x86).
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let (ac, bc) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ac
        .remainder()
        .iter()
        .zip(bc.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (x, y) in ac.zip(bc) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, secs: f64) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize)
            .map(|i| (2.0 * PI * freq * i as f64 / rate as f64).sin() as f32 * 0.5)
            .collect()
    }

    fn convert_all(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        let mut c = Converter::new(from, 1, to, 1);
        let mut out = Vec::new();
        // Feed in uneven chunks to exercise the streaming state.
        for chunk in input.chunks(337) {
            c.process(chunk, &mut out);
        }
        c.flush(&mut out);
        out
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn keeps_length_frequency_and_level() {
        for (from, to) in [
            (48_000, 8_000),
            (44_100, 48_000),
            (48_000, 16_000),
            (8_000, 48_000),
        ] {
            let out = convert_all(&sine(from, 440.0, 1.0), from, to);
            let expected = to as usize;
            assert!(
                out.len().abs_diff(expected) <= 2,
                "{from}->{to}: {} samples",
                out.len()
            );
            let mid = &out[to as usize / 10..to as usize * 9 / 10];
            let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
            let freq = crossings as f64 * to as f64 / mid.len() as f64;
            assert!((freq - 440.0).abs() < 3.0, "{from}->{to}: {freq} Hz");
            let level = rms(mid) / (0.5 / 2f64.sqrt());
            assert!((level - 1.0).abs() < 0.01, "{from}->{to}: gain {level}");
        }
    }

    #[test]
    fn rejects_aliases_when_downsampling() {
        // 6 kHz can't exist at 8 kHz; it must be filtered, not folded to 2 kHz.
        let out = convert_all(&sine(48_000, 6_000.0, 1.0), 48_000, 8_000);
        let mid = &out[800..7200];
        let atten_db = 20.0 * (rms(mid) / (0.5 / 2f64.sqrt())).log10();
        assert!(atten_db < -60.0, "only {atten_db:.1} dB");
    }

    #[test]
    fn untabled_rates_match_tabled_quality() {
        // 48000/47999 needs 47999 phases, too many to tabulate.
        assert!(Resampler::new(48_000, 47_999, 1).table.is_empty());
        let out = convert_all(&sine(48_000, 440.0, 1.0), 48_000, 47_999);
        assert!(out.len().abs_diff(47_999) <= 2);
        let level = rms(&out[4_800..43_200]) / (0.5 / 2f64.sqrt());
        assert!((level - 1.0).abs() < 0.01, "gain {level}");
    }

    #[test]
    fn stereo_channels_stay_separate() {
        // Left: 440 Hz tone, right: silence.
        let left = sine(44_100, 440.0, 0.5);
        let input: Vec<f32> = left.iter().flat_map(|&l| [l, 0.0]).collect();
        let mut c = Converter::new(44_100, 2, 48_000, 2);
        let mut out = Vec::new();
        c.process(&input, &mut out);
        c.flush(&mut out);
        let (l, r): (Vec<f32>, Vec<f32>) = out.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
        assert!(rms(&l[2_400..21_600]) > 0.3);
        assert!(r.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn maps_channels() {
        let mut out = Vec::new();
        map_channels(&[1.0, 0.0, 0.5, 0.5], 2, 1, &mut out);
        assert_eq!(out, [0.5, 0.5]);
        out.clear();
        map_channels(&[0.25, 0.75], 1, 2, &mut out);
        assert_eq!(out, [0.25, 0.25, 0.75, 0.75]);
    }
}
