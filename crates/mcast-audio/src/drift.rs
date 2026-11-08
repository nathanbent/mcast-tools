//! Clock-drift correction for playback.
//!
//! A sender's clock and our sound card's clock never run at exactly the same
//! rate; tens of ppm apart is typical, a few hundred for cheap USB devices.
//! Uncorrected, the jitter buffer slowly fills (until audio is dropped) or
//! drains (until it underruns), giving a glitch every few minutes.
//!
//! [`DriftControl`] watches how full the jitter buffer is and returns a
//! resampling ratio that holds it at its target. It is a PI controller on
//! the buffer level, smoothed over several seconds so network jitter and the
//! sound card's callback size don't wobble the pitch. The integral term
//! converges on the actual clock difference, which is reported as the drift.
//! Corrections are limited to ±1000 ppm (about 1.7 cents of pitch: inaudible).

use std::time::{Duration, Instant};

/// Proportional gain, per second of buffer error. With [`KI`] this gives a
/// critically damped loop with a natural frequency of 0.03 rad/s: a new
/// sender's drift is matched within two or three minutes (the buffer only
/// moves a few ms meanwhile), and short-term jitter is ignored.
const KP: f64 = 0.06;
/// Integral gain, per second² of buffer error.
const KI: f64 = 0.0009;
/// Time constant of the buffer-level smoothing.
const SMOOTHING: Duration = Duration::from_secs(8);
/// Largest correction applied.
const LIMIT: f64 = 1e-3;

pub struct DriftControl {
    /// Frames per second at the buffer.
    rate: f64,
    target_frames: f64,
    /// Smoothed buffer error in seconds (positive: fuller than target).
    error: Option<f64>,
    integral: f64,
    last: Option<Instant>,
    ratio: f64,
}

impl DriftControl {
    pub fn new(rate: u32, target_frames: usize) -> Self {
        Self {
            rate: rate as f64,
            target_frames: target_frames as f64,
            error: None,
            integral: 0.0,
            last: None,
            ratio: 1.0,
        }
    }

    /// Feed the buffer level observed at `now`, right after `pushed` frames
    /// were added; returns the resampling ratio (output frames per input
    /// frame) to use from here on. While the buffer isn't `playing` (filling
    /// up after a start or underrun) the level says nothing about drift, so
    /// the controller holds its last estimate.
    pub fn update(
        &mut self,
        buffered_frames: usize,
        pushed: usize,
        playing: bool,
        now: Instant,
    ) -> f64 {
        if !playing {
            self.error = None;
            self.last = None;
            self.ratio = 1.0 - self.integral * KI;
            return self.ratio;
        }
        let dt = self
            .last
            .map_or(0.0, |t| now.duration_since(t).as_secs_f64().min(0.5));
        self.last = Some(now);
        // Right after a push the level is at the top of its sawtooth; the
        // midpoint between before and after the push is a fair average.
        // (Using the peak would hold the real level half a packet low.)
        let level = buffered_frames as f64 - pushed as f64 / 2.0;
        let raw = (level - self.target_frames) / self.rate;
        let error = match self.error {
            None => raw,
            Some(e) => e + (raw - e) * (dt / SMOOTHING.as_secs_f64()).min(1.0),
        };
        self.error = Some(error);
        // Clamp the integral so it alone can't exceed the limit (anti-windup).
        self.integral = (self.integral + error * dt).clamp(-LIMIT / KI, LIMIT / KI);
        let correction = (KP * error + KI * self.integral).clamp(-LIMIT, LIMIT);
        // Fuller than target: the sender is fast, so stretch less (ratio < 1).
        self.ratio = 1.0 - correction;
        self.ratio
    }

