//! A soft, slowly changing chord ("pad") to use as a test source in place of
//! a plain sine: easy to listen to for a long time, yet steady enough that a
//! dropped packet or click stands out.
//!
//! Four chords (Cmaj7, Am7, Fmaj7, G6) of 8 s each loop every 32 s. Notes
//! two chords share are held across the change; the others cross-fade over
//! 3 s. Each note is a pair of sines detuned by a fraction of a hertz and
//! panned slightly apart, which gives a slow chorus and some stereo width,
//! with its own slow swell. The bass carries a little of its 2nd and 3rd
//! harmonics so it can still be heard on a phone's small speaker, and all
//! the upper notes lie between 290 and 530 Hz, inside G.711's band.

use std::f64::consts::TAU;

/// Length of each chord.
const CHORD_SECONDS: f64 = 8.0;
/// Cross-fade between chords, centred on the change.
const CROSSFADE_SECONDS: f64 = 3.0;
/// Fade-in when the pad starts.
const FADE_IN_SECONDS: f64 = 2.0;
/// Spread of the detuned pair, in Hz either side of the note.
const DETUNE_HZ: f64 = 0.15;
/// Depth of each note's slow swell.
const SWELL_DEPTH: f64 = 0.25;

/// MIDI note numbers: bass, then the upper voices.
const CHORDS: [[u8; 5]; 4] = [
    [48, 55, 64, 67, 71], // Cmaj7: C3 G3 E4 G4 B4
    [45, 52, 64, 67, 72], // Am7:   A2 E3 E4 G4 C5
    [41, 60, 64, 69, 72], // Fmaj7: F2 C4 E4 A4 C5
    [43, 62, 64, 67, 71], // G6:    G2 D4 E4 G4 B4
];

/// Harmonics of a bass note (number, level).
const BASS_HARMONICS: [(f64, f64); 3] = [(1.0, 1.0), (2.0, 0.35), (3.0, 0.15)];

struct Voice {
    note: u8,
    /// Oscillator phase steps (radians per sample) for each harmonic of the
    /// left- and right-leaning halves of the detuned pair.
    steps: Vec<(f64, f64)>,
    phases: Vec<(f64, f64)>,
    levels: Vec<f64>,
    swell_step: f64,
    swell_phase: f64,
    /// Overall level, before the pad's normalisation.
    weight: f64,
}

/// Endless stereo pad at a given sample rate.
pub struct Pad {
    rate: f64,
    voices: Vec<Voice>,
    /// Sample index.
    pos: u64,
    scale: f64,
}

fn midi_hz(note: u8) -> f64 {
    440.0 * 2f64.powf((note as f64 - 69.0) / 12.0)
}

/// 0 → 1 over `x` in 0..1, with zero slope at both ends.
fn ease(x: f64) -> f64 {
    0.5 - 0.5 * (x.clamp(0.0, 1.0) * std::f64::consts::PI).cos()
}

impl Pad {
    /// `amplitude` is the peak the pad never exceeds (linear, 1.0 = full scale).
    pub fn new(rate: u32, amplitude: f32) -> Self {
        let rate = rate as f64;
        let bass: Vec<u8> = CHORDS.iter().map(|c| c[0]).collect();
        let mut notes: Vec<u8> = CHORDS.iter().flatten().copied().collect();
        notes.sort_unstable();
        notes.dedup();
        let voices: Vec<Voice> = notes
            .into_iter()
            .enumerate()
            .map(|(i, note)| {
                let f = midi_hz(note);
                let harmonics: &[(f64, f64)] = if bass.contains(&note) {
                    &BASS_HARMONICS
                } else {
                    &BASS_HARMONICS[..1]
                };
                let step = |hz: f64| TAU * hz / rate;
                Voice {
                    note,
                    steps: harmonics
                        .iter()
                        .map(|&(h, _)| (step(h * (f - DETUNE_HZ)), step(h * (f + DETUNE_HZ))))
                        .collect(),
                    // Spread the starting phases so the notes don't all line up.
                    phases: harmonics
                        .iter()
                        .map(|_| (i as f64 * 1.3, i as f64 * 2.1 + 0.7))
                        .collect(),
                    levels: harmonics.iter().map(|&(_, l)| l).collect(),
                    // 0.05-0.12 Hz, different for each note.
                    swell_step: step(0.05 + 0.07 * ((i * 7) % 11) as f64 / 10.0),
                    swell_phase: i as f64 * 0.9,
                    // Bass a little under the upper voices; higher notes slightly
                    // softer, so the top of the chord doesn't poke out.
                    weight: if bass.contains(&note) {
                        0.8
                    } else {
                        1.0 - (note as f64 - 60.0) * 0.015
                    },
                }
            })
            .collect();
        // Worst case: every voice of a chord at the top of its swell, every
        // harmonic in phase. During a cross-fade each note's gain is
        // (1-x)·[in this chord] + x·[in the next], so the total is the same
        // blend of the two chords' totals and never exceeds the louder one.
        let peak = CHORDS
            .iter()
            .map(|chord| {
                chord
                    .iter()
                    .map(|n| {
                        let v = voices.iter().find(|v| v.note == *n).unwrap();
                        v.weight * v.levels.iter().sum::<f64>() * (1.0 + SWELL_DEPTH)
                    })
                    .sum::<f64>()
            })
            .fold(0.0, f64::max);
        let scale = amplitude as f64 / peak;
        Self {
            rate,
            voices,
            pos: 0,
            scale,
        }
    }

