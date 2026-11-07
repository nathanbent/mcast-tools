//! FFT noise reduction, modelled on ffmpeg's `afftdn` with a fixed noise
//! floor: each frequency bin is attenuated (Wiener-style) by how close it is
//! to the noise floor, never by more than `reduction_db`.
//!
//! Short-time Fourier transform with sqrt-Hann windows at 50% overlap, so
//! with no attenuation the output equals the input (delayed internally, then
//! re-aligned so the stream keeps its length and timing).

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

pub struct Denoiser {
    channels: usize,
    n: usize,
    hop: usize,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    noise_power: f32,
    floor_gain: f32,
    input: Vec<Vec<f32>>,
    overlap: Vec<Vec<f32>>,
    smoothed_power: Vec<Vec<f32>>,
    spectrum: Vec<Complex<f32>>,
    /// Output samples still to drop to cancel the STFT delay.
    skip: usize,
    in_frames: u64,
    out_frames: u64,
}

impl Denoiser {
    pub fn new(reduction_db: f64, noise_floor_db: f64, rate: u32, channels: usize) -> Self {
        // About 20 ms frames.
        let n = ((rate as f64 * 0.02) as usize)
            .next_power_of_two()
            .clamp(128, 2048);
        let hop = n / 2;
        let window: Vec<f32> = (0..n)
            .map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).sqrt())
            .collect();
        let mut planner = FftPlanner::new();
        // White noise of RMS sigma puts sigma^2 * sum(w^2) power in each bin.
        let sigma = 10f64.powf(noise_floor_db / 20.0) as f32;
        let window_power: f32 = window.iter().map(|w| w * w).sum();
        Self {
            channels,
            n,
            hop,
            fft: planner.plan_fft_forward(n),
            ifft: planner.plan_fft_inverse(n),
            window,
            noise_power: sigma * sigma * window_power,
            floor_gain: 10f64.powf(-reduction_db / 20.0) as f32,
            // Pre-fill so the first frame completes after `hop` samples.
            input: vec![vec![0.0; n - hop]; channels],
            overlap: vec![vec![0.0; n]; channels],
            smoothed_power: vec![vec![0.0; n]; channels],
            spectrum: vec![Complex::default(); n],
            skip: (n - hop) * channels,
            in_frames: 0,
            out_frames: 0,
        }
    }

    // `c` indexes several per-channel buffers at once.
    #[allow(clippy::needless_range_loop)]
    fn run_frame(&mut self, out: &mut Vec<f32>) {
        let mut hop_out = vec![vec![0.0f32; self.hop]; self.channels];
        for c in 0..self.channels {
            for (i, s) in self.spectrum.iter_mut().enumerate() {
                *s = Complex::new(self.input[c][i] * self.window[i], 0.0);
            }
            self.fft.process(&mut self.spectrum);
            for (k, bin) in self.spectrum.iter_mut().enumerate() {
                let p = bin.norm_sqr();
                let sp = &mut self.smoothed_power[c][k];
                *sp = 0.6 * *sp + 0.4 * p;
                let gain = if *sp > 0.0 {
                    (1.0 - self.noise_power / *sp).max(self.floor_gain)
                } else {
                    self.floor_gain
                };
                *bin *= gain;
            }
            self.ifft.process(&mut self.spectrum);
            let scale = 1.0 / self.n as f32;
            for (i, s) in self.spectrum.iter().enumerate() {
                self.overlap[c][i] += s.re * scale * self.window[i];
            }
            hop_out[c].copy_from_slice(&self.overlap[c][..self.hop]);
            self.overlap[c].copy_within(self.hop.., 0);
            let n = self.n;
            self.overlap[c][n - self.hop..].fill(0.0);
            self.input[c].drain(..self.hop);
        }
        for i in 0..self.hop {
            for ch in hop_out.iter() {
                if self.skip > 0 {
                    self.skip -= 1;
                } else {
                    out.push(ch[i]);
                }
            }
        }
    }

    pub fn process(&mut self, buf: &[f32], out: &mut Vec<f32>) {
        let before = out.len();
        for frame in buf.chunks_exact(self.channels) {
            for (c, &x) in frame.iter().enumerate() {
                self.input[c].push(x);
            }
            self.in_frames += 1;
            if self.input[0].len() == self.n {
                self.run_frame(out);
            }
        }
        self.out_frames += ((out.len() - before) / self.channels) as u64;
    }

    /// Push out the remaining audio so output length equals input length.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let start = out.len();
        let zeros = vec![0.0; self.n * self.channels];
        while self.out_frames < self.in_frames {
            let in_before = self.in_frames;
            self.process(&zeros[..self.hop * self.channels], out);
            self.in_frames = in_before;
        }
        let extra = (self.out_frames - self.in_frames) as usize * self.channels;
        out.truncate(out.len().saturating_sub(extra).max(start));
        self.out_frames = self.in_frames;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn run(d: &mut Denoiser, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in input.chunks(777) {
            d.process(chunk, &mut out);
        }
        d.flush(&mut out);
        out
    }

    #[test]
    fn transparent_above_the_noise_floor() {
        // A loud tone is far above -40 dBFS, so it passes unchanged and aligned.
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let out = run(&mut Denoiser::new(10.0, -40.0, 48_000, 1), &input);
        assert_eq!(out.len(), input.len());
        let err: Vec<f32> = out[2048..46_000]
            .iter()
            .zip(&input[2048..46_000])
            .map(|(a, b)| a - b)
            .collect();
        assert!(rms(&err) < 0.005, "error {}", rms(&err));
    }

    #[test]
    fn attenuates_noise_by_up_to_reduction() {
        // Noise well below the floor is reduced by the full 10 dB.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..96_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.004 // about -59 dBFS RMS
            })
            .collect();
        let out = run(&mut Denoiser::new(10.0, -40.0, 48_000, 2), &noise);
        let reduction = 20.0 * (rms(&out[9600..]) / rms(&noise[9600..])).log10();
        assert!((reduction + 10.0).abs() < 1.0, "{reduction} dB");
    }
}
