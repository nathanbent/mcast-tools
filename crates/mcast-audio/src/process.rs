//! Content-tuned processing for outgoing audio, mirroring `mcast-play.sh`.
//!
//! | mode  | chain (at the source sample rate)                                      |
//! |-------|-------------------------------------------------------------------------|
//! | voice | high-pass, noise reduction, compressor 4:1, de-esser, presence EQ, loudness -16 LUFS |
//! | music | high-pass, compressor 2:1, (narrowband) mid EQ, loudness -18 LUFS       |
//! | none  | nothing                                                                 |
//!
//! An input gain, if any, comes first, so the chain sees a usable level from
//! a quiet microphone. `normalize` adds loudness normalisation (to the voice
//! target) when the mode is none; voice and music always normalise.
//!
//! When normalising, a low-pass at the codec's band edge (3.4 kHz for
//! G.711, 7 kHz for G.722 / wideband Opus) runs just before it. A volume
//! change and limiter come last so normalisation can't undo them.
//!
//! Loudness is normalised to an integrated target with a -2 dBTP true-peak
//! ceiling. Files are measured first (two passes, exact); live input uses an
//! automatic gain control that settles quickly, then follows slowly.

use crate::codec::{Codec, OpusBandwidth, OpusOptions, StreamSpec};
use crate::dsp::biquad::{Biquad, Coeffs, BUTTERWORTH_Q};
use crate::dsp::db_to_lin;
use crate::dsp::deesser::DeEsser;
use crate::dsp::denoise::Denoiser;
use crate::dsp::dynamics::{Compressor, CompressorParams, Limiter};
use crate::dsp::loudness::{LoudnessAgc, LoudnessMeter};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    None,
    Voice,
    Music,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(Mode::None),
            "voice" | "speech" => Ok(Mode::Voice),
            "music" => Ok(Mode::Music),
            _ => Err(format!(
                "unknown mode '{s}' (expected voice, music or none)"
            )),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::None => "none",
            Mode::Voice => "voice",
            Mode::Music => "music",
        })
    }
}

impl Mode {
    /// Integrated loudness target (LUFS).
    pub fn loudness_target(self) -> Option<f64> {
        match self {
            Mode::None => None,
            Mode::Voice => Some(-16.0),
            Mode::Music => Some(-18.0),
        }
    }
}

const TRUE_PEAK_DBTP: f64 = -2.0;
/// ffmpeg `alimiter` limit used after a volume change.
pub(crate) const VOLUME_LIMIT: f64 = 0.89;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProcessOptions {
    pub mode: Mode,
    /// Volume change in dB, applied after processing, with a limiter.
    pub volume_db: Option<f64>,
    /// Gain in dB applied to the source before any processing (not to
    /// chimes), e.g. to lift a quiet microphone.
    pub input_gain_db: Option<f64>,
    /// Normalise loudness even when the mode is none (to the voice target).
    pub normalize: bool,
}

impl ProcessOptions {
    /// Integrated loudness target, if the audio is normalised.
    pub fn loudness_target(&self) -> Option<f64> {
        self.mode.loudness_target().or_else(|| {
            self.normalize
                .then(|| Mode::Voice.loudness_target().unwrap())
        })
    }
}

/// What the codec can carry, which shapes the EQ and final low-pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub narrow: bool,
    pub lowpass_hz: Option<f64>,
}

impl Band {
    pub fn for_stream(spec: &StreamSpec, opus: &OpusOptions) -> Self {
        let narrow = Band {
            narrow: true,
            lowpass_hz: Some(3400.0),
        };
        let wide = Band {
            narrow: false,
            lowpass_hz: Some(7000.0),
        };
        let full = Band {
            narrow: false,
            lowpass_hz: None,
        };
        match spec.codec {
            Codec::Pcmu | Codec::Pcma => narrow,
            Codec::G722 => wide,
            Codec::Opus => match opus.max_bandwidth {
                Some(OpusBandwidth::Narrow) => narrow,
                Some(OpusBandwidth::Wide) => wide,
                _ => full,
            },
            Codec::L16 | Codec::L24 => full,
        }
    }
}

