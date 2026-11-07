//! Chimes played before an announcement (as in `mcast-play.sh -C`) and,
//! optionally, after it.
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
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

const LOUDNESS_LUFS: f64 = -18.0;
const TRUE_PEAK_DBTP: f64 = -3.0;
const HIGHPASS_HZ: f64 = 200.0;
/// Longest tone file we'll load; a chime is a few seconds at most.
const MAX_SECONDS: u64 = 30;

/// The built-in chimes. Every fundamental lies within 500-1600 Hz so they
/// sound the same on G.711 as on wideband codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BuiltIn {
    /// Three rising bell notes, C5-E5-G5: the classic "attention" chime.
    #[default]
    Rising,
    /// The same notes falling, G5-E5-C5: the classic "end of page" chime.
    Falling,
    /// Two notes, E5 then C5, like a doorbell.
    DingDong,
    /// Four rising notes, C5-E5-G5-C6.
    Arpeggio,
    /// A single struck bell (A5) with a long ring.
    Bell,
    /// Four quick alternating notes (B5/G5) for urgent pages.
    Alert,
}

impl BuiltIn {
    pub const ALL: [BuiltIn; 6] = [
        BuiltIn::Rising,
        BuiltIn::Falling,
        BuiltIn::DingDong,
        BuiltIn::Arpeggio,
        BuiltIn::Bell,
        BuiltIn::Alert,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BuiltIn::Rising => "rising",
            BuiltIn::Falling => "falling",
            BuiltIn::DingDong => "ding-dong",
            BuiltIn::Arpeggio => "arpeggio",
            BuiltIn::Bell => "bell",
            BuiltIn::Alert => "alert",
        }
    }

    /// Comma-separated names, for help and error messages.
    pub fn names() -> String {
        Self::ALL.map(Self::name).join(", ")
    }

    fn voice(self) -> Voice {
        const C5: f64 = 523.25;
        const E5: f64 = 659.25;
        const G5: f64 = 783.99;
        const A5: f64 = 880.0;
        const B5: f64 = 987.77;
        const C6: f64 = 1046.50;
        match self {
            BuiltIn::Rising => Voice::bell(&[(C5, 0.0), (E5, 0.35), (G5, 0.70)], 3.5, 2.0),
            BuiltIn::Falling => Voice::bell(&[(G5, 0.0), (E5, 0.35), (C5, 0.70)], 3.5, 2.0),
            BuiltIn::DingDong => Voice::bell(&[(E5, 0.0), (C5, 0.55)], 2.6, 2.3),
            BuiltIn::Arpeggio => {
                Voice::bell(&[(C5, 0.0), (E5, 0.25), (G5, 0.50), (C6, 0.75)], 3.2, 2.2)
            }
            BuiltIn::Bell => Voice {
                notes: &[(A5, 0.0)],
                decay: 1.6,
                seconds: 2.5,
                // A struck bell's partials are inharmonic.
                partials: &[(1.0, 0.30), (2.0, 0.10), (2.76, 0.06)],
            },
            BuiltIn::Alert => {
                Voice::bell(&[(B5, 0.0), (G5, 0.18), (B5, 0.36), (G5, 0.54)], 7.0, 1.2)
            }
        }
    }
}

impl fmt::Display for BuiltIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for BuiltIn {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        Self::ALL
            .into_iter()
            .find(|b| b.name() == s || (s == "dingdong" && *b == BuiltIn::DingDong))
            .ok_or_else(|| format!("unknown chime '{s}' (built-in chimes: {})", Self::names()))
    }
}

/// How a built-in chime is synthesised: notes (frequency, start in seconds),
/// each a sum of partials (multiple of the note, amplitude) with an
/// exponential decay.
struct Voice {
    notes: &'static [(f64, f64)],
    decay: f64,
    seconds: f64,
    partials: &'static [(f64, f64)],
}

impl Voice {
    /// A sine plus a soft octave overtone.
    const fn bell(notes: &'static [(f64, f64)], decay: f64, seconds: f64) -> Self {
        Voice {
            notes,
            decay,
            seconds,
            partials: &[(1.0, 0.30), (2.0, 0.08)],
        }
    }

