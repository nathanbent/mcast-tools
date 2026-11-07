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
fn map_channels(input: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
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

struct Resampler {
    channels: usize,
    /// Input samples advanced per output sample.
    step: f64,
    /// Cutoff in cycles per input sample.
    cutoff: f64,
    /// Kernel half-width in input samples.
    half: usize,
    /// Interleaved input history.
    buf: Vec<f32>,
    /// Position of the next output, in input frames from the start of `buf`.
    t: f64,
    weights: Vec<f64>,
}

impl Resampler {
    fn new(from: u32, to: u32, channels: usize) -> Self {
        let ratio = (to as f64 / from as f64).min(1.0);
        let half = (ZERO_CROSSINGS / ratio).ceil() as usize;
        Self {
            channels,
            step: from as f64 / to as f64,
            cutoff: 0.5 * ratio * ROLLOFF,
            half,
            // Start with `half` frames of silence so the first output is centred on input 0.
            buf: vec![0.0; half * channels],
            t: half as f64,
            weights: vec![0.0; 2 * half],
        }
    }

    fn kernel(&self, x: f64) -> f64 {
        let u = x / self.half as f64;
        if u.abs() >= 1.0 {
            return 0.0;
        }
        let window = 0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos();
        let arg = 2.0 * self.cutoff * x;
        let sinc = if arg.abs() < 1e-12 {
            1.0
        } else {
            (PI * arg).sin() / (PI * arg)
        };
        2.0 * self.cutoff * sinc * window
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let frames = self.buf.len() / self.channels;
        let ch = self.channels;
        while (self.t.floor() as usize) + self.half < frames {
            let base = self.t.floor() as usize;
            let first = base + 1 - self.half;
            for (i, w) in (0..2 * self.half).zip(0..) {
                self.weights[w] = self.kernel(self.t - (first + i) as f64);
            }
            for c in 0..ch {
                let mut acc = 0.0f64;
                for (i, w) in self.weights.iter().enumerate() {
                    acc += self.buf[(first + i) * ch + c] as f64 * w;
                }
                out.push(acc as f32);
            }
            self.t += self.step;
        }
        // Drop history the next output no longer needs.
        let keep_from = (self.t.floor() as usize + 1)
            .saturating_sub(self.half)
            .min(frames);
        self.buf.drain(..keep_from * ch);
        self.t -= keep_from as f64;
    }

    fn flush(&mut self, out: &mut Vec<f32>) {
        let zeros = vec![0.0; self.half * self.channels];
        self.process(&zeros, out);
    }
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
    fn maps_channels() {
        let mut out = Vec::new();
        map_channels(&[1.0, 0.0, 0.5, 0.5], 2, 1, &mut out);
        assert_eq!(out, [0.5, 0.5]);
        out.clear();
        map_channels(&[0.25, 0.75], 1, 2, &mut out);
        assert_eq!(out, [0.25, 0.25, 0.75, 0.75]);
    }
}
