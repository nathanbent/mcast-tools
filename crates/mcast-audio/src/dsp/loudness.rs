//! ITU-R BS.1770-4 / EBU R128 loudness: K-weighting, gated integrated
//! loudness, and a slow automatic gain control for live input.

use super::biquad::Biquad;

/// K-weighting pre-filter and RLB high-pass, designed for any sample rate
/// (the analog prototypes used by libebur128).
fn k_weighting(rate: u32, channels: usize) -> [Biquad; 2] {
    let rate = rate as f64;
    let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = super::biquad::Coeffs::from_raw(
        [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let highpass = super::biquad::Coeffs::from_raw(
        [1.0, -2.0, 1.0],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );
    [
        Biquad::new(shelf, channels),
        Biquad::new(highpass, channels),
    ]
}

fn to_lufs(mean_square: f64) -> f64 {
    -0.691 + 10.0 * mean_square.log10()
}

/// Measures integrated loudness (400 ms blocks, 75% overlap, -70 LUFS
/// absolute and -10 LU relative gates).
pub struct LoudnessMeter {
    filters: [Biquad; 2],
    channels: usize,
    sub_len: usize,
    sub_acc: f64,
    sub_count: usize,
    last4: [f64; 4],
    subs_seen: usize,
    blocks: Vec<f64>,
}

impl LoudnessMeter {
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            filters: k_weighting(rate, channels),
            channels,
            sub_len: (rate / 10) as usize, // 100 ms
            sub_acc: 0.0,
            sub_count: 0,
            last4: [0.0; 4],
            subs_seen: 0,
            blocks: Vec::new(),
        }
    }

    pub fn push(&mut self, buf: &[f32]) {
        for frame in buf.chunks_exact(self.channels) {
            for (c, &x) in frame.iter().enumerate() {
                let pre = self.filters[0].tick(c, x as f64);
                let y = self.filters[1].tick(c, pre);
                self.sub_acc += y * y;
            }
            self.sub_count += 1;
            if self.sub_count == self.sub_len {
                self.last4.rotate_left(1);
                self.last4[3] = self.sub_acc / self.sub_len as f64;
                self.sub_acc = 0.0;
                self.sub_count = 0;
                self.subs_seen += 1;
                if self.subs_seen >= 4 {
                    self.blocks.push(self.last4.iter().sum::<f64>() / 4.0);
                }
            }
        }
    }

    /// Integrated loudness in LUFS, or `None` if everything was gated out.
    pub fn integrated(&self) -> Option<f64> {
        let abs: Vec<f64> = self
            .blocks
            .iter()
            .copied()
            .filter(|&z| to_lufs(z) > -70.0)
            .collect();
        if abs.is_empty() {
            return None;
        }
        let relative = to_lufs(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
        let gated: Vec<f64> = abs.into_iter().filter(|&z| to_lufs(z) > relative).collect();
        (!gated.is_empty()).then(|| to_lufs(gated.iter().sum::<f64>() / gated.len() as f64))
    }
}

/// Slowly steers short-term (≈3 s) loudness towards a target, for live input
/// where the whole programme can't be measured first. Holds its gain through
/// silence so pauses aren't pumped up.
pub struct LoudnessAgc {
    filters: [Biquad; 2],
    channels: usize,
    target: f64,
    mean_square: f64,
    avg_coeff: f64,
    gain_db: f64,
    max_step_db: f64,
}

impl LoudnessAgc {
    pub fn new(target_lufs: f64, rate: u32, channels: usize) -> Self {
        Self {
            filters: k_weighting(rate, channels),
            channels,
            target: target_lufs,
            mean_square: 0.0,
            avg_coeff: 1.0 - (-1.0 / (rate as f64 * 3.0)).exp(),
            gain_db: 0.0,
            max_step_db: 6.0 / rate as f64, // at most 6 dB per second
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(self.channels) {
            let mut ms = 0.0;
            for (c, &x) in frame.iter().enumerate() {
                let pre = self.filters[0].tick(c, x as f64);
                let y = self.filters[1].tick(c, pre);
                ms += y * y;
            }
            self.mean_square += (ms - self.mean_square) * self.avg_coeff;
            let loudness = to_lufs(self.mean_square.max(1e-20));
            if loudness > -50.0 {
                let want = (self.target - loudness).clamp(-24.0, 24.0);
                self.gain_db += (want - self.gain_db).clamp(-self.max_step_db, self.max_step_db);
            }
            let g = super::db_to_lin(self.gain_db) as f32;
            for s in frame {
                *s *= g;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, amp: f64, secs: f64, channels: usize) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize)
            .flat_map(|i| {
                let v = (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin())
                    as f32;
                std::iter::repeat(v).take(channels)
            })
            .collect()
    }

    #[test]
    fn reference_levels() {
        // BS.1770: a 0 dBFS 997 Hz sine in one channel reads -3.01 LUFS; in two, 0 LUFS.
        for (rate, ch, want) in [
            (48_000, 1, -3.01),
            (48_000, 2, 0.0),
            (44_100, 1, -3.01),
            (16_000, 1, -3.01),
        ] {
            let mut m = LoudnessMeter::new(rate, ch);
            m.push(&sine(rate, 997.0, 1.0, 5.0, ch));
            let got = m.integrated().unwrap();
            assert!((got - want).abs() < 0.05, "{rate} Hz x{ch}: {got}");
        }
        // -20 dBFS reads 20 LU lower; silence is gated out.
        let mut m = LoudnessMeter::new(48_000, 1);
        m.push(&sine(48_000, 997.0, 0.1, 5.0, 1));
        assert!((m.integrated().unwrap() + 23.01).abs() < 0.05);
        let mut m = LoudnessMeter::new(48_000, 1);
        m.push(&[0.0; 48_000]);
        assert_eq!(m.integrated(), None);
    }

    #[test]
    fn agc_converges_to_target() {
        let mut agc = LoudnessAgc::new(-16.0, 48_000, 1);
        let mut x = sine(48_000, 997.0, 0.05, 20.0, 1); // about -29 LUFS
        agc.process(&mut x);
        let mut m = LoudnessMeter::new(48_000, 1);
        m.push(&x[48_000 * 15..]);
        let got = m.integrated().unwrap();
        assert!((got + 16.0).abs() < 0.5, "{got}");
    }
}