    fn render(&self, rate: u32) -> Vec<f32> {
        (0..(rate as f64 * self.seconds) as usize)
            .map(|i| {
                let t = i as f64 / rate as f64;
                self.notes
                    .iter()
                    .filter(|&&(_, start)| t >= start)
                    .map(|&(f, start)| {
                        let t = t - start;
                        self.partials
                            .iter()
                            .map(|&(m, a)| a * (2.0 * PI * m * f * t).sin())
                            .sum::<f64>()
                            * (-self.decay * t).exp()
                    })
                    .sum::<f64>() as f32
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChimeSource {
    BuiltIn(BuiltIn),
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
    /// Render the tone then the gap (a chime before the announcement),
    /// interleaved at `rate` with `channels` channels.
    pub fn render(
        &self,
        band: Band,
        volume_db: Option<f64>,
        rate: u32,
        channels: u16,
    ) -> Result<Vec<f32>> {
        let mut mono = self.tone(band, volume_db, rate)?;
        mono.resize(mono.len() + self.gap_frames(rate), 0.0);
        Ok(interleave(&mono, channels))
    }

    /// Render the gap then the tone (a chime after the announcement).
    pub fn render_end(
        &self,
        band: Band,
        volume_db: Option<f64>,
        rate: u32,
        channels: u16,
    ) -> Result<Vec<f32>> {
        let mut mono = vec![0.0; self.gap_frames(rate)];
        mono.extend(self.tone(band, volume_db, rate)?);
        Ok(interleave(&mono, channels))
    }

    fn gap_frames(&self, rate: u32) -> usize {
        (self.gap.as_secs_f64() * rate as f64).round() as usize
    }

    /// The mono tone, band-limited, normalised and with any volume change.
    fn tone(&self, band: Band, volume_db: Option<f64>, rate: u32) -> Result<Vec<f32>> {
        let mut mono = match &self.source {
            ChimeSource::BuiltIn(b) => b.voice().render(rate),
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
        Ok(mono)
    }
}

fn interleave(mono: &[f32], channels: u16) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * channels as usize);
    map_channels(mono, 1, channels as usize, &mut out);
    out
}

fn limit(input: &[f32], ceiling: f64, rate: u32, true_peak: bool) -> Vec<f32> {
    let mut l = Limiter::new(ceiling, rate, 1, true_peak);
    let mut out = Vec::with_capacity(input.len());
    l.process(input, &mut out);
    l.flush(&mut out);
    out
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
            source: ChimeSource::BuiltIn(BuiltIn::Rising),
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
            source: ChimeSource::BuiltIn(BuiltIn::Rising),
            gap: Duration::ZERO,
        };
        let base = chime.render(band, None, 16_000, 1).unwrap();
        let quiet = chime.render(band, Some(-6.0), 16_000, 2).unwrap();
        assert_eq!(quiet.len(), base.len() * 2);
        assert!(quiet.chunks(2).all(|f| f[0] == f[1]));
        let ratio = peak(&quiet) / peak(&base);
        assert!((ratio - db_to_lin(-6.0)).abs() < 0.02, "{ratio}");
    }

    #[test]
    fn every_built_in_is_normalised_and_in_band() {
        let rate = 8000;
        let band = Band {
            narrow: true,
            lowpass_hz: Some(3400.0),
        };
        for b in BuiltIn::ALL {
            let voice = b.voice();
            assert!(
                voice
                    .notes
                    .iter()
                    .all(|&(f, _)| (500.0..=1600.0).contains(&f)),
                "{b}"
            );
            let chime = Chime {
                source: ChimeSource::BuiltIn(b),
                gap: Duration::ZERO,
            };
            let out = chime.render(band, None, rate, 1).unwrap();
            assert_eq!(out.len(), (voice.seconds * rate as f64) as usize, "{b}");
            let mut meter = LoudnessMeter::new(rate, 1);
            meter.push(&out);
            let lufs = meter.integrated().unwrap();
            assert!((lufs - LOUDNESS_LUFS).abs() < 1.5, "{b}: {lufs} LUFS");
            assert!(peak(&out) <= db_to_lin(TRUE_PEAK_DBTP) + 1e-6, "{b}");
        }
    }

    #[test]
    fn end_chime_puts_the_gap_first() {
        let rate = 8000;
        let band = Band {
            narrow: true,
            lowpass_hz: Some(3400.0),
        };
        let chime = Chime {
            source: ChimeSource::BuiltIn(BuiltIn::Falling),
            gap: Duration::from_millis(500),
        };
        let start = chime.render(band, None, rate, 1).unwrap();
        let end = chime.render_end(band, None, rate, 1).unwrap();
        assert_eq!(start.len(), end.len());
        assert!(end[..rate as usize / 2].iter().all(|&s| s == 0.0));
        assert_eq!(
            &end[rate as usize / 2..],
            &start[..start.len() - rate as usize / 2]
        );
    }

    #[test]
    fn parses_names() {
        for b in BuiltIn::ALL {
            assert_eq!(b.name().parse::<BuiltIn>(), Ok(b));
        }
        assert_eq!("Ding Dong".parse::<BuiltIn>(), Ok(BuiltIn::DingDong));
        assert_eq!("dingdong".parse::<BuiltIn>(), Ok(BuiltIn::DingDong));
        assert!("klaxon".parse::<BuiltIn>().is_err());
    }
}