    /// Estimated sender clock relative to ours, in ppm (positive: sender fast).
    pub fn drift_ppm(&self) -> f64 {
        self.integral * KI * 1e6
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::VariableResampler;
    use crate::jitter::jitter_buffer;

    struct Outcome {
        drift_ppm: f64,
        /// Worst buffer error over the last minute, in ms.
        settled_error_ms: f64,
        underruns: u64,
        overflows: u64,
        /// Largest swing of the correction around its mean over the last
        /// minute, in ppm: how much the pitch wobbles once settled.
        wobble_ppm: f64,
    }

    /// Simulate `secs` of a sender `ppm` fast (negative: slow) feeding our
    /// playback through the real jitter buffer, resampler and controller.
    /// Packets are 20 ms with up to ±`jitter_ms` of network jitter; the sound
    /// card pulls 10 ms at a time. With `correct` false the ratio stays 1.
    fn simulate(ppm: f64, jitter_ms: f64, secs: f64, correct: bool) -> Outcome {
        const RATE: u32 = 8_000;
        let target = RATE as usize * 40 / 1000; // 40 ms
        let (mut w, mut r) = jitter_buffer(1, target);
        let mut rs = VariableResampler::new(1);
        let mut dc = DriftControl::new(RATE, target);
        let base = Instant::now();
        let packet = (RATE / 50) as usize;
        let sender_period = 0.020 / (1.0 + ppm * 1e-6);
        let mut seed = 12345u64;
        let mut rand = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };

        let (mut next_send, mut next_pull) = (0.0f64, 0.0f64);
        let mut in_flight: Vec<f64> = Vec::new(); // arrival times
        let mut out = vec![0.0f32; RATE as usize / 100];
        let (mut resampled, mut worst_err) = (Vec::new(), 0.0f64);
        let mut corr = (f64::INFINITY, f64::NEG_INFINITY);
        let input = vec![0.1f32; packet];
        loop {
            // Next event: a packet leaves, a packet arrives, or the card pulls.
            let arrival = in_flight.iter().cloned().fold(f64::INFINITY, f64::min);
            let now = next_send.min(next_pull).min(arrival);
            if now > secs {
                break;
            }
            if now == next_send {
                in_flight.push(now + 0.005 + (rand() * 2.0 - 1.0) * jitter_ms / 1000.0 + 0.005);
                next_send += sender_period;
            } else if now == arrival {
                in_flight.retain(|&t| t != arrival);
                resampled.clear();
                rs.process(&input, &mut resampled);
                w.push(&resampled);
                let s = w.stats();
                let at = base + Duration::from_secs_f64(now);
                let ratio = dc.update(s.buffered_frames, resampled.len(), s.playing, at);
                if correct {
                    rs.set_ratio(ratio);
                }
                if now > secs - 60.0 {
                    let level = s.buffered_frames as f64 - resampled.len() as f64 / 2.0;
                    let err = (level - target as f64) / RATE as f64 * 1000.0;
                    worst_err = worst_err.max(err.abs());
                }
                if now > secs - 60.0 {
                    let c = (ratio - 1.0) * 1e6;
                    corr = (corr.0.min(c), corr.1.max(c));
                }
            } else {
                r.pull(&mut out);
                next_pull += 0.010;
            }
        }
        let s = w.stats();
        Outcome {
            drift_ppm: dc.drift_ppm(),
            settled_error_ms: worst_err,
            underruns: s.underruns,
            overflows: s.overflows,
            wobble_ppm: (corr.1 - corr.0) / 2.0,
        }
    }

    #[test]
    fn holds_buffer_level_against_fast_and_slow_senders() {
        for ppm in [300.0, -300.0, 0.0, 800.0, -800.0] {
            let o = simulate(ppm, 4.0, 600.0, true);
            assert!(
                (o.drift_ppm - ppm).abs() < 15.0,
                "{ppm} ppm: estimated {:.1}",
                o.drift_ppm
            );
            // Within about half a packet of target once settled (the level
            // is sampled right after each 20 ms packet lands).
            assert!(
                o.settled_error_ms < 15.0,
                "{ppm} ppm: off by {:.1} ms",
                o.settled_error_ms
            );
            assert_eq!((o.underruns, o.overflows), (0, 0), "{ppm} ppm");
            // ±50 ppm is under a tenth of a cent: far below audibility.
            assert!(o.wobble_ppm < 50.0, "{ppm} ppm: wobble {:.1}", o.wobble_ppm);
        }
    }

    #[test]
    fn without_correction_the_same_streams_glitch() {
        // Control for the test above: the simulation really does drift.
        assert!(simulate(300.0, 4.0, 600.0, false).overflows > 0);
        assert!(simulate(-300.0, 4.0, 600.0, false).underruns > 0);
    }

    #[test]
    fn holds_estimate_while_not_playing() {
        let mut dc = DriftControl::new(48_000, 1_920);
        let t = Instant::now();
        // Persistently full buffer: estimate grows positive.
        for i in 0..2_000 {
            dc.update(2_400, 0, true, t + Duration::from_millis(i * 20));
        }
        let est = dc.drift_ppm();
        assert!(est > 0.0);
        let r = dc.update(0, 0, false, t + Duration::from_secs(41));
        assert_eq!(dc.drift_ppm(), est);
        assert!((r - (1.0 - est * 1e-6)).abs() < 1e-12);
    }
}
