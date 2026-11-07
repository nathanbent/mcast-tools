//! Pre-tone ("chime") played before an announcement, as in `mcast-play.sh -C`.
//!
//! The tone is mono, band-limited for the codec (high-pass 200 Hz, low-pass at
//! the codec's band edge), normalised to -18 LUFS with a -3 dBTP ceiling, and
//! followed by a gap of silence. It is not voice-processed: noise reduction
//! and de-essing would smear a bell. A volume change applies to it as well as
//! to the announcement, since both are part of the same page.

use crate::convert::{map_channels, Converter};
use crate::dsp::biquad::{Biquad, Coeffs, BUTTERWORTH_Q};
use crate::dsp::db_to_lin;
use crate::dsp::dynamics::Limiter;
use crate::dsp::loudness::LoudnessMeter;
use crate::file::AudioFile;
use crate::process::{Band, VOLUME_LIMIT};
use anyhow::{Context, Result};
use std::f64::consts::PI;
use std::path::PathBuf;
use std::time::Duration;

const LOUDNESS_LUFS: f64 = -18.0;
const TRUE_PEAK_DBTP: f64 = -3.0;
const HIGHPASS_HZ: f64 = 200.0;
/// Longest tone file we'll load; a chime is a few seconds at most.
const MAX_SECONDS: u64 = 30;

#[derive(Debug, Clone, PartialEq)]
pub enum ChimeSource {
    /// Three ascending bell notes (C5, E5, G5), all within 500-1600 Hz so
    /// they sound the same on G.711 as on wideband codecs.
    BuiltIn,
    /// Any audio file `AudioFile` can read.
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chime {
    pub source: ChimeSource,
    /// Silence between the tone and the announcement.
    pub gap: Duration,
}

impl Chime {
    /// Render the tone and gap, interleaved at `rate` with `channels` channels.
    pub fn render(
        &self,
        band: Band,
        volume_db: Option<f64>,
        rate: u32,
        channels: u16,
    ) -> Result<Vec<f32>> {
        let mut mono = match &self.source {
            ChimeSource::BuiltIn => built_in(rate),
            ChimeSource::File(path) => load_mono(path, rate)
                .with_context(|| format!("loading chime {}", path.display()))?,
        };
        Biquad::new(Coeffs::highpass(rate, HIGHPASS_HZ, BUTTERWORTH_Q), 1).process(&mut mono);
        if let Some(lp) = band.lowpass_hz {
            Biquad::new(Coeffs::lowpass(rate, lp, BUTTERWORTH_Q), 1).process(&mut mono);
        }
        let mut meter = LoudnessMeter::new(rate, 1);
        meter.push(&mono);
        if let Some(measured) = meter.integrated() {
            let gain = db_to_lin((LOUDNESS_LUFS - measured).clamp(-30.0, 30.0)) as f32;
            mono.iter_mut().for_each(|s| *s *= gain);
        }
        let mut mono = limit(&mono, db_to_lin(TRUE_PEAK_DBTP), rate, true);
        if let Some(db) = volume_db {
            let gain = db_to_lin(db) as f32;
            mono.iter_mut().for_each(|s| *s *= gain);
            mono = limit(&mono, VOLUME_LIMIT, rate, false);
        }
        mono.resize(
            mono.len() + (self.gap.as_secs_f64() * rate as f64).round() as usize,
            0.0,
        );
        let mut out = Vec::with_capacity(mono.len() * channels as usize);
        map_channels(&mono, 1, channels as usize, &mut out);
        Ok(out)
    }
}

fn limit(input: &[f32], ceiling: f64, rate: u32, true_peak: bool) -> Vec<f32> {
    let mut l = Limiter::new(ceiling, rate, 1, true_peak);
    let mut out = Vec::with_capacity(input.len());
    l.process(input, &mut out);
    l.flush(&mut out);
    out
}

/// The built-in chime: each note a sine plus a soft octave overtone with an
/// exponential decay, 0.35 s apart, 2 s in all.
fn built_in(rate: u32) -> Vec<f32> {
    const NOTES: [(f64, f64); 3] = [(523.25, 0.0), (659.25, 0.35), (783.99, 0.70)];
    (0..(rate as f64 * 2.0) as usize)
        .map(|i| {
            let t = i as f64 / rate as f64;
            NOTES
                .iter()
                .filter(|&&(_, start)| t >= start)
                .map(|&(f, start)| {
                    let t = t - start;
                    (0.30 * (2.0 * PI * f * t).sin() + 0.08 * (2.0 * PI * 2.0 * f * t).sin())
                        * (-3.5 * t).exp()
                })
                .sum::<f64>() as f32
        })
        .collect()
}

fn load_mono(path: &std::path::Path, rate: u32) -> Result<Vec<f32>> {
    let mut f = AudioFile::open(path)?;
    let (file_rate, file_channels) = (f.sample_rate(), f.channels());
    let max = (MAX_SECONDS * file_rate as u64) as usize * file_channels as usize;
    let mut samples = Vec::new();
    let mut buf = vec![0.0f32; 4096 * file_channels as usize];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 || samples.len() >= max {
            break;
        }
        samples.extend_from_slice(&buf[..n]);
    }
    samples.truncate(max);
    let mut conv = Converter::new(file_rate, file_channels, rate, 1);
    let mut out = Vec::new();
    conv.process(&samples, &mut out);
    conv.flush(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(s: &[f32]) -> f64 {
        s.iter().fold(0f32, |m, v| m.max(v.abs())) as f64
    }

    #[test]
    fn built_in_chime_is_normalised_with_gap() {
        let rate = 8000;
        let chime = Chime {
            source: ChimeSource::BuiltIn,
            gap: Duration::from_millis(400),
        };
        let band = Band {
            narrow: true,
            lowpass_hz: Some(3400.0),
        };
        let out = chime.render(band, None, rate, 1).unwrap();
        assert_eq!(out.len(), (2.4 * rate as f64) as usize);
        // The gap is silent.
        assert!(out[(2.0 * rate as f64) as usize..]
            .iter()
            .all(|&s| s == 0.0));
        let mut meter = LoudnessMeter::new(rate, 1);
        meter.push(&out[..2 * rate as usize]);
        let lufs = meter.integrated().unwrap();
        assert!((lufs - LOUDNESS_LUFS).abs() < 1.5, "{lufs} LUFS");
        assert!(peak(&out) <= db_to_lin(TRUE_PEAK_DBTP) + 1e-6);
    }

    #[test]
    fn volume_and_channels_apply() {
        let band = Band {
            narrow: false,
            lowpass_hz: None,
        };
        let chime = Chime {
            source: ChimeSource::BuiltIn,
            gap: Duration::ZERO,
        };
        let base = chime.render(band, None, 16_000, 1).unwrap();
        let quiet = chime.render(band, Some(-6.0), 16_000, 2).unwrap();
        assert_eq!(quiet.len(), base.len() * 2);
        assert!(quiet.chunks(2).all(|f| f[0] == f[1]));
        let ratio = peak(&quiet) / peak(&base);
        assert!((ratio - db_to_lin(-6.0)).abs() < 0.02, "{ratio}");
    }
}