/// How loudness normalisation is done.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Loudness {
    /// Run the chain up to normalisation and measure (first pass over a file).
    Measure,
    /// Apply a fixed gain found by measuring (second pass).
    Static { gain_db: f64 },
    /// Live input: steer towards the target as audio arrives.
    Dynamic,
}

enum Stage {
    Filter(Biquad),
    Denoise(Denoiser),
    Compress(Compressor),
    DeEss(DeEsser),
    Gain(f32),
    Agc(LoudnessAgc),
    Limit(Limiter),
    Meter(LoudnessMeter),
}

impl Stage {
    fn process(&mut self, buf: Vec<f32>) -> Vec<f32> {
        let mut buf = buf;
        match self {
            Stage::Filter(f) => f.process(&mut buf),
            Stage::Compress(c) => c.process(&mut buf),
            Stage::DeEss(d) => d.process(&mut buf),
            Stage::Agc(a) => a.process(&mut buf),
            Stage::Meter(m) => m.push(&buf),
            Stage::Gain(g) => buf.iter_mut().for_each(|s| *s *= *g),
            Stage::Denoise(d) => {
                let mut out = Vec::with_capacity(buf.len());
                d.process(&buf, &mut out);
                return out;
            }
            Stage::Limit(l) => {
                let mut out = Vec::with_capacity(buf.len());
                l.process(&buf, &mut out);
                return out;
            }
        }
        buf
    }

    fn flush(&mut self) -> Vec<f32> {
        let mut out = Vec::new();
        match self {
            Stage::Denoise(d) => d.flush(&mut out),
            Stage::Limit(l) => l.flush(&mut out),
            _ => {}
        }
        out
    }
}

pub struct Processor {
    stages: Vec<Stage>,
}

impl Processor {
    pub fn new(
        opts: ProcessOptions,
        band: Band,
        loudness: Loudness,
        rate: u32,
        channels: usize,
    ) -> Self {
        let filter = |c: Coeffs| Stage::Filter(Biquad::new(c, channels));
        let mut stages = Vec::new();
        // Whether something after the input gain keeps peaks in range.
        let mut limited = false;
        if let Some(db) = opts.input_gain_db.filter(|db| *db != 0.0) {
            stages.push(Stage::Gain(db_to_lin(db) as f32));
        }
        match opts.mode {
            Mode::None => {}
            Mode::Voice => {
                // Rumble/plosive cut, light denoise, steady level, tame sibilance, presence lift.
                let (hp, eq_hz, eq_db) = if band.narrow {
                    (250.0, 2500.0, 3.0)
                } else {
                    (120.0, 3000.0, 2.0)
                };
                stages.push(filter(Coeffs::highpass(rate, hp, BUTTERWORTH_Q)));
                stages.push(Stage::Denoise(Denoiser::new(10.0, -40.0, rate, channels)));
                stages.push(Stage::Compress(Compressor::new(
                    CompressorParams {
                        threshold_db: -20.0,
                        ratio: 4.0,
                        attack_ms: 5.0,
                        release_ms: 120.0,
                        makeup: 3.0,
                    },
                    rate,
                    channels,
                )));
                stages.push(Stage::DeEss(DeEsser::new(0.4, rate, channels)));
                stages.push(filter(Coeffs::peaking(rate, eq_hz, 1.0, eq_db)));
            }
            Mode::Music => {
                // Keep more low end and dynamics; on G.711, lean on the midrange that survives.
                stages.push(filter(Coeffs::highpass(
                    rate,
                    if band.narrow { 150.0 } else { 60.0 },
                    BUTTERWORTH_Q,
                )));
                stages.push(Stage::Compress(Compressor::new(
                    CompressorParams {
                        threshold_db: -24.0,
                        ratio: 2.0,
                        attack_ms: 20.0,
                        release_ms: 250.0,
                        makeup: 2.0,
                    },
                    rate,
                    channels,
                )));
                if band.narrow {
                    stages.push(filter(Coeffs::peaking(rate, 300.0, 1.0, -2.0)));
                    stages.push(filter(Coeffs::peaking(rate, 1800.0, 1.5, 2.0)));
                }
            }
        }
        if let Some(target) = opts.loudness_target() {
            // The script low-passes after loudnorm; doing it before keeps the
            // true-peak ceiling intact (filtering after a limiter can overshoot).
            if let Some(lp) = band.lowpass_hz {
                stages.push(filter(Coeffs::lowpass(rate, lp, BUTTERWORTH_Q)));
            }
            match loudness {
                Loudness::Measure => {
                    stages.push(Stage::Meter(LoudnessMeter::new(rate, channels)));
                    return Self { stages };
                }
                Loudness::Static { gain_db } => stages.push(Stage::Gain(db_to_lin(gain_db) as f32)),
                Loudness::Dynamic => {
                    stages.push(Stage::Agc(LoudnessAgc::new(target, rate, channels)))
                }
            }
            stages.push(Stage::Limit(Limiter::new(
                db_to_lin(TRUE_PEAK_DBTP),
                rate,
                channels,
                true,
            )));
            limited = true;
        }
        if let Some(db) = opts.volume_db {
            limited = true;
            stages.push(Stage::Gain(db_to_lin(db) as f32));
            stages.push(Stage::Limit(Limiter::new(
                VOLUME_LIMIT,
                rate,
                channels,
                false,
            )));
        }
        if !limited && opts.input_gain_db.is_some_and(|db| db > 0.0) {
            // A boost with nothing after it to catch the peaks.
            stages.push(Stage::Limit(Limiter::new(
                VOLUME_LIMIT,
                rate,
                channels,
                false,
            )));
        }
        Self { stages }
    }