    /// Gain of `note` at time `t` seconds, following the chord sequence.
    fn gain(note: u8, t: f64) -> f64 {
        let cycle = CHORD_SECONDS * CHORDS.len() as f64;
        let p = t % cycle;
        let i = (p / CHORD_SECONDS) as usize % CHORDS.len();
        let u = p - i as f64 * CHORD_SECONDS;
        let has = |c: usize| CHORDS[c % CHORDS.len()].contains(&note) as u8 as f64;
        let half = CROSSFADE_SECONDS / 2.0;
        if u >= CHORD_SECONDS - half {
            let x = ease((u - (CHORD_SECONDS - half)) / CROSSFADE_SECONDS);
            has(i) * (1.0 - x) + has(i + 1) * x
        } else if u < half {
            let x = ease((u + half) / CROSSFADE_SECONDS);
            has(i + CHORDS.len() - 1) * (1.0 - x) + has(i) * x
        } else {
            has(i)
        }
    }

    /// Next stereo frame.
    pub fn next_frame(&mut self) -> (f32, f32) {
        // Start half a cross-fade into the first chord, so it begins whole
        // rather than fading in from the last one.
        let t = self.pos as f64 / self.rate + CROSSFADE_SECONDS / 2.0;
        let fade = ease(self.pos as f64 / (FADE_IN_SECONDS * self.rate));
        self.pos += 1;
        let (mut l, mut r) = (0.0, 0.0);
        for v in &mut self.voices {
            let g = Self::gain(v.note, t);
            let swell = 1.0 + SWELL_DEPTH * v.swell_phase.sin();
            v.swell_phase = (v.swell_phase + v.swell_step) % TAU;
            for ((phase, step), level) in v.phases.iter_mut().zip(&v.steps).zip(&v.levels) {
                if g > 0.0 {
                    let a = g * swell * level * v.weight;
                    let (x, y) = (phase.0.sin() * a, phase.1.sin() * a);
                    l += 0.7 * x + 0.3 * y;
                    r += 0.3 * x + 0.7 * y;
                }
                // Keep the oscillators running while silent so a note comes
                // back in where it would have been.
                phase.0 = (phase.0 + step.0) % TAU;
                phase.1 = (phase.1 + step.1) % TAU;
            }
        }
        let k = self.scale * fade;
        ((l * k) as f32, (r * k) as f32)
    }

    /// Fill an interleaved buffer: left on even channels, right on odd ones,
    /// their average for mono.
    pub fn fill(&mut self, out: &mut [f32], channels: usize) {
        for frame in out.chunks_exact_mut(channels) {
            let (l, r) = self.next_frame();
            if channels == 1 {
                frame[0] = 0.5 * (l + r);
            } else {
                for (c, s) in frame.iter_mut().enumerate() {
                    *s = if c % 2 == 0 { l } else { r };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(rate: u32, amplitude: f32, secs: f64) -> Vec<f32> {
        let mut pad = Pad::new(rate, amplitude);
        let mut out = vec![0.0; (rate as f64 * secs) as usize * 2];
        pad.fill(&mut out, 2);
        out
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn stays_under_the_level_and_is_audible() {
        let a = 10f32.powf(-12.0 / 20.0);
        let s = render(48_000, a, 70.0);
        let p = peak(&s);
        assert!(p <= a, "peak {p} over {a}");
        // Not so cautious that it's barely there.
        assert!(p > a * 0.35, "peak {p} too far under {a}");
    }

    #[test]
    fn fades_in_and_has_no_jumps() {
        let s = render(8_000, 0.5, 40.0);
        assert!(peak(&s[..20]) < 1e-3);
        // Highest note is ~530 Hz plus bass harmonics under 300 Hz, so at
        // 8 kHz neighbouring samples can't differ by more than 2π·f/rate of
        // the peak; a click or a hard chord change would.
        let max_step = s
            .chunks_exact(2)
            .zip(s.chunks_exact(2).skip(1))
            .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
            .fold(0f32, f32::max);
        assert!(
            max_step < 0.5 * TAU as f32 * 560.0 / 8_000.0,
            "step {max_step}"
        );
    }

    #[test]
    fn gains_are_continuous_and_bounded() {
        for note in [41, 45, 48, 52, 55, 60, 62, 64, 67, 69, 71, 72] {
            let mut prev = Pad::gain(note, 0.0);
            for i in 1..64_000 {
                let g = Pad::gain(note, i as f64 * 0.001);
                assert!((0.0..=1.0).contains(&g));
                assert!((g - prev).abs() < 0.002, "note {note} jumps at {i} ms");
                prev = g;
            }
        }
        // E4 is in every chord: held, never faded.
        assert!((0..64_000).all(|i| Pad::gain(64, i as f64 * 0.001) == 1.0));
    }

    #[test]
    fn mono_is_the_average() {
        let mut a = Pad::new(16_000, 0.5);
        let mut b = Pad::new(16_000, 0.5);
        let mut st = vec![0.0; 2000];
        let mut mono = vec![0.0; 1000];
        a.fill(&mut st, 2);
        b.fill(&mut mono, 1);
        for (m, f) in mono.iter().zip(st.chunks_exact(2)) {
            assert!((m - 0.5 * (f[0] + f[1])).abs() < 1e-6);
        }
    }
}
