//! De-esser, standing in for ffmpeg's `deesser`.
//!
//! A side-chain compares the band above ~5.5 kHz with the whole signal. When
//! that band dominates (an "s" or "sh"), a high shelf at 5.5 kHz is lowered;
//! otherwise the shelf is flat and the signal passes untouched.

use super::biquad::{Biquad, Coeffs, BUTTERWORTH_Q};

const SPLIT_HZ: f64 = 5500.0;
/// High-band share of the signal (RMS ratio) above which reduction starts.
const THRESHOLD: f64 = 0.35;
/// Recompute the shelf every this many frames.
const UPDATE: usize = 16;

pub struct DeEsser {
    channels: usize,
    rate: u32,
    enabled: bool,
    detector: Biquad,
    shelf: Biquad,
    max_reduction_db: f64,
    env_high: f64,
    env_full: f64,
    attack: f64,
    release: f64,
    reduction_db: f64,
    applied_db: f64,
    counter: usize,
}

impl DeEsser {
    /// `intensity` 0..1 (ffmpeg's `i`); 0.4 allows up to 12 dB of reduction.
    pub fn new(intensity: f64, rate: u32, channels: usize) -> Self {
        let coeff = |ms: f64| 1.0 - (-1.0 / (rate as f64 * ms / 1000.0)).exp();
        Self {
            channels,
            rate,
            // Sibilance can't be represented below ~12 kHz sample rate.
            enabled: rate >= 12_000,
            detector: Biquad::new(Coeffs::highpass(rate, SPLIT_HZ, BUTTERWORTH_Q), channels),
            shelf: Biquad::new(Coeffs::IDENTITY, channels),
            max_reduction_db: 30.0 * intensity,
            env_high: 0.0,
            env_full: 0.0,
            attack: coeff(1.0),
            release: coeff(60.0),
            reduction_db: 0.0,
            applied_db: 0.0,
            counter: 0,
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.enabled {
            return;
        }
        for frame in buf.chunks_exact_mut(self.channels) {
            let (mut ph, mut pf) = (0.0, 0.0);
            for (c, &x) in frame.iter().enumerate() {
                let h = self.detector.tick(c, x as f64);
                ph += h * h;
                pf += (x as f64) * (x as f64);
            }
            let (a, r) = (self.attack, self.release);
            self.env_high += (ph - self.env_high) * if ph > self.env_high { a } else { r };
            self.env_full += (pf - self.env_full) * if pf > self.env_full { a } else { r };
            let ratio = if self.env_full > 1e-12 {
                (self.env_high / self.env_full).sqrt()
            } else {
                0.0
            };
            let target = if ratio > THRESHOLD {
                (20.0 * (ratio / THRESHOLD).log10() * 3.0).min(self.max_reduction_db)
            } else {
                0.0
            };
            // Smooth to avoid zipper noise (about 2 ms).
            self.reduction_db += (target - self.reduction_db) * 0.01;

            self.counter += 1;
            if self.counter >= UPDATE {
                self.counter = 0;
                if (self.reduction_db - self.applied_db).abs() > 0.05 {
                    self.applied_db = self.reduction_db;
                    let c = if self.applied_db < 0.05 {
                        Coeffs::IDENTITY
                    } else {
                        Coeffs::highshelf(self.rate, SPLIT_HZ, -self.applied_db)
                    };
                    self.shelf.set(c);
                }
            }
            for (c, s) in frame.iter_mut().enumerate() {
                *s = self.shelf.tick(c, *s as f64) as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, amp: f64) -> Vec<f32> {
        (0..48_000)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin()) as f32)
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    #[test]
    fn cuts_sibilance_not_voice() {
        let mut d = DeEsser::new(0.4, 48_000, 1);
        let mut voice = tone(500.0, 0.3);
        d.process(&mut voice);
        assert!((rms(&voice[24_000..]) / (0.3 / 2f64.sqrt()) - 1.0).abs() < 0.01);

        let mut d = DeEsser::new(0.4, 48_000, 1);
        let mut ess = tone(7000.0, 0.3);
        d.process(&mut ess);
        let cut_db = 20.0 * (rms(&ess[24_000..]) / (0.3 / 2f64.sqrt())).log10();
        assert!(cut_db < -6.0, "{cut_db}");
    }
}
