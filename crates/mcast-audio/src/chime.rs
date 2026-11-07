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

/// What kind of sound a built-in chime is, for grouping them in lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    /// Tuned notes: attention and end-of-page chimes.
    Chime,
    /// Struck and rung bells and other percussion.
    Bell,
    /// Electronic beeps and tones.
    Tone,
    /// Sirens, horns and alarms, for urgent pages.
    Alert,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Chime,
        Category::Bell,
        Category::Tone,
        Category::Alert,
    ];

    /// Short name, as in the API: "chime", "bell", "tone" or "alert".
    pub fn name(self) -> &'static str {
        match self {
            Category::Chime => "chime",
            Category::Bell => "bell",
            Category::Tone => "tone",
            Category::Alert => "alert",
        }
    }

    /// Heading for a group of them in a list.
    pub fn label(self) -> &'static str {
        match self {
            Category::Chime => "Chimes",
            Category::Bell => "Bells",
            Category::Tone => "Beeps and tones",
            Category::Alert => "Alerts and sirens",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

macro_rules! built_ins {
    ($($(#[$doc:meta])* $variant:ident = $name:literal, $cat:ident, $about:literal;)*) => {
        /// The built-in chimes. Every fundamental (and every end of a
        /// glide) lies within 500-1600 Hz so they sound the same on G.711 as
        /// on wideband codecs.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub enum BuiltIn {
            $($(#[$doc])* $variant,)*
        }

        impl BuiltIn {
            /// Every built-in, grouped by category.
            pub const ALL: &'static [BuiltIn] = &[$(BuiltIn::$variant,)*];

            /// The name requests use, such as "ding-dong".
            pub fn name(self) -> &'static str {
                match self {
                    $(BuiltIn::$variant => $name,)*
                }
            }

            pub fn category(self) -> Category {
                match self {
                    $(BuiltIn::$variant => Category::$cat,)*
                }
            }

            /// What it sounds like, in a few words.
            pub fn description(self) -> &'static str {
                match self {
                    $(BuiltIn::$variant => $about,)*
                }
            }
        }
    };
}

built_ins! {
    #[default]
    Rising = "rising", Chime, "Three rising notes, C5-E5-G5: the classic attention chime";
    Falling = "falling", Chime, "The same notes falling, G5-E5-C5: the classic end-of-page chime";
    DingDong = "ding-dong", Chime, "Two notes, E5 then C5, like a doorbell";
    Arpeggio = "arpeggio", Chime, "Four rising notes, C5-E5-G5-C6";
    ArpeggioDown = "arpeggio-down", Chime, "Four falling notes, C6-G5-E5-C5";
    BingBong = "bing-bong", Chime, "Two slow notes, A5 then F5, as at a station or airport";
    DingDangDong = "ding-dang-dong", Chime, "Three slow falling notes, C6-A5-F5";
    Westminster = "westminster", Chime, "The Westminster quarter phrase, B5-A5-G5-D5, as from a clock tower";
    Triad = "triad", Chime, "A C-major chord, struck all at once";
    Gentle = "gentle", Chime, "Three soft, pure notes, D5-F#5-A5, slowly";
    Welcome = "welcome", Chime, "Four bright rising notes, E5-G5-C6-E6";
    Farewell = "farewell", Chime, "Four bright falling notes, E6-C6-G5-E5";
    Pentatonic = "pentatonic", Chime, "A quick rising run, C5 to C6";
    Question = "question", Chime, "Two notes rising a fifth, C5-G5";
    Fanfare = "fanfare", Chime, "A short brassy fanfare, G5-G5-G5-C6";
    Bell = "bell", Bell, "A single struck bell (A5) with a long ring";
    ChurchBell = "church-bell", Bell, "A deep church bell, D5, with its hum and a long ring";
    TubularBell = "tubular-bell", Bell, "An orchestral tubular bell, E5";
    SchoolBell = "school-bell", Bell, "An electric school bell ringing for two seconds";
    Telephone = "telephone", Bell, "An old telephone bell: two rings";
    ShipsBell = "ships-bell", Bell, "A ship's bell struck in two pairs";
    Handbell = "handbell", Bell, "A handbell, shaken a few times";
    BicycleBell = "bicycle-bell", Bell, "A bicycle bell, rung twice";
    Triangle = "triangle", Bell, "An orchestral triangle with a shimmering ring";
    Glockenspiel = "glockenspiel", Bell, "Three glockenspiel notes, C6-E6-G6";
    Marimba = "marimba", Bell, "Four mellow marimba notes, C5-E5-G5-C6";
    MusicBox = "music-box", Bell, "A tinkling music-box phrase";
    SingingBowl = "singing-bowl", Bell, "A singing bowl: a soft, slowly beating ring";
    WindChimes = "wind-chimes", Bell, "Wind chimes, a scatter of high notes";
    Cowbell = "cowbell", Bell, "A cowbell, struck twice";
    Beep = "beep", Tone, "A single 1 kHz beep";
    DoubleBeep = "double-beep", Tone, "Two short 1 kHz beeps";
    TripleBeep = "triple-beep", Tone, "Three short 1 kHz beeps";
    Pips = "pips", Tone, "Time-signal pips: five short and one long";
    Blip = "blip", Tone, "A quick rising blip";
    Notify = "notify", Tone, "Two short high notes, like a message notification";
    Error = "error", Tone, "Two low buzzy notes falling: 'no'";
    Sonar = "sonar", Tone, "A sonar ping with an echo";
    Countdown = "countdown", Tone, "Three short beeps then a long high one, like a race start";
    Steady = "steady", Tone, "A steady 1 kHz tone for two seconds";
    Alert = "alert", Alert, "Four quick alternating notes (B5/G5) for urgent pages";
    SirenWail = "siren-wail", Alert, "A siren slowly wailing up and down";
    SirenYelp = "siren-yelp", Alert, "A siren yelping quickly up and down";
    HiLo = "hi-lo", Alert, "Alternating high and low tones";
    Klaxon = "klaxon", Alert, "A rasping horn, 'a-oo-ga', twice";
    Buzzer = "buzzer", Alert, "A harsh buzzer";
    Alarm = "alarm", Alert, "Fast beeping, like an alarm clock";
    Whoop = "whoop", Alert, "A rising whoop, twice: an evacuation tone";
    Warble = "warble", Alert, "A fast-warbling alarm tone";
}

impl BuiltIn {
    /// Comma-separated names, for help and error messages.
    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|b| b.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn voice(self) -> Voice {
        use BuiltIn::*;
        const C5: f64 = 523.25;
        const D5: f64 = 587.33;
        const E5: f64 = 659.25;
        const F5: f64 = 698.46;
        const FS5: f64 = 739.99;
        const G5: f64 = 783.99;
        const A5: f64 = 880.0;
        const B5: f64 = 987.77;
        const C6: f64 = 1046.50;
        const D6: f64 = 1174.66;
        const E6: f64 = 1318.51;
        const G6: f64 = 1567.98;
        match self {
            Rising => Voice::struck(&[(C5, 0.0), (E5, 0.35), (G5, 0.70)], 3.5, 2.0, SOFT),
            Falling => Voice::struck(&[(G5, 0.0), (E5, 0.35), (C5, 0.70)], 3.5, 2.0, SOFT),
            DingDong => Voice::struck(&[(E5, 0.0), (C5, 0.55)], 2.6, 2.3, SOFT),
            Arpeggio => Voice::struck(
                &[(C5, 0.0), (E5, 0.25), (G5, 0.50), (C6, 0.75)],
                3.2,
                2.2,
                SOFT,
            ),
            ArpeggioDown => Voice::struck(
                &[(C6, 0.0), (G5, 0.25), (E5, 0.50), (C5, 0.75)],
                3.2,
                2.2,
                SOFT,
            ),
            BingBong => Voice::struck(&[(A5, 0.0), (F5, 0.7)], 2.2, 2.8, BELL),
            DingDangDong => Voice::struck(&[(C6, 0.0), (A5, 0.5), (F5, 1.0)], 2.4, 2.8, BELL),
            Westminster => Voice::struck(
                &[(B5, 0.0), (A5, 0.45), (G5, 0.90), (D5, 1.35)],
                2.2,
                3.2,
                BELL,
            ),
            Triad => Voice::struck(&[(C5, 0.0), (E5, 0.0), (G5, 0.0)], 2.0, 2.5, SOFT),
            Gentle => Voice::struck(&[(D5, 0.0), (FS5, 0.5), (A5, 1.0)], 2.0, 3.0, PURE),
            Welcome => Voice::struck(
                &[(E5, 0.0), (G5, 0.2), (C6, 0.4), (E6, 0.6)],
                3.0,
                2.2,
                SOFT,
            ),
            Farewell => Voice::struck(
                &[(E6, 0.0), (C6, 0.2), (G5, 0.4), (E5, 0.6)],
                3.0,
                2.2,
                SOFT,
            ),
            Pentatonic => Voice::struck(
                &[
                    (C5, 0.0),
                    (D5, 0.12),
                    (E5, 0.24),
                    (G5, 0.36),
                    (A5, 0.48),
                    (C6, 0.60),
                ],
                4.0,
                2.0,
                SOFT,
            ),
            Question => Voice::struck(&[(C5, 0.0), (G5, 0.3)], 3.5, 1.6, SOFT),
            Fanfare => Voice::held(
                &[
                    (G5, 0.0, 0.14),
                    (G5, 0.18, 0.14),
                    (G5, 0.36, 0.14),
                    (C6, 0.54, 0.9),
                ],
                1.8,
                BRASS,
            ),
            Bell => Voice::struck(&[(A5, 0.0)], 1.6, 2.5, BELL),
            ChurchBell => Voice::struck(&[(D5, 0.0)], 0.9, 4.0, CHURCH),
            TubularBell => Voice::struck(&[(E5, 0.0)], 1.2, 3.5, TUBULAR),
            SchoolBell => Voice::held(&[(D6, 0.0, 2.0)], 2.4, BELL).tremolo(20.0, 0.8),
            Telephone => Voice::held(
                &[
                    (C6, 0.0, 0.9),
                    (E6, 0.0, 0.9),
                    (C6, 1.3, 0.9),
                    (E6, 1.3, 0.9),
                ],
                2.4,
                PURE,
            )
            .tremolo(20.0, 1.0),
            ShipsBell => Voice::struck(
                &[(B5, 0.0), (B5, 0.35), (B5, 1.2), (B5, 1.55)],
                2.2,
                3.2,
                BELL,
            ),
            Handbell => Voice::struck(
                &[(G5, 0.0), (G5, 0.22), (G5, 0.44), (G5, 0.66), (G5, 0.88)],
                3.0,
                2.0,
                HANDBELL,
            ),
            BicycleBell => Voice::struck(
                &[
                    (E6, 0.0),
                    (E6, 0.08),
                    (E6, 0.16),
                    (E6, 0.5),
                    (E6, 0.58),
                    (E6, 0.66),
                ],
                6.0,
                1.4,
                BELL,
            ),
            Triangle => Voice::struck(&[(C6, 0.0)], 1.0, 2.5, TRIANGLE),
            Glockenspiel => {
                Voice::struck(&[(C6, 0.0), (E6, 0.18), (G6, 0.36)], 2.5, 2.0, GLOCKENSPIEL)
            }
            Marimba => Voice::struck(
                &[(C5, 0.0), (E5, 0.15), (G5, 0.30), (C6, 0.45)],
                6.0,
                1.4,
                MARIMBA,
            ),
            MusicBox => Voice::struck(
                &[(C6, 0.0), (E6, 0.22), (G6, 0.44), (E6, 0.66), (C6, 0.88)],
                3.5,
                2.4,
                MUSIC_BOX,
            ),
            SingingBowl => Voice::struck(&[(550.0, 0.0), (552.5, 0.0)], 0.7, 4.0, BOWL),
            WindChimes => Voice::struck(
                &[
                    (E6, 0.0),
                    (C6, 0.17),
                    (G6, 0.29),
                    (D6, 0.52),
                    (A5, 0.61),
                    (E6, 0.83),
                    (C6, 1.02),
                    (G5, 1.3),
                ],
                2.5,
                2.6,
                TUBULAR,
            ),
            Cowbell => Voice::struck(
                &[(562.0, 0.0), (845.0, 0.0), (562.0, 0.3), (845.0, 0.3)],
                9.0,
                1.0,
                COWBELL,
            ),
            Beep => Voice::held(&[(1000.0, 0.0, 0.35)], 0.8, PURE),
            DoubleBeep => Voice::held(&[(1000.0, 0.0, 0.15), (1000.0, 0.25, 0.15)], 0.8, PURE),
            TripleBeep => Voice::held(
                &[
                    (1000.0, 0.0, 0.12),
                    (1000.0, 0.2, 0.12),
                    (1000.0, 0.4, 0.12),
                ],
                0.8,
                PURE,
            ),
            Pips => {
                let mut notes: Vec<_> = (0..5).map(|i| (1000.0, i as f64 * 0.25, 0.08)).collect();
                notes.push((1000.0, 1.25, 0.4));
                Voice::held(&notes, 1.8, PURE)
            }
            Blip => Voice::glide(&[(600.0, 1200.0, 0.0, 0.12)], 0.8, PURE),
            Notify => Voice::held(&[(C6, 0.0, 0.08), (G6, 0.1, 0.14)], 0.8, SOFT),
            Error => Voice::held(&[(F5, 0.0, 0.15), (C5, 0.18, 0.3)], 0.8, SQUARE),
            Sonar => Voice::struck(&[(D6, 0.0)], 3.0, 2.5, PURE).echo(0.6, 0.35),
            Countdown => Voice::held(
                &[
                    (G5, 0.0, 0.12),
                    (G5, 0.5, 0.12),
                    (G5, 1.0, 0.12),
                    (G6, 1.5, 0.45),
                ],
                2.2,
                PURE,
            ),
            Steady => Voice::held(&[(1000.0, 0.0, 2.0)], 2.2, PURE),
            Alert => Voice::struck(
                &[(B5, 0.0), (G5, 0.18), (B5, 0.36), (G5, 0.54)],
                7.0,
                1.2,
                SOFT,
            ),
            SirenWail => Voice::glide(
                &[
                    (600.0, 1400.0, 0.0, 1.0),
                    (1400.0, 600.0, 1.0, 1.0),
                    (600.0, 1400.0, 2.0, 1.0),
                ],
                3.0,
                SOFT,
            ),
            SirenYelp => {
                let notes: Vec<_> = (0..12)
                    .map(|i| {
                        let (from, to) = if i % 2 == 0 {
                            (700.0, 1500.0)
                        } else {
                            (1500.0, 700.0)
                        };
                        (from, to, i as f64 * 0.18, 0.18)
                    })
                    .collect();
                Voice::glide(&notes, 2.2, SOFT)
            }
            HiLo => {
                let notes: Vec<_> = (0..6)
                    .map(|i| (if i % 2 == 0 { 960.0 } else { 770.0 }, i as f64 * 0.5, 0.5))
                    .collect();
                Voice::held(&notes, 3.0, SOFT)
            }
            Klaxon => Voice::glide(
                &[(500.0, 700.0, 0.0, 0.5), (500.0, 700.0, 0.8, 0.5)],
                1.5,
                BRASS,
            ),
            Buzzer => Voice::held(&[(600.0, 0.0, 1.2)], 1.4, SQUARE).tremolo(50.0, 0.5),
            Alarm => {
                let notes: Vec<_> = (0..10).map(|i| (1000.0, i as f64 * 0.2, 0.1)).collect();
                Voice::held(&notes, 2.0, SOFT)
            }
            Whoop => Voice::glide(
                &[(500.0, 1200.0, 0.0, 0.8), (500.0, 1200.0, 1.0, 0.8)],
                2.0,
                SOFT,
            ),
            Warble => {
                let notes: Vec<_> = (0..25)
                    .map(|i| {
                        (
                            if i % 2 == 0 { 800.0 } else { 970.0 },
                            i as f64 * 0.08,
                            0.08,
                        )
                    })
                    .collect();
                Voice::held(&notes, 2.2, SOFT)
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
        let s = s
            .trim()
            .to_ascii_lowercase()
            .replace(['_', ' '], "-")
            .replace('\'', "");
        // Also accept names run together: "dingdong", "churchbell".
        let squash = |n: &str| n.replace('-', "");
        Self::ALL
            .iter()
            .copied()
            .find(|b| b.name() == s || squash(b.name()) == s)
            .ok_or_else(|| format!("unknown chime '{s}' (built-in chimes: {})", Self::names()))
    }
}

/// Partials: (multiple of the note, amplitude, decay relative to the
/// voice's), so a struck sound's upper partials can die away faster.
type Partials = &'static [(f64, f64, f64)];

/// A sine plus a soft octave overtone.
const SOFT: Partials = &[(1.0, 0.30, 1.0), (2.0, 0.08, 1.0)];
const PURE: Partials = &[(1.0, 0.30, 1.0)];
/// A struck bell's partials are inharmonic.
const BELL: Partials = &[(1.0, 0.30, 1.0), (2.0, 0.10, 1.0), (2.76, 0.06, 1.0)];
const CHURCH: Partials = &[
    (0.5, 0.14, 0.6),
    (1.0, 0.30, 1.0),
    (1.19, 0.12, 1.2),
    (1.5, 0.07, 1.4),
    (2.0, 0.10, 1.6),
    (2.66, 0.05, 2.0),
];
const TUBULAR: Partials = &[
    (1.0, 0.30, 1.0),
    (2.76, 0.10, 1.6),
    (5.40, 0.05, 2.4),
    (8.93, 0.02, 3.0),
];
const HANDBELL: Partials = &[(1.0, 0.30, 1.0), (3.0, 0.08, 1.8)];
const TRIANGLE: Partials = &[
    (1.0, 0.15, 1.0),
    (2.13, 0.10, 0.9),
    (3.31, 0.12, 0.8),
    (4.72, 0.08, 0.9),
    (6.19, 0.05, 1.0),
];
const GLOCKENSPIEL: Partials = &[(1.0, 0.30, 1.0), (2.76, 0.06, 2.5), (5.40, 0.03, 4.0)];
const MARIMBA: Partials = &[(1.0, 0.35, 1.0), (3.93, 0.10, 3.0), (9.2, 0.03, 5.0)];
const MUSIC_BOX: Partials = &[(1.0, 0.30, 1.0), (3.0, 0.06, 2.0), (5.4, 0.03, 3.0)];
const BOWL: Partials = &[(1.0, 0.30, 1.0), (2.71, 0.12, 1.3), (4.8, 0.05, 1.8)];
const COWBELL: Partials = &[(1.0, 0.30, 1.0), (3.0, 0.06, 2.0)];
/// Odd harmonics: a hollow, buzzy square-ish wave.
const SQUARE: Partials = &[
    (1.0, 0.30, 1.0),
    (3.0, 0.10, 1.0),
    (5.0, 0.06, 1.0),
    (7.0, 0.043, 1.0),
    (9.0, 0.033, 1.0),
];
/// Every harmonic: a bright, brassy sawtooth-ish wave.
const BRASS: Partials = &[
    (1.0, 0.30, 1.0),
    (2.0, 0.15, 1.0),
    (3.0, 0.10, 1.0),
    (4.0, 0.075, 1.0),
    (5.0, 0.06, 1.0),
];

/// Fade in and out of a held note, so it doesn't click.
const RAMP_SECONDS: f64 = 0.005;

/// One note of a built-in chime.
#[derive(Debug, Clone, Copy)]
struct Note {
    /// Frequency at the start, and at the end of `len` (a glide if
    /// different).
    freq: f64,
    to: f64,
    /// Start, in seconds.
    at: f64,
    /// Length in seconds; a struck note rings to the end of the chime.
    len: f64,
    amp: f64,
}

/// How a built-in chime is synthesised: notes, each a sum of partials;
/// struck notes decay exponentially, held ones hold until they end.
struct Voice {
    notes: Vec<Note>,
    seconds: f64,
    partials: Partials,
    /// Exponential decay rate (per second) of struck notes; `None` for held.
    decay: Option<f64>,
    /// Amplitude modulation (rate in Hz, depth 0-1), as of a bell's hammer.
    tremolo: Option<(f64, f64)>,
}

impl Voice {
    /// Struck notes, (frequency, start), ringing to the end.
    fn struck(notes: &[(f64, f64)], decay: f64, seconds: f64, partials: Partials) -> Self {
        Voice {
            notes: notes
                .iter()
                .map(|&(freq, at)| Note {
                    freq,
                    to: freq,
                    at,
                    len: f64::INFINITY,
                    amp: 1.0,
                })
                .collect(),
            seconds,
            partials,
            decay: Some(decay),
            tremolo: None,
        }
    }

    /// Held notes, (frequency, start, length).
    fn held(notes: &[(f64, f64, f64)], seconds: f64, partials: Partials) -> Self {
        let glides: Vec<_> = notes.iter().map(|&(f, at, len)| (f, f, at, len)).collect();
        Self::glide(&glides, seconds, partials)
    }

    /// Held notes that glide, (from, to, start, length).
    fn glide(notes: &[(f64, f64, f64, f64)], seconds: f64, partials: Partials) -> Self {
        Voice {
            notes: notes
                .iter()
                .map(|&(freq, to, at, len)| Note {
                    freq,
                    to,
                    at,
                    len,
                    amp: 1.0,
                })
                .collect(),
            seconds,
            partials,
            decay: None,
            tremolo: None,
        }
    }

    fn tremolo(mut self, rate: f64, depth: f64) -> Self {
        self.tremolo = Some((rate, depth));
        self
    }

    /// Repeat every note `delay` seconds later, quieter.
    fn echo(mut self, delay: f64, amp: f64) -> Self {
        let echoes: Vec<_> = self
            .notes
            .iter()
            .map(|n| Note {
                at: n.at + delay,
                amp: n.amp * amp,
                ..*n
            })
            .collect();
        self.notes.extend(echoes);
        self
    }

    fn render(&self, rate: u32) -> Vec<f32> {
        let rate_f = rate as f64;
        let frames = (rate_f * self.seconds) as usize;
        let mut out = vec![0f64; frames];
        // Partials at or above this would alias.
        let limit = 0.45 * rate_f;
        for note in &self.notes {
            let first = ((note.at * rate_f).ceil() as usize).min(frames);
            let last = ((note.at + note.len) * rate_f).ceil().min(frames as f64) as usize;
            let sweep = if note.to != note.freq {
                (note.to - note.freq) / note.len
            } else {
                0.0
            };
            for &(m, a, d) in self.partials {
                if m * note.freq.max(note.to) >= limit {
                    continue;
                }
                for (i, s) in out.iter_mut().enumerate().take(last).skip(first) {
                    let t = (i as f64 / rate_f - note.at).max(0.0);
                    let env = match self.decay {
                        Some(decay) => (-decay * d * t).exp(),
                        None => (t / RAMP_SECONDS)
                            .min((note.len - t) / RAMP_SECONDS)
                            .clamp(0.0, 1.0),
                    };
                    let phase = 2.0 * PI * m * (note.freq * t + 0.5 * sweep * t * t);
                    *s += note.amp * a * env * phase.sin();
                }
            }
        }
        if let Some((r, depth)) = self.tremolo {
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f64 / rate_f;
                *s *= 1.0 - depth * 0.5 * (1.0 - (2.0 * PI * r * t).cos());
            }
        }
        out.into_iter().map(|s| s as f32).collect()
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

/// Sample rate of [`preview_wav`]: wide enough for every chime's partials,
/// small enough to send quickly.
pub const PREVIEW_RATE: u32 = 24_000;

/// A built-in chime as a mono 16-bit WAV file, for listening to it before
/// choosing it. Wideband and without a gap: as a page on a wideband codec
/// would play it.
pub fn preview_wav(b: BuiltIn) -> Result<Vec<u8>> {
    let chime = Chime {
        source: ChimeSource::BuiltIn(b),
        gap: Duration::ZERO,
    };
    let band = Band {
        narrow: false,
        lowpass_hz: None,
    };
    let mono = chime.tone(band, None, PREVIEW_RATE)?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: PREVIEW_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = std::io::Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut out, spec)?;
    for s in mono {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)?;
    }
    w.finalize()?;
    Ok(out.into_inner())
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
        let narrow = Band {
            narrow: true,
            lowpass_hz: Some(3400.0),
        };
        let wide = Band {
            narrow: false,
            lowpass_hz: None,
        };
        for &b in BuiltIn::ALL {
            let voice = b.voice();
            assert!(
                voice
                    .notes
                    .iter()
                    .all(|n| [n.freq, n.to].iter().all(|f| (500.0..=1600.0).contains(f))),
                "{b}"
            );
            assert!(!b.description().is_empty(), "{b}");
            for (rate, band) in [(8000, narrow), (48_000, wide)] {
                let chime = Chime {
                    source: ChimeSource::BuiltIn(b),
                    gap: Duration::ZERO,
                };
                let out = chime.render(band, None, rate, 1).unwrap();
                assert_eq!(out.len(), (voice.seconds * rate as f64) as usize, "{b}");
                let mut meter = LoudnessMeter::new(rate, 1);
                meter.push(&out);
                let lufs = meter.integrated().unwrap();
                assert!(
                    (lufs - LOUDNESS_LUFS).abs() < 1.5,
                    "{b} at {rate}: {lufs} LUFS"
                );
                assert!(
                    peak(&out) <= db_to_lin(TRUE_PEAK_DBTP) + 1e-6,
                    "{b} at {rate}"
                );
            }
        }
    }

    #[test]
    fn preview_is_a_wav() {
        let wav = preview_wav(BuiltIn::DingDong).unwrap();
        let r = hound::WavReader::new(std::io::Cursor::new(wav)).unwrap();
        assert_eq!(r.spec().sample_rate, PREVIEW_RATE);
        assert_eq!(r.spec().channels, 1);
        assert_eq!(r.len(), (2.3 * PREVIEW_RATE as f64) as u32);
    }

    #[test]
    fn names_are_unique_and_grouped() {
        let mut seen = std::collections::HashSet::new();
        assert!(BuiltIn::ALL.iter().all(|b| seen.insert(b.name())));
        // ALL lists each category together, in Category order.
        let cats: Vec<_> = BuiltIn::ALL.iter().map(|b| b.category()).collect();
        assert!(cats.windows(2).all(|w| w[0] <= w[1]));
        assert!(Category::ALL.iter().all(|c| cats.contains(c)));
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
        for &b in BuiltIn::ALL {
            assert_eq!(b.name().parse::<BuiltIn>(), Ok(b));
        }
        assert_eq!("Church Bell".parse::<BuiltIn>(), Ok(BuiltIn::ChurchBell));
        assert_eq!("ship's bell".parse::<BuiltIn>(), Ok(BuiltIn::ShipsBell));
        assert_eq!("Ding Dong".parse::<BuiltIn>(), Ok(BuiltIn::DingDong));
        assert_eq!("dingdong".parse::<BuiltIn>(), Ok(BuiltIn::DingDong));
        assert!("foghorn".parse::<BuiltIn>().is_err());
    }
}
