//! Second-order IIR filters (RBJ Audio EQ Cookbook), as used by ffmpeg's
//! `highpass`, `lowpass` and `equalizer` filters.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// Butterworth Q, ffmpeg's default for `highpass`/`lowpass`.
pub const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Coefficients already normalised by a0: `b = [b0, b1, b2]`, `a = [a1, a2]`.
    pub fn from_raw(b: [f64; 3], a: [f64; 2]) -> Self {
        Coeffs {
            b0: b[0],
            b1: b[1],
            b2: b[2],
            a1: a[0],
            a2: a[1],
        }
    }

    fn new(b: [f64; 3], a: [f64; 3]) -> Self {
        Coeffs {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
        }
    }

    fn w0(rate: u32, freq: f64) -> Option<(f64, f64)> {
        // Frequencies at or above Nyquist can't be filtered at this rate.
        if freq <= 0.0 || freq >= 0.49 * rate as f64 {
            return None;
        }
        let w0 = 2.0 * PI * freq / rate as f64;
        Some((w0.cos(), w0.sin()))
    }

    pub fn highpass(rate: u32, freq: f64, q: f64) -> Self {
        let Some((cos, sin)) = Self::w0(rate, freq) else {
            return Self::IDENTITY;
        };
        let alpha = sin / (2.0 * q);
        Self::new(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    pub fn lowpass(rate: u32, freq: f64, q: f64) -> Self {
        let Some((cos, sin)) = Self::w0(rate, freq) else {
            return Self::IDENTITY;
        };
        let alpha = sin / (2.0 * q);
        Self::new(
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    /// Peaking EQ (ffmpeg `equalizer` with `t=q`).
    pub fn peaking(rate: u32, freq: f64, q: f64, gain_db: f64) -> Self {
        let Some((cos, sin)) = Self::w0(rate, freq) else {
            return Self::IDENTITY;
        };
        let a = 10f64.powf(gain_db / 40.0);
        let alpha = sin / (2.0 * q);
        Self::new(
            [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
        )
    }

    /// High shelf (RBJ, slope 1).
    pub fn highshelf(rate: u32, freq: f64, gain_db: f64) -> Self {
        let Some((cos, sin)) = Self::w0(rate, freq) else {
            return Self::IDENTITY;
        };
        let a = 10f64.powf(gain_db / 40.0);
        let alpha = sin / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        Self::new(
            [
                a * ((a + 1.0) + (a - 1.0) * cos + sa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sa),
            ],
            [
                (a + 1.0) - (a - 1.0) * cos + sa,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sa,
            ],
        )
    }

    /// Magnitude response in dB at `freq`.
    pub fn response_db(&self, rate: u32, freq: f64) -> f64 {
        let w = 2.0 * PI * freq / rate as f64;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2)
            + (self.b1 * s1 + self.b2 * s2).powi(2))
        .sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2)
            + (self.a1 * s1 + self.a2 * s2).powi(2))
        .sqrt();
        20.0 * (num / den).log10()
    }
}

/// A biquad applied to every channel of an interleaved buffer.
#[derive(Debug, Clone)]
pub struct Biquad {
    c: Coeffs,
    /// Transposed direct form II state per channel.
    state: Vec<[f64; 2]>,
}

impl Biquad {
    pub fn new(c: Coeffs, channels: usize) -> Self {
        Self {
            c,
            state: vec![[0.0; 2]; channels],
        }
    }

    /// Change coefficients, keeping state (for slowly varying filters).
    pub fn set(&mut self, c: Coeffs) {
        self.c = c;
    }

    #[inline]
    pub fn tick(&mut self, channel: usize, x: f64) -> f64 {
        let s = &mut self.state[channel];
        let y = self.c.b0 * x + s[0];
        s[0] = self.c.b1 * x - self.c.a1 * y + s[1];
        s[1] = self.c.b2 * x - self.c.a2 * y;
        y
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if self.c == Coeffs::IDENTITY {
            return;
        }
        let ch = self.state.len();
        for (i, x) in buf.iter_mut().enumerate() {
            *x = self.tick(i % ch, *x as f64) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses() {
        let hp = Coeffs::highpass(48_000, 250.0, BUTTERWORTH_Q);
        assert!((hp.response_db(48_000, 250.0) + 3.01).abs() < 0.05);
        assert!(hp.response_db(48_000, 62.5) < -23.0); // 12 dB/octave
        assert!(hp.response_db(48_000, 4000.0).abs() < 0.05);
        let lp = Coeffs::lowpass(8_000, 3400.0, BUTTERWORTH_Q);
        assert!((lp.response_db(8_000, 3400.0) + 3.01).abs() < 0.05);
        let eq = Coeffs::peaking(48_000, 2500.0, 1.0, 3.0);
        assert!((eq.response_db(48_000, 2500.0) - 3.0).abs() < 0.01);
        assert!(eq.response_db(48_000, 100.0).abs() < 0.1);
        assert_eq!(
            Coeffs::lowpass(8_000, 7000.0, BUTTERWORTH_Q),
            Coeffs::IDENTITY
        );
        let shelf = Coeffs::highshelf(48_000, 5500.0, -12.0);
        assert!((shelf.response_db(48_000, 15_000.0) + 12.0).abs() < 0.5);
        assert!(shelf.response_db(48_000, 500.0).abs() < 0.2);
    }

    #[test]
    fn filters_signal_like_response() {
        let mut f = Biquad::new(Coeffs::highpass(8_000, 250.0, BUTTERWORTH_Q), 1);
        let mut buf: Vec<f32> = (0..8000)
            .map(|i| (2.0 * PI * 100.0 * i as f64 / 8000.0).sin() as f32)
            .collect();
        f.process(&mut buf);
        let peak = buf[4000..].iter().fold(0f32, |m, v| m.max(v.abs()));
        let expected = 10f64
            .powf(Coeffs::highpass(8_000, 250.0, BUTTERWORTH_Q).response_db(8_000, 100.0) / 20.0);
        assert!(
            (peak as f64 - expected).abs() < 0.01,
            "{peak} vs {expected}"
        );
    }
}