    pub fn is_passthrough(&self) -> bool {
        self.stages.is_empty()
    }

    /// Process interleaved audio, appending to `out`. Output may lag input
    /// slightly (noise reduction and limiter lookahead); `flush` catches up.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let mut buf = input.to_vec();
        for stage in &mut self.stages {
            buf = stage.process(buf);
        }
        out.extend_from_slice(&buf);
    }

    /// Emit everything still held, so total output length equals input length.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let mut buf = Vec::new();
        for stage in &mut self.stages {
            buf = stage.process(buf);
            buf.extend(stage.flush());
        }
        out.extend_from_slice(&buf);
    }

    /// Integrated loudness measured in `Loudness::Measure` mode.
    pub fn measured_loudness(&self) -> Option<f64> {
        self.stages.iter().find_map(|s| match s {
            Stage::Meter(m) => m.integrated(),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speechy(rate: u32, secs: f64) -> Vec<f32> {
        // A 200 Hz buzz with syllable-like amplitude modulation and some hiss.
        let mut seed = 7u32;
        (0..(rate as f64 * secs) as usize)
            .map(|i| {
                let t = i as f64 / rate as f64;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5) * 0.01;
                let env = (0.5 + 0.5 * (2.0 * std::f64::consts::PI * 3.0 * t).sin()).powi(2);
                let buzz = (1..8)
                    .map(|h| (2.0 * std::f64::consts::PI * 200.0 * h as f64 * t).sin() / h as f64)
                    .sum::<f64>();
                (0.05 * env * buzz + noise) as f32
            })
            .collect()
    }

    fn run(p: &mut Processor, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in input.chunks(480) {
            p.process(chunk, &mut out);
        }
        p.flush(&mut out);
        out
    }

    #[test]
    fn two_pass_hits_loudness_target_within_true_peak() {
        let rate = 48_000;
        let input = speechy(rate, 8.0);
        let band = Band {
            narrow: false,
            lowpass_hz: Some(7000.0),
        };
        for mode in [Mode::Voice, Mode::Music] {
            let opts = ProcessOptions {
                mode,
                ..ProcessOptions::default()
            };
            let mut measure = Processor::new(opts, band, Loudness::Measure, rate, 1);
            run(&mut measure, &input);
            let measured = measure.measured_loudness().unwrap();
            let target = mode.loudness_target().unwrap();
            let mut p = Processor::new(
                opts,
                band,
                Loudness::Static {
                    gain_db: target - measured,
                },
                rate,
                1,
            );
            let out = run(&mut p, &input);
            assert_eq!(out.len(), input.len());
            let mut meter = LoudnessMeter::new(rate, 1);
            meter.push(&out);
            let got = meter.integrated().unwrap();
            // Limiting peaks can only lower it, a little.
            assert!(
                got <= target + 0.1 && got > target - 1.5,
                "{mode}: {got} LUFS"
            );
            let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(
                peak as f64 <= db_to_lin(TRUE_PEAK_DBTP) + 1e-6,
                "{mode}: peak {peak}"
            );
        }
    }

    #[test]
    fn none_is_passthrough_and_volume_is_applied_and_limited() {
        let p = Processor::new(
            ProcessOptions::default(),
            Band::for_stream(
                &StreamSpec::new(Codec::L16, 48_000, 2),
                &OpusOptions::default(),
            ),
            Loudness::Dynamic,
            48_000,
            2,
        );
        assert!(p.is_passthrough());
        let opts = ProcessOptions {
            volume_db: Some(-6.0),
            ..ProcessOptions::default()
        };
        let mut p = Processor::new(
            opts,
            Band {
                narrow: true,
                lowpass_hz: None,
            },
            Loudness::Dynamic,
            8_000,
            1,
        );
        let out = run(&mut p, &[0.5; 8000]);
        assert!((out[4000] - 0.5 * db_to_lin(-6.0) as f32).abs() < 1e-4);
        let opts = ProcessOptions {
            volume_db: Some(12.0),
            ..ProcessOptions::default()
        };
        let mut p = Processor::new(
            opts,
            Band {
                narrow: true,
                lowpass_hz: None,
            },
            Loudness::Dynamic,
            8_000,
            1,
        );
        let out = run(&mut p, &[0.5; 8000]);
        assert!(out.iter().all(|s| s.abs() <= VOLUME_LIMIT as f32 + 1e-6));
    }

    const NARROW: Band = Band {
        narrow: true,
        lowpass_hz: None,
    };

    #[test]
    fn input_gain_lifts_the_source_and_a_boost_is_limited() {
        let opts = ProcessOptions {
            input_gain_db: Some(6.0),
            ..ProcessOptions::default()
        };
        let mut p = Processor::new(opts, NARROW, Loudness::Dynamic, 8_000, 1);
        let out = run(&mut p, &[0.1; 8000]);
        assert!((out[4000] - 0.1 * db_to_lin(6.0) as f32).abs() < 1e-4);
        let mut p = Processor::new(opts, NARROW, Loudness::Dynamic, 8_000, 1);
        let out = run(&mut p, &[0.9; 8000]);
        assert!(out.iter().all(|s| s.abs() <= VOLUME_LIMIT as f32 + 1e-6));
        // A cut needs no limiter.
        let opts = ProcessOptions {
            input_gain_db: Some(-6.0),
            ..ProcessOptions::default()
        };
        let mut p = Processor::new(opts, NARROW, Loudness::Dynamic, 8_000, 1);
        let out = run(&mut p, &[0.5; 8000]);
        assert!((out[4000] - 0.5 * db_to_lin(-6.0) as f32).abs() < 1e-4);
    }

    #[test]
    fn normalize_without_a_mode_brings_quiet_live_input_to_the_voice_target() {
        let rate = 48_000;
        let quiet: Vec<f32> = speechy(rate, 6.0).iter().map(|s| s * 0.5).collect();
        let opts = ProcessOptions {
            normalize: true,
            ..ProcessOptions::default()
        };
        assert_eq!(opts.loudness_target(), Mode::Voice.loudness_target());
        let mut p = Processor::new(opts, NARROW, Loudness::Dynamic, rate, 1);
        let out = run(&mut p, &quiet);
        let mut before = LoudnessMeter::new(rate, 1);
        before.push(&quiet[rate as usize * 2..]);
        let mut after = LoudnessMeter::new(rate, 1);
        after.push(&out[rate as usize * 2..]);
        let (before, after) = (before.integrated().unwrap(), after.integrated().unwrap());
        assert!(before < -26.0, "{before}");
        assert!((after + 16.0).abs() < 2.0, "{before} -> {after}");
        let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(
            peak as f64 <= db_to_lin(TRUE_PEAK_DBTP) + 1e-6,
            "peak {peak}"
        );
    }
}
