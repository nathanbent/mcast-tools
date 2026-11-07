//! Compressor (port of ffmpeg's `acompressor`) and a lookahead peak limiter.

use super::db_to_lin;
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub struct CompressorParams {
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    /// Linear makeup gain, as in ffmpeg (`makeup=3` is x3, about +9.5 dB).
    pub makeup: f64,
}

/// RMS-detecting, channel-linked (average) soft-knee compressor with the same
/// detector, knee and gain law as ffmpeg's `acompressor` defaults
/// (`detection=rms`, `link=average`, `knee=2.82843`).
pub struct Compressor {
    channels: usize,
    ratio: f64,
    thres: f64,
    knee_start: f64,
    knee_stop: f64,
    compressed_knee_stop: f64,
    adj_knee_start: f64,
    attack_coeff: f64,
    release_coeff: f64,
    makeup: f64,
    lin_slope: f64,
}

const KNEE: f64 = 2.828_43;

impl Compressor {
    pub fn new(p: CompressorParams, rate: u32, channels: usize) -> Self {
        let threshold = db_to_lin(p.threshold_db);
        let lin_knee_start = threshold / KNEE.sqrt();
        let lin_knee_stop = threshold * KNEE.sqrt();
        let thres = threshold.ln();
        let knee_start = lin_knee_start.ln();
        let knee_stop = lin_knee_stop.ln();
        Self {
            channels,
            ratio: p.ratio,
            thres,
            knee_start,
            knee_stop,
            compressed_knee_stop: (knee_stop - thres) / p.ratio + thres,
            adj_knee_start: lin_knee_start * lin_knee_start,
            attack_coeff: (1.0 / (p.attack_ms * rate as f64 / 4000.0)).min(1.0),
            release_coeff: (1.0 / (p.release_ms * rate as f64 / 4000.0)).min(1.0),
            makeup: p.makeup,
            lin_slope: 0.0,
        }
    }

    fn output_gain(&self) -> f64 {
        let slope = self.lin_slope.ln() * 0.5; // RMS detection
        let mut gain = (slope - self.thres) / self.ratio + self.thres;
        let delta = 1.0 / self.ratio;
        if KNEE > 1.0 && slope < self.knee_stop {
            gain = hermite(
                slope,
                self.knee_start,
                self.knee_stop,
                self.knee_start,
                self.compressed_knee_stop,
                1.0,
                delta,
            );
        }
        (gain - slope).exp()
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(self.channels) {
            let mut abs = frame.iter().map(|s| s.abs() as f64).sum::<f64>() / self.channels as f64;
            abs *= abs;
            let coeff = if abs > self.lin_slope {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.lin_slope += (abs - self.lin_slope) * coeff;
            let gain = if self.lin_slope > 0.0 && self.lin_slope > self.adj_knee_start {
                self.output_gain()
            } else {
                1.0
            };
            for s in frame {
                *s = (*s as f64 * gain * self.makeup) as f32;
            }
        }
    }
}

fn hermite(x: f64, x0: f64, x1: f64, p0: f64, p1: f64, m0: f64, m1: f64) -> f64 {
    let width = x1 - x0;
    let t = (x - x0) / width;
    let (m0, m1) = (m0 * width, m1 * width);
    let ct2 = -3.0 * p0 - 2.0 * m0 + 3.0 * p1 - m1;
    let ct3 = 2.0 * p0 + m0 - 2.0 * p1 + m1;
    ct3 * t * t * t + ct2 * t * t + m0 * t + p0
}

/// Lookahead peak limiter. Delays audio by the lookahead so gain reduction
/// starts before a peak arrives, then releases smoothly.
pub struct Limiter {
    channels: usize,
    ceiling: f64,
    lookahead: usize,
    true_peak: bool,
    delay: VecDeque<f32>,
    /// Monotonic deque of (frame index, required gain) for a sliding minimum.
    window: VecDeque<(u64, f64)>,
    /// Last 3 input samples per channel, for inter-sample peak estimation.
    history: Vec<[f32; 3]>,
    frame: u64,
    gain: f64,
    attack_coeff: f64,
    release_coeff: f64,
    pending_in: u64,
    out_count: u64,
}

impl Limiter {
    /// `true_peak` estimates peaks between samples (4x, Catmull-Rom), like a
    /// dBTP meter; otherwise sample peaks are used (like ffmpeg `alimiter`).
    pub fn new(ceiling: f64, rate: u32, channels: usize, true_peak: bool) -> Self {
        let lookahead = ((rate as f64 * 0.005) as usize).max(1); // 5 ms
        Self {
            channels,
            ceiling,
            lookahead,
            true_peak,
            delay: VecDeque::with_capacity((lookahead + 1) * channels),
            window: VecDeque::new(),
            history: vec![[0.0; 3]; channels],
            frame: 0,
            gain: 1.0,
            attack_coeff: 1.0 - 0.01f64.powf(1.0 / lookahead as f64),
            release_coeff: 1.0 - (-1.0 / (rate as f64 * 0.05)).exp(), // 50 ms
            pending_in: 0,
            out_count: 0,
        }
    }

    fn peak(&mut self, frame: &[f32]) -> f64 {
        let mut peak = 0f64;
        for (c, &x) in frame.iter().enumerate() {
            let [a, b, c1] = self.history[c];
            peak = peak.max(x.abs() as f64);
            if self.true_peak {
                // Peaks between the previous two samples (b..c1), from a..x.
                for t in [0.25f32, 0.5, 0.75] {
                    let v = catmull_rom(a, b, c1, x, t);
                    peak = peak.max(v.abs() as f64);
                }
            }
            self.history[c] = [b, c1, x];
        }
        peak
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for frame in input.chunks_exact(self.channels) {
            let peak = self.peak(frame);
            let required = if peak > self.ceiling {
                self.ceiling / peak
            } else {
                1.0
            };
            while self.window.back().is_some_and(|&(_, g)| g >= required) {
                self.window.pop_back();
            }
            self.window.push_back((self.frame, required));
            // Keep one extra frame: the inter-sample estimate lags by a sample.
            while self
                .window
                .front()
                .is_some_and(|&(i, _)| i + self.lookahead as u64 + 1 < self.frame)
            {
                self.window.pop_front();
            }
            self.frame += 1;
            self.delay.extend(frame.iter().copied());
            self.pending_in += 1;

            let target = self.window.front().map_or(1.0, |&(_, g)| g);
            let coeff = if target < self.gain {
                self.attack_coeff
            } else {
                self.release_coeff
            };
            self.gain += (target - self.gain) * coeff;

            if self.delay.len() > self.lookahead * self.channels {
                for _ in 0..self.channels {
                    let s = self.delay.pop_front().unwrap() as f64 * self.gain;
                    let c = self.ceiling;
                    out.push(s.clamp(-c, c) as f32);
                }
                self.out_count += 1;
            }
        }
    }

    /// Push out the delayed audio.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let missing = self.pending_in.saturating_sub(self.out_count) as usize;
        let zeros = vec![0.0; missing * self.channels];
        self.process(&zeros, out);
        self.pending_in -= missing as u64;
    }
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    0.5 * (2.0 * p1
        + (-p0 + p2) * t
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t * t
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t * t * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::lin_to_db;

    fn sine(rate: u32, freq: f64, amp: f64, secs: f64) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize)
            .map(|i| {
                (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Output RMS of a 1 kHz sine through ffmpeg's `acompressor` with the
    /// script's voice and music settings (measured with ffmpeg's `astats`).
    #[test]
    fn compressor_matches_ffmpeg() {
        let voice = CompressorParams {
            threshold_db: -20.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            makeup: 1.0,
        };
        let music = CompressorParams {
            threshold_db: -24.0,
            ratio: 2.0,
            attack_ms: 20.0,
            release_ms: 250.0,
            makeup: 2.0,
        };
        let cases = [
            (voice, -6.0, -18.366080),
            (voice, -10.0, -19.366080),
            (voice, -30.0, -30.001927),
            (music, -6.0, -10.093136),
            (music, -10.0, -12.093136),
            (music, -30.0, -23.996449),
        ];
        for (p, in_db, ffmpeg_db) in cases {
            let mut c = Compressor::new(p, 48_000, 1);
            let mut x = sine(48_000, 1000.0, db_to_lin(in_db + 3.0103), 2.0); // RMS = in_db
            c.process(&mut x);
            let out_db = lin_to_db(rms(&x[48_000..]));
            assert!(
                (out_db - ffmpeg_db).abs() < 0.01,
                "{p:?} {in_db} dB: {out_db}, ffmpeg {ffmpeg_db}"
            );
        }
    }

    #[test]
    fn limiter_holds_ceiling_and_preserves_length() {
        let mut l = Limiter::new(0.5, 48_000, 2, true);
        let input: Vec<f32> = sine(48_000, 3000.0, 0.9, 0.5)
            .iter()
            .flat_map(|&s| [s, -s])
            .collect();
        let mut out = Vec::new();
        for chunk in input.chunks(998) {
            l.process(chunk, &mut out);
        }
        l.flush(&mut out);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|s| s.abs() <= 0.5 + 1e-6));
        // After settling, the level sits close to the ceiling (not over-limited).
        let peak = out[24_000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.45, "{peak}");
        // Quiet audio passes through unchanged (just delayed).
        let mut l = Limiter::new(0.5, 48_000, 1, false);
        let quiet = sine(48_000, 440.0, 0.1, 0.1);
        let mut out = Vec::new();
        l.process(&quiet, &mut out);
        l.flush(&mut out);
        assert_eq!(out, quiet);
    }
}
