//! The panel's own data: zones and announcements.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Longest id or name accepted.
pub const MAX_NAME: usize = 64;
/// Most times an announcement can repeat.
pub const MAX_REPEAT: u32 = 20;
/// Longest gap between repeats, in seconds.
pub const MAX_REPEAT_GAP_S: f64 = 3600.0;

/// Everything the panel stores.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub zones: Vec<Zone>,
    pub announcements: Vec<Announcement>,
    pub schedules: Vec<Schedule>,
    /// IANA time zone for schedules, e.g. "Europe/London"; unset uses the
    /// system's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// What announcements use for the settings they leave out.
    #[serde(skip_serializing_if = "Defaults::is_empty")]
    pub defaults: Defaults,
    /// While true, no schedule plays (each keeps its own on/off).
    #[serde(skip_serializing_if = "is_false")]
    pub schedules_paused: bool,
    /// Dates schedules don't play on: holidays, breaks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub days_off: Vec<DayOff>,
    /// Media files and folders (paths) left out of the panel's lists.
    /// Announcements that use one still play it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_media: Vec<String>,
    /// Places with their own zones and schedules: buildings, campuses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<Site>,
    /// Sets of schedules switched on and off together.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schedule_groups: Vec<ScheduleGroup>,
    /// Tags on sound files, for finding them: path → tags.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub media_tags: BTreeMap<String, Vec<String>>,
    /// Lists of sounds that play one after another; announcements use them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub playlists: Vec<Playlist>,
    /// How admins arranged the page: a tab's id → its sections' ids, in
    /// order. Sections left out follow in their usual order.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub layout: BTreeMap<String, Vec<String>>,
    /// Headings the Panel groups announcements under.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<Category>,
    /// How the Panel looks, for every screen.
    #[serde(skip_serializing_if = "PanelLook::is_default")]
    pub panel: PanelLook,
    /// Days of play history to keep; 0 keeps it all. Unset: 90.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_days: Option<u32>,
    /// Buttons and contacts on GPIO pins that play announcements.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<Input>,
}

/// A button or contact on a GPIO pin (on a Raspberry Pi, its header) that
/// plays an announcement: a panic button, a door contact, a bell push.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub id: String,
    pub name: String,
    /// The GPIO chip, `gpiochip0` (a Pi's header) unless set: `gpiochipN`
    /// or `/dev/gpiochipN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chip: Option<String>,
    /// The line on that chip. On a Pi, the BCM GPIO number: GPIO17 is
    /// header pin 11.
    pub line: u32,
    /// Pressed when the pin reads low: a switch to ground, the usual
    /// wiring with a pull-up. False: pressed when it reads high.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub active_low: bool,
    /// The pin's pull resistor.
    #[serde(default, skip_serializing_if = "Bias::is_default")]
    pub bias: Bias,
    /// Changes shorter than this are ignored (switch bounce); unset: 50 ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<u32>,
    /// The announcement it plays (id).
    pub announcement: String,
    /// Play while held, and stop on release. Otherwise a press plays it
    /// through, and releasing does nothing.
    #[serde(default, skip_serializing_if = "is_false")]
    pub while_held: bool,
    /// Off: the pin isn't watched.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

/// An input's pull resistor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bias {
    #[default]
    PullUp,
    PullDown,
    /// None: something outside drives the pin.
    None,
}

impl Bias {
    fn is_default(&self) -> bool {
        *self == Bias::PullUp
    }
}

/// Debounce unless an input says otherwise.
pub const DEFAULT_DEBOUNCE_MS: u32 = 50;
/// Most inputs, and the longest debounce.
pub const MAX_INPUTS: usize = 64;
pub const MAX_DEBOUNCE_MS: u32 = 1000;

impl Input {
    /// Its chip's device: `/dev/gpiochip0` unless it says.
    pub fn chip_path(&self) -> String {
        match self
            .chip
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            None => "/dev/gpiochip0".into(),
            Some(c) if c.starts_with('/') => c.to_string(),
            Some(c) => format!("/dev/{c}"),
        }
    }

    pub fn debounce_ms(&self) -> u32 {
        self.debounce_ms.unwrap_or(DEFAULT_DEBOUNCE_MS)
    }
}

/// Longest history that can be asked for, in days (ten years).
pub const MAX_HISTORY_DAYS: u32 = 3650;

impl Config {
    /// Days of play history to keep (0: all of it).
    pub fn history_days(&self) -> u32 {
        self.history_days.unwrap_or(crate::history::DEFAULT_DAYS)
    }
}

/// Colors a button or category can have: named, so the page can keep them
/// readable in light and dark.
pub const COLORS: [&str; 8] = [
    "red", "orange", "amber", "green", "teal", "blue", "purple", "grey",
];

/// Button sizes on the Panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Size {
    Small,
    #[default]
    Medium,
    Large,
}

/// Buttons as tiles, or one row each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelMode {
    #[default]
    Tiles,
    List,
}

fn is_true(b: &bool) -> bool {
    *b
}

/// How the Panel looks. Leaving a field out keeps its default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PanelLook {
    #[serde(skip_serializing_if = "is_medium")]
    pub size: Size,
    #[serde(skip_serializing_if = "is_tiles")]
    pub mode: PanelMode,
    /// The zones and repeat count under each name.
    #[serde(skip_serializing_if = "is_true")]
    pub show_details: bool,
    /// The "Next:" line.
    #[serde(skip_serializing_if = "is_true")]
    pub show_next: bool,
    /// The "Now playing" list.
    #[serde(skip_serializing_if = "is_true")]
    pub show_playing: bool,
    /// The stats strip: plays today and this week, problems in the last day.
    #[serde(skip_serializing_if = "is_true")]
    pub show_stats: bool,
    /// Ask before playing an emergency announcement.
    #[serde(skip_serializing_if = "is_false")]
    pub confirm_emergency: bool,
}

impl Default for PanelLook {
    fn default() -> Self {
        Self {
            size: Size::Medium,
            mode: PanelMode::Tiles,
            show_details: yes(),
            show_next: yes(),
            show_playing: yes(),
            show_stats: yes(),
            confirm_emergency: false,
        }
    }
}

impl PanelLook {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Every field, defaults too (the file leaves those out).
    pub fn full(&self) -> Value {
        json!({
            "size": self.size,
            "mode": self.mode,
            "show_details": self.show_details,
            "show_next": self.show_next,
            "show_playing": self.show_playing,
            "show_stats": self.show_stats,
            "confirm_emergency": self.confirm_emergency,
        })
    }
}

fn is_medium(s: &Size) -> bool {
    *s == Size::Medium
}

fn is_tiles(m: &PanelMode) -> bool {
    *m == PanelMode::Tiles
}

/// Longest icon (an emoji or a few characters), in characters.
pub const MAX_ICON: usize = 4;

fn check_look(what: &str, color: &Option<String>, icon: &Option<String>) -> Result<()> {
    if let Some(c) = color {
        if !COLORS.contains(&c.as_str()) {
            bail!("{what}: no color \"{c}\" (use {})", COLORS.join(", "));
        }
    }
    if let Some(i) = icon {
        let n = i.chars().count();
        if n == 0 || n > MAX_ICON || i.chars().any(|c| c.is_control() || c.is_whitespace()) {
            bail!("{what}: an icon is an emoji or up to {MAX_ICON} characters, no spaces");
        }
    }
    Ok(())
}

/// Most things a layout lists (sections, categories, buttons).
pub const MAX_LAYOUT: usize = 500;
/// Most tags on an announcement.
pub const MAX_TAGS: usize = 20;

/// A heading on the Panel: announcements in it are shown together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Category {
    pub id: String,
    pub name: String,
    /// Its buttons' color, unless one sets its own (one of [`COLORS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Its buttons' size, instead of the Panel's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
}

/// Most items in a playlist.
pub const MAX_PLAYLIST: usize = 100;
/// Longest gap between a playlist's items, in milliseconds.
pub const MAX_PLAYLIST_GAP_MS: u32 = 60_000;

/// Sounds (and later other kinds of page) that play one after another, as
/// one stream, with the announcement's chimes before and after.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub items: Vec<PlaylistItem>,
    /// Silence between items.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gap_ms: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// One thing in a playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlaylistItem {
    /// `{"type": "sound", "media": "bells/lunch.wav"}`.
    Sound { media: String },
    /// `{"type": "text", "text": "The library closes in ten minutes"}`: a
    /// text page, spoken by the mcast server's text to speech (`speak`)
    /// and shown on the displays of the zones it plays to (`show`), as the
    /// playlist reaches it.
    Text {
        text: String,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        speak: bool,
        #[serde(default = "yes", skip_serializing_if = "is_true")]
        show: bool,
        /// The voice to speak it in; unset is the server's default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        /// For a page that's only shown: how long the playlist pauses on
        /// it, in seconds; unset is [`SHOW_SECS`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<u32>,
    },
}

/// How long a page that's only shown holds the playlist, by default.
pub const SHOW_SECS: u32 = 5;
/// The longest it can (the mcast server's longest pause).
pub const MAX_SHOW_SECS: u32 = 60;

/// Longest text page, in characters (as the mcast server speaks).
pub const MAX_TEXT: usize = 2000;

impl PlaylistItem {
    pub fn media(&self) -> Option<&str> {
        match self {
            PlaylistItem::Sound { media } => Some(media),
            PlaylistItem::Text { .. } => None,
        }
    }

    /// Whether it's heard: a sound, or text that's spoken.
    pub fn audible(&self) -> bool {
        match self {
            PlaylistItem::Sound { .. } => true,
            PlaylistItem::Text { speak, .. } => *speak,
        }
    }

    /// Its item in a `files` source: a media name, text to say, or for a
    /// page that's only shown, a pause while it's up.
    fn source_item(&self) -> Option<Value> {
        match self {
            PlaylistItem::Sound { media } => Some(media.as_str().into()),
            PlaylistItem::Text {
                text,
                speak: true,
                voice,
                ..
            } => {
                let mut v = json!({ "say": text.trim() });
                if let Some(voice) = voice {
                    v["voice"] = voice.as_str().into();
                }
                Some(v)
            }
            PlaylistItem::Text {
                show: true,
                seconds,
                ..
            } => Some(json!({ "silence_ms": seconds.unwrap_or(SHOW_SECS) * 1000 })),
            PlaylistItem::Text { .. } => None,
        }
    }
}

impl Playlist {
    /// What to show as each item of the stream starts (by its index in
    /// the `files` source): a text page that's shown goes up as its own
    /// item does. One that's only shown is a pause of its own in the
    /// stream, so it's up for that long before the next item plays.
    pub fn texts_by_item(&self) -> Vec<Vec<String>> {
        self.items
            .iter()
            .filter(|i| i.source_item().is_some())
            .map(|i| match i {
                PlaylistItem::Text {
                    text, show: true, ..
                } => vec![text.trim().to_string()],
                _ => Vec::new(),
            })
            .collect()
    }

    /// Whether anything in it is shown on displays.
    pub fn shows_text(&self) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i, PlaylistItem::Text { show: true, .. }))
    }
}

/// Part of an announcement that sends other audio to other zones, in the
/// same run (a message in two languages to two buildings, say).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// Zone ids.
    pub zones: Vec<String>,
    pub audio: Audio,
}

/// Longest tag, in characters.
pub const MAX_TAG: usize = 32;

/// A tag as stored: trimmed, inner spaces collapsed; `None` if it isn't one
/// (empty, too long, a comma or a control character in it).
pub fn clean_tag(tag: &str) -> Option<String> {
    let t = tag.split_whitespace().collect::<Vec<_>>().join(" ");
    let ok = !t.is_empty()
        && t.chars().count() <= MAX_TAG
        && !t.chars().any(|c| c == ',' || c.is_control());
    ok.then_some(t)
}

/// A place with its own zones and schedules, and its own time zone, days
/// off and pause switch. A site's schedule plays only to its zones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Site {
    pub id: String,
    pub name: String,
    /// IANA time zone for its schedules; unset uses the panel's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    /// Dates its schedules don't play on, besides the panel's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days_off: Vec<DayOff>,
    /// While true, none of its schedules plays.
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

/// Schedules switched on and off together: "Normal bells", "Exam week".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleGroup {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Written in an announcement's `chime` or `end_chime` to have no chime,
/// whatever the default is.
pub const NO_CHIME: &str = "none";

/// The panel's defaults for announcements. Each applies to announcements
/// that don't set that field themselves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    /// Chime before the audio, as in an announcement. Unset: none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chime: Option<Value>,
    /// Chime after the audio. Unset: none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_chime: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    /// Gain on the audio before processing, in dB (not the chimes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_gain_db: Option<f64>,
    /// Normalise loudness even without voice or music processing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalize: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_in_ms: Option<f64>,
    /// Codec for zone groups that don't name their own. Unset: the mcast
    /// server's (L16).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

impl Defaults {
    pub fn is_empty(&self) -> bool {
        *self == Defaults::default()
    }

    /// A zone's target with these defaults filled in.
    pub fn target(&self, t: &Target) -> Target {
        Target {
            encoding: t.encoding.clone().or_else(|| self.encoding.clone()),
            ..t.clone()
        }
    }
}

/// Codec names the mcast server takes.
pub const ENCODINGS: [&str; 10] = [
    "l16", "l24", "pcmu", "g711u", "ulaw", "pcma", "g711a", "alaw", "g722", "opus",
];

fn check_encoding(what: &str, e: &Option<String>) -> Result<()> {
    match e {
        Some(e) if !ENCODINGS.contains(&e.to_ascii_lowercase().as_str()) => bail!(
            "{what}: unknown encoding \"{e}\" (use l16, l24, pcmu/g711u, pcma/g711a, g722 or opus)"
        ),
        _ => Ok(()),
    }
}

/// A day, or a run of days, when schedules don't play.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DayOff {
    /// "Christmas", "Half term"; optional.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// `YYYY-MM-DD`.
    pub from: String,
    /// The last day off, if more than one: `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl DayOff {
    /// First and last day, both included.
    pub fn range(&self) -> Result<(jiff::civil::Date, jiff::civil::Date)> {
        let from = parse_date(&self.from)?;
        let to = match &self.to {
            Some(t) => parse_date(t)?,
            None => from,
        };
        Ok((from, to))
    }
}

/// A date: `YYYY-MM-DD`.
pub fn parse_date(s: &str) -> Result<jiff::civil::Date> {
    match s.parse::<jiff::civil::Date>() {
        Ok(d) if s.len() == 10 => Ok(d),
        _ => bail!("\"{s}\" isn't a date like 2026-12-24"),
    }
}

/// Most times a weekly schedule can have.
pub const MAX_TIMES: usize = 48;

/// When to play an announcement by itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub id: String,
    /// Optional; the panel shows the announcement's name without one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Announcement id.
    pub announcement: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub when: When,
    /// Site id: it plays to the announcement's zones there, in the site's
    /// time zone. Unset: to all the announcement's zones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// Schedule group id; the group must be on for it to play.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

fn yes() -> bool {
    true
}

/// Wall-clock times in the config's time zone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum When {
    /// `{"weekly": {"days": ["mon", "fri"], "times": ["08:30", "12:00"]}}`
    Weekly { days: Vec<Day>, times: Vec<String> },
    /// `{"once": "2026-12-24T16:30"}`
    Once(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Day {
    pub fn weekday(self) -> jiff::civil::Weekday {
        use jiff::civil::Weekday as W;
        match self {
            Day::Mon => W::Monday,
            Day::Tue => W::Tuesday,
            Day::Wed => W::Wednesday,
            Day::Thu => W::Thursday,
            Day::Fri => W::Friday,
            Day::Sat => W::Saturday,
            Day::Sun => W::Sunday,
        }
    }
}

/// A time of day: `HH:MM` or `HH:MM:SS`.
pub fn parse_time(s: &str) -> Result<jiff::civil::Time> {
    let ok = matches!(s.len(), 5 | 8) && s.as_bytes()[2] == b':';
    match s.parse::<jiff::civil::Time>() {
        Ok(t) if ok => Ok(t),
        _ => bail!("\"{s}\" isn't a time of day like 08:30"),
    }
}

/// A date and time: `YYYY-MM-DDTHH:MM` (or with seconds).
pub fn parse_datetime(s: &str) -> Result<jiff::civil::DateTime> {
    let ok = matches!(s.len(), 16 | 19) && s.as_bytes()[10] == b'T';
    match s.parse::<jiff::civil::DateTime>() {
        Ok(t) if ok => Ok(t),
        _ => bail!("\"{s}\" isn't a date and time like 2026-12-24T16:30"),
    }
}

fn get_tz(name: &str) -> Result<jiff::tz::TimeZone> {
    jiff::tz::TimeZone::get(name).map_err(|_| {
        anyhow::anyhow!("unknown time zone \"{name}\" (use a name like Europe/London)")
    })
}

impl Config {
    /// The panel's time zone: schedules without a site of their own use it.
    pub fn tz(&self) -> Result<jiff::tz::TimeZone> {
        match &self.time_zone {
            Some(name) => get_tz(name),
            None => Ok(jiff::tz::TimeZone::system()),
        }
    }

    pub fn site(&self, id: &str) -> Option<&Site> {
        self.sites.iter().find(|s| s.id == id)
    }

    /// A site's time zone (its own, else the panel's); `None` for no site.
    pub fn site_tz(&self, site: Option<&str>) -> Result<jiff::tz::TimeZone> {
        match site
            .and_then(|id| self.site(id))
            .and_then(|s| s.time_zone.as_deref())
        {
            Some(name) => get_tz(name),
            None => self.tz(),
        }
    }

    /// Its time zone's name, for people.
    pub fn site_tz_name(&self, site: Option<&str>) -> String {
        match site
            .and_then(|id| self.site(id))
            .and_then(|s| s.time_zone.clone())
        {
            Some(n) => n,
            None => self.tz_name(),
        }
    }

    /// The time zone a schedule's times are in.
    pub fn schedule_tz(&self, s: &Schedule) -> jiff::tz::TimeZone {
        self.site_tz(s.site.as_deref())
            .unwrap_or_else(|_| jiff::tz::TimeZone::system())
    }

    /// Whether a schedule may play at all: it, its group and its site are
    /// on, and schedules aren't all paused (days off aside).
    pub fn schedule_on(&self, s: &Schedule) -> bool {
        let group_on = s
            .group
            .as_deref()
            .is_none_or(|g| self.schedule_groups.iter().any(|x| x.id == g && x.enabled));
        let site_on = s
            .site
            .as_deref()
            .is_none_or(|id| self.site(id).is_none_or(|x| !x.paused));
        s.enabled && group_on && site_on && !self.schedules_paused
    }

    /// The days off that apply at a site: the panel's, and its own.
    fn days_off_at(&self, site: Option<&str>) -> Vec<(jiff::civil::Date, jiff::civil::Date)> {
        let own = site
            .and_then(|id| self.site(id))
            .map(|s| s.days_off.as_slice());
        self.days_off
            .iter()
            .chain(own.unwrap_or_default())
            .filter_map(|d| d.range().ok())
            .collect()
    }

    /// The time zone's name, for people.
    pub fn tz_name(&self) -> String {
        match &self.time_zone {
            Some(n) => n.clone(),
            None => jiff::tz::TimeZone::system()
                .iana_name()
                .map_or_else(|| "the system's time zone".into(), str::to_string),
        }
    }
}

/// A set of speakers that are paged together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    pub id: String,
    pub name: String,
    pub targets: Vec<Target>,
    /// Site id, for zones that belong to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// The speakers expected to hear its pages: the names they give in RTCP
    /// (`mcast receive --name Lobby`, else their hostname). After a play,
    /// the panel says which of them heard it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listeners: Vec<String>,
    /// Text displays: groups (phonebook names or addresses) that get the
    /// text pages of what plays here, as real-time text (`mcast text recv`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub displays: Vec<String>,
}

/// Most expected listeners a zone can have.
pub const MAX_LISTENERS: usize = 100;
/// Most text displays a zone can have.
pub const MAX_DISPLAYS: usize = 20;

/// One multicast group, with the send options its speakers need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// A phonebook name or an address such as `239.192.2.1:10000`.
    pub group: String,
    /// `l16`, `l24`, `pcmu`/`g711u`, `pcma`/`g711a`, `g722` or `opus`;
    /// unset uses the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    /// Added to the announcement's volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    /// IP TOS byte, e.g. 184 (DSCP EF).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tos: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<u32>,
    /// `"any"`, an IPv4 interface address, or an interface index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
}

/// What to play: an uploaded media file, or just the chime(s).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Audio {
    /// `{"media": "lunch.wav"}`: a file in the server's media directory.
    Media(String),
    /// `"chime_only"`: no audio, just the chime (the default chime unless
    /// `chime` or `end_chime` says otherwise).
    ChimeOnly,
    /// `"live"`: someone talks, from the panel's page (push to talk). Its
    /// chimes go before and after.
    Live,
    /// `{"playlist": "id"}`: a playlist's sounds, one after another.
    Playlist(String),
}

/// The sample rate the panel's page sends live audio at.
pub const LIVE_RATE: u32 = 48_000;

/// Who wins when a group is already being sent to.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// Skip groups that are busy.
    #[default]
    Normal,
    /// Take over groups from anything but another high or emergency announcement.
    High,
    /// Take over groups from anything but another emergency announcement.
    Emergency,
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Emergency => "emergency",
        })
    }
}

fn one() -> u32 {
    1
}

fn default_gap() -> f64 {
    2.0
}

fn is_one(v: &u32) -> bool {
    *v == 1
}

fn is_normal(p: &Priority) -> bool {
    *p == Priority::Normal
}

/// Audio, chimes and where they go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announcement {
    pub id: String,
    pub name: String,
    pub audio: Audio,
    /// Chime before the audio, as in the mcast API: `"default"` (the mcast
    /// server's), a built-in name such as `"ding-dong"`, or
    /// `{"file": "NAME"}`; or [`NO_CHIME`] for none. Unset: the panel's
    /// default ([`Defaults::chime`]), or none (but see [`Audio::ChimeOnly`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chime: Option<Value>,
    /// Chime after the audio, in the same form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_chime: Option<Value>,
    /// Processing: `"voice"`, `"music"` or `"none"`. This and the next two,
    /// unset, come from the panel's defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    /// Gain on the audio before processing, in dB (not the chimes): for a
    /// live announcement, the microphone's input gain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_gain_db: Option<f64>,
    /// Normalise loudness even without voice or music processing (which
    /// always do): for a live announcement, levels out quiet and loud talkers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalize: Option<bool>,
    /// Silence before the chime, so speakers' amplifiers can wake up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_in_ms: Option<f64>,
    /// How many times to play it.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub repeat: u32,
    /// Seconds between repeats.
    #[serde(default = "default_gap")]
    pub repeat_gap_s: f64,
    /// Zone ids.
    pub zones: Vec<String>,
    #[serde(default, skip_serializing_if = "is_normal")]
    pub priority: Priority,
    /// Other audio to other zones, played alongside, in the same run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// The Panel heading it's under (a category's id); none for "Other".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// For filtering the Panel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Its button's color, instead of its category's (one of [`COLORS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// An emoji (or a few characters) on its button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Ask "are you sure?" before playing it from the Panel.
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm: bool,
}

/// An announcement's chime with the default filled in: `None` for no chime.
fn chime_or(own: &Option<Value>, default: &Option<Value>) -> Option<Value> {
    let is_none = |v: &Value| v.as_str() == Some(NO_CHIME);
    match own {
        Some(v) if is_none(v) => None,
        Some(v) => Some(v.clone()),
        None => default.clone().filter(|v| !is_none(v)),
    }
}

impl Announcement {
    /// This announcement with the panel's defaults filled in, as it plays.
    /// Its chimes come out as `None` (no chime) or a chime to send.
    pub fn with_defaults(&self, d: &Defaults) -> Announcement {
        Announcement {
            chime: chime_or(&self.chime, &d.chime),
            end_chime: chime_or(&self.end_chime, &d.end_chime),
            mode: self.mode.clone().or_else(|| d.mode.clone()),
            volume_db: self.volume_db.or(d.volume_db),
            input_gain_db: self.input_gain_db.or(d.input_gain_db),
            normalize: self.normalize.or(d.normalize),
            lead_in_ms: self.lead_in_ms.or(d.lead_in_ms),
            ..self.clone()
        }
    }

    /// Its own audio with its zones, then each part's: what plays where.
    pub fn sections(&self) -> Vec<(&Audio, &[String])> {
        std::iter::once((&self.audio, self.zones.as_slice()))
            .chain(self.parts.iter().map(|p| (&p.audio, p.zones.as_slice())))
            .collect()
    }

    /// The send request for one target, in the mcast API's JSON. `group` is
    /// the target's resolved address and `audio` what plays there (its own,
    /// or a part's). Call it on [`Announcement::with_defaults`].
    pub fn send_request(&self, cfg: &Config, audio: &Audio, target: &Target, group: &str) -> Value {
        let mut r = Map::new();
        r.insert("group".into(), group.into());
        r.insert("name".into(), self.name.clone().into());
        r.insert(
            "source".into(),
            match audio {
                Audio::Media(path) => json!({ "type": "file", "path": path }),
                Audio::ChimeOnly => json!({ "type": "chime" }),
                Audio::Live => json!({ "type": "live", "rate": LIVE_RATE, "channels": 1 }),
                Audio::Playlist(id) => {
                    let p = cfg.playlist(id);
                    let paths: Vec<Value> = p
                        .map(|p| {
                            p.items
                                .iter()
                                .filter_map(PlaylistItem::source_item)
                                .collect()
                        })
                        .unwrap_or_default();
                    json!({ "type": "files", "paths": paths, "gap_ms": p.map_or(0, |p| p.gap_ms) })
                }
            },
        );
        let volume = match (self.volume_db, target.volume_db) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
        let mut put = |k: &str, v: Option<Value>| {
            if let Some(v) = v {
                r.insert(k.into(), v);
            }
        };
        put("chime", self.chime.clone());
        put("end_chime", self.end_chime.clone());
        put("mode", self.mode.clone().map(Into::into));
        put("volume_db", volume.map(Into::into));
        put("input_gain_db", self.input_gain_db.map(Into::into));
        put("normalize", self.normalize.filter(|n| *n).map(Into::into));
        put("lead_in_ms", self.lead_in_ms.map(Into::into));
        put("encoding", target.encoding.clone().map(Into::into));
        put("tos", target.tos.map(Into::into));
        put("ttl", target.ttl.map(Into::into));
        put("interface", target.interface.clone().map(Into::into));
        Value::Object(r)
    }
}

/// `name`, or something inside it when it's a folder.
fn within(name: &str, path: &str) -> bool {
    name == path
        || name
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Config {
    /// Whether a media file is hidden from lists (it, or a folder it's in).
    pub fn is_hidden(&self, name: &str) -> bool {
        self.hidden_media.iter().any(|h| within(name, h))
    }

    /// Who uses media file `name` (or, for a folder, anything in it):
    /// "announcement Lunch bell", "the defaults".
    pub fn media_users(&self, name: &str) -> Vec<String> {
        let file_of = |c: &Option<Value>| {
            c.as_ref()
                .and_then(|c| c["file"].as_str())
                .is_some_and(|f| within(f, name))
        };
        let mut users = Vec::new();
        if file_of(&self.defaults.chime) || file_of(&self.defaults.end_chime) {
            users.push("the defaults".to_string());
        }
        for a in &self.announcements {
            let audio = a
                .sections()
                .iter()
                .any(|(au, _)| matches!(au, Audio::Media(m) if within(m, name)));
            if audio || file_of(&a.chime) || file_of(&a.end_chime) {
                users.push(format!("announcement {}", a.name));
            }
        }
        for p in &self.playlists {
            if p.items
                .iter()
                .filter_map(PlaylistItem::media)
                .any(|m| within(m, name))
            {
                users.push(format!("playlist {}", p.name));
            }
        }
        users
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id == id)
    }

    /// The announcements that play playlist `id`.
    pub fn playlist_users(&self, id: &str) -> Vec<String> {
        self.announcements
            .iter()
            .filter(|a| {
                a.sections()
                    .iter()
                    .any(|(au, _)| matches!(au, Audio::Playlist(p) if p == id))
            })
            .map(|a| a.name.clone())
            .collect()
    }

    /// Point everything that used `from` (a file, or a folder and what's in
    /// it) at `to`, after it was renamed or moved.
    pub fn media_moved(&mut self, from: &str, to: &str) {
        let moved = |n: &str| -> Option<String> {
            within(n, from).then(|| format!("{to}{}", &n[from.len()..]))
        };
        let chime = |c: &mut Option<Value>| {
            if let Some(new) = c.as_ref().and_then(|c| c["file"].as_str()).and_then(moved) {
                *c = Some(json!({ "file": new }));
            }
        };
        chime(&mut self.defaults.chime);
        chime(&mut self.defaults.end_chime);
        for a in &mut self.announcements {
            for audio in
                std::iter::once(&mut a.audio).chain(a.parts.iter_mut().map(|p| &mut p.audio))
            {
                if let Audio::Media(m) = audio {
                    if let Some(new) = moved(m) {
                        *m = new;
                    }
                }
            }
            chime(&mut a.chime);
            chime(&mut a.end_chime);
        }
        for p in &mut self.playlists {
            for item in &mut p.items {
                if let PlaylistItem::Sound { media } = item {
                    if let Some(new) = moved(media) {
                        *media = new;
                    }
                }
            }
        }
        for h in &mut self.hidden_media {
            if let Some(new) = moved(h) {
                *h = new;
            }
        }
        let tagged: Vec<String> = self
            .media_tags
            .keys()
            .filter(|n| within(n, from))
            .cloned()
            .collect();
        for n in tagged {
            if let (Some(tags), Some(new)) = (self.media_tags.remove(&n), moved(&n)) {
                self.media_tags.insert(new, tags);
            }
        }
    }

    /// Forget a deleted file or folder: its hidden mark and tags.
    pub fn media_deleted(&mut self, name: &str) {
        self.hidden_media.retain(|h| h != name);
        self.media_tags.retain(|n, _| !within(n, name));
    }

    /// A file's tags.
    pub fn tags_of(&self, name: &str) -> &[String] {
        self.media_tags.get(name).map_or(&[], Vec::as_slice)
    }

    /// Add and remove tags on files; a file left with none is dropped.
    /// Tags match without regard to case.
    pub fn retag(&mut self, names: &[String], add: &[String], remove: &[String]) -> Result<()> {
        let clean = |list: &[String]| -> Result<Vec<String>> {
            list.iter()
                .map(|t| {
                    clean_tag(t).with_context(|| {
                        format!("\"{t}\" isn't a tag: up to {MAX_TAG} characters, no commas")
                    })
                })
                .collect()
        };
        let (add, remove) = (clean(add)?, clean(remove)?);
        // Spell a new tag as it's already spelled elsewhere.
        let known: Vec<String> = self.media_tags.values().flatten().cloned().collect();
        let spelled = |t: &String| {
            known
                .iter()
                .find(|k| k.eq_ignore_ascii_case(t))
                .cloned()
                .unwrap_or_else(|| t.clone())
        };
        for n in names {
            if !valid_media_name(n) {
                bail!("\"{n}\" isn't a media file name");
            }
            let tags = self.media_tags.entry(n.clone()).or_default();
            tags.retain(|t| !remove.iter().any(|r| r.eq_ignore_ascii_case(t)));
            for t in &add {
                if !tags.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                    tags.push(spelled(t));
                }
            }
            tags.sort_by_key(|t| t.to_lowercase());
            if tags.is_empty() {
                self.media_tags.remove(n);
            }
        }
        Ok(())
    }

    /// Whether schedules at `site` are off on this date.
    pub fn is_day_off(&self, site: Option<&str>, date: jiff::civil::Date) -> bool {
        self.days_off_at(site)
            .iter()
            .any(|(from, to)| *from <= date && date <= *to)
    }

    /// The last day of the days off at `site` that `date` is in, if it's in
    /// any (runs that touch or overlap are followed through).
    pub fn days_off_until(
        &self,
        site: Option<&str>,
        date: jiff::civil::Date,
    ) -> Option<jiff::civil::Date> {
        let ranges = self.days_off_at(site);
        let covering = |day| {
            ranges
                .iter()
                .filter(|(from, to)| *from <= day && day <= *to)
                .map(|(_, to)| *to)
                .max()
        };
        let mut end = covering(date)?;
        while let Some(further) = end.tomorrow().ok().and_then(covering) {
            end = further;
        }
        Some(end)
    }

    pub fn zone(&self, id: &str) -> Option<&Zone> {
        self.zones.iter().find(|z| z.id == id)
    }

    /// An announcement's zones, or (with a site) those of them at that site.
    pub fn zones_at<'a>(&'a self, a: &Announcement, site: Option<&str>) -> Vec<&'a Zone> {
        a.sections()
            .into_iter()
            .flat_map(|(_, zones)| self.zones_in(zones, site))
            .collect()
    }

    /// The sites an announcement plays at (just `site`, when given): those
    /// of its zones, by id, each once.
    pub fn sites_at(&self, a: &Announcement, site: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = self
            .zones_at(a, site)
            .into_iter()
            .filter_map(|z| z.site.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// These zones (ids), or (with a site) those of them at that site.
    pub fn zones_in<'a>(&'a self, zones: &[String], site: Option<&str>) -> Vec<&'a Zone> {
        zones
            .iter()
            .filter_map(|z| self.zone(z))
            .filter(|z| site.is_none_or(|s| z.site.as_deref() == Some(s)))
            .collect()
    }

    /// An announcement by id, or else by name (ignoring case).
    pub fn announcement(&self, which: &str) -> Option<&Announcement> {
        self.announcements
            .iter()
            .find(|a| a.id == which)
            .or_else(|| {
                self.announcements
                    .iter()
                    .find(|a| a.name.eq_ignore_ascii_case(which))
            })
    }

    /// Check everything fits together; the first problem found is the error.
    pub fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for z in &self.zones {
            check_id("zone", &z.id)?;
            check_name("zone", &z.name)?;
            if !ids.insert(z.id.as_str()) {
                bail!("two zones have the id \"{}\"", z.id);
            }
            if !names.insert(z.name.to_lowercase()) {
                bail!("two zones are called \"{}\"", z.name);
            }
            if z.targets.is_empty() {
                bail!("zone \"{}\" has no targets", z.id);
            }
            if z.listeners.len() > MAX_LISTENERS {
                bail!("zone \"{}\" has more than {MAX_LISTENERS} listeners", z.id);
            }
            let mut heard_by = HashSet::new();
            for l in &z.listeners {
                check_name(&format!("zone \"{}\": listener", z.id), l)?;
                if l.trim() != l {
                    bail!(
                        "zone \"{}\": listener \"{l}\" starts or ends with a space",
                        z.id
                    );
                }
                if !heard_by.insert(l.to_lowercase()) {
                    bail!("zone \"{}\" lists the listener \"{l}\" twice", z.id);
                }
            }
            if z.displays.len() > MAX_DISPLAYS {
                bail!("zone \"{}\" has more than {MAX_DISPLAYS} displays", z.id);
            }
            let mut shown_on = HashSet::new();
            for d in &z.displays {
                if d.trim().is_empty() || d.trim() != d {
                    bail!("zone \"{}\": display \"{d}\" isn't a group", z.id);
                }
                if !shown_on.insert(d.to_lowercase()) {
                    bail!("zone \"{}\" lists the display \"{d}\" twice", z.id);
                }
            }
            for t in &z.targets {
                if t.group.trim().is_empty() {
                    bail!("zone \"{}\" has a target with no group", z.id);
                }
                check_number(&format!("zone \"{}\": volume_db", z.id), t.volume_db)?;
                check_encoding(&format!("zone \"{}\": encoding", z.id), &t.encoding)?;
            }
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for a in &self.announcements {
            let what = format!("announcement \"{}\"", a.id);
            check_id("announcement", &a.id)?;
            check_name("announcement", &a.name)?;
            if !ids.insert(a.id.as_str()) {
                bail!("two announcements have the id \"{}\"", a.id);
            }
            if !names.insert(a.name.to_lowercase()) {
                bail!("two announcements are called \"{}\"", a.name);
            }
            if let Some(c) = &a.category {
                if !self.categories.iter().any(|x| x.id == *c) {
                    bail!("{what}: no category \"{c}\"");
                }
            }
            check_look(&what, &a.color, &a.icon)?;
            if a.tags.len() > MAX_TAGS {
                bail!("{what}: at most {MAX_TAGS} tags");
            }
            let mut tags = HashSet::new();
            for t in &a.tags {
                if clean_tag(t).as_deref() != Some(t.as_str()) {
                    bail!("{what}: \"{t}\" isn't a tag (up to {MAX_TAG} characters, no commas)");
                }
                if !tags.insert(t.to_lowercase()) {
                    bail!("{what}: the tag \"{t}\" is there twice");
                }
            }
            if a.zones.is_empty() {
                bail!("{what} has no zones");
            }
            if a.audio == Audio::Live && !a.parts.is_empty() {
                bail!("{what}: a live announcement can't have parts");
            }
            let mut seen_zones = HashSet::new();
            for (i, (audio, zones)) in a.sections().into_iter().enumerate() {
                let what = if i == 0 {
                    what.clone()
                } else {
                    format!("{what}, part {i}")
                };
                match audio {
                    Audio::Media(m) if !valid_media_name(m) => {
                        bail!("{what}: \"{m}\" isn't a media file name")
                    }
                    Audio::Playlist(id) => match self.playlist(id) {
                        None => bail!("{what}: no playlist \"{id}\""),
                        Some(p) if !p.items.iter().any(PlaylistItem::audible) => {
                            bail!("{what}: playlist \"{}\" has nothing to play", p.name)
                        }
                        _ => {}
                    },
                    Audio::Live if i > 0 => bail!("{what}: a part can't be live"),
                    _ => {}
                }
                if zones.is_empty() {
                    bail!("{what} has no zones");
                }
                for z in zones {
                    if self.zone(z).is_none() {
                        bail!("{what}: no zone \"{z}\"");
                    }
                    if !seen_zones.insert(z.as_str()) {
                        bail!("{what}: zone \"{z}\" is in the announcement twice");
                    }
                }
            }
            if !(1..=MAX_REPEAT).contains(&a.repeat) {
                bail!("{what}: repeat must be 1 to {MAX_REPEAT}");
            }
            if a.audio == Audio::Live && a.repeat != 1 {
                bail!("{what}: a live announcement plays once");
            }
            if !(0.0..=MAX_REPEAT_GAP_S).contains(&a.repeat_gap_s) {
                bail!("{what}: repeat_gap_s must be 0 to {MAX_REPEAT_GAP_S}");
            }
            check_number(&format!("{what}: volume_db"), a.volume_db)?;
            check_input_gain(&format!("{what}: input_gain_db"), a.input_gain_db)?;
            check_number(&format!("{what}: lead_in_ms"), a.lead_in_ms)?;
            check_chime(&format!("{what}: chime"), &a.chime)?;
            check_chime(&format!("{what}: end_chime"), &a.end_chime)?;
        }
        let d = &self.defaults;
        check_number("defaults: volume_db", d.volume_db)?;
        check_input_gain("defaults: input_gain_db", d.input_gain_db)?;
        check_number("defaults: lead_in_ms", d.lead_in_ms)?;
        check_chime("defaults: chime", &d.chime)?;
        check_chime("defaults: end_chime", &d.end_chime)?;
        check_encoding("defaults", &d.encoding)?;
        for h in &self.hidden_media {
            if !valid_media_name(h) {
                bail!("hidden_media: \"{h}\" isn't a media file or folder name");
            }
        }
        for (n, tags) in &self.media_tags {
            if !valid_media_name(n) {
                bail!("media_tags: \"{n}\" isn't a media file name");
            }
            for t in tags {
                if clean_tag(t).as_deref() != Some(t.as_str()) {
                    bail!("media_tags: \"{t}\" on {n} isn't a tag");
                }
            }
        }
        check_days_off(&self.days_off)?;
        if self.history_days() > MAX_HISTORY_DAYS {
            bail!("history_days: at most {MAX_HISTORY_DAYS} (or 0 to keep it all)");
        }
        for (tab, order) in &self.layout {
            check_id("layout tab", tab)?;
            if order.len() > MAX_LAYOUT {
                bail!("layout \"{tab}\": at most {MAX_LAYOUT} sections");
            }
            let mut seen = HashSet::new();
            for id in order {
                check_id(&format!("layout \"{tab}\" section"), id)?;
                if !seen.insert(id.as_str()) {
                    bail!("layout \"{tab}\": \"{id}\" is listed twice");
                }
            }
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for c in &self.categories {
            check_look(&format!("category \"{}\"", c.id), &c.color, &None)?;
            check_id("category", &c.id)?;
            check_name("category", &c.name)?;
            if !ids.insert(c.id.as_str()) {
                bail!("two categories have the id \"{}\"", c.id);
            }
            if !names.insert(c.name.to_lowercase()) {
                bail!("two categories are called \"{}\"", c.name);
            }
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for p in &self.playlists {
            let what = format!("playlist \"{}\"", p.id);
            check_id("playlist", &p.id)?;
            check_name("playlist", &p.name)?;
            if !ids.insert(p.id.as_str()) {
                bail!("two playlists have the id \"{}\"", p.id);
            }
            if !names.insert(p.name.to_lowercase()) {
                bail!("two playlists are called \"{}\"", p.name);
            }
            if p.items.len() > MAX_PLAYLIST {
                bail!("{what}: at most {MAX_PLAYLIST} items");
            }
            if p.gap_ms > MAX_PLAYLIST_GAP_MS {
                bail!("{what}: gap_ms must be 0 to {MAX_PLAYLIST_GAP_MS}");
            }
            for (n, item) in p.items.iter().enumerate() {
                match item {
                    PlaylistItem::Sound { media } if !valid_media_name(media) => {
                        bail!("{what}: \"{media}\" isn't a media file name");
                    }
                    PlaylistItem::Text {
                        text,
                        speak,
                        show,
                        voice,
                        seconds,
                    } => {
                        let what = format!("{what}, item {}", n + 1);
                        if text.trim().is_empty() {
                            bail!("{what}: the text is empty");
                        }
                        if text.chars().count() > MAX_TEXT {
                            bail!("{what}: text is at most {MAX_TEXT} characters");
                        }
                        if !speak && !show {
                            bail!("{what}: text must be spoken, shown or both");
                        }
                        if let Some(n) = seconds {
                            if *speak {
                                bail!("{what}: only a page that's only shown has seconds");
                            }
                            if !(1..=MAX_SHOW_SECS).contains(n) {
                                bail!("{what}: seconds is 1 to {MAX_SHOW_SECS}");
                            }
                        }
                        if let Some(v) = voice {
                            if v.is_empty()
                                || v.len() > 100
                                || v.starts_with('.')
                                || !v
                                    .chars()
                                    .all(|c| c.is_ascii_alphanumeric() || "-_.+".contains(c))
                            {
                                bail!("{what}: \"{v}\" isn't a voice name");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for site in &self.sites {
            check_id("site", &site.id)?;
            check_name("site", &site.name)?;
            if !ids.insert(site.id.as_str()) {
                bail!("two sites have the id \"{}\"", site.id);
            }
            if !names.insert(site.name.to_lowercase()) {
                bail!("two sites are called \"{}\"", site.name);
            }
            if let Some(tz) = &site.time_zone {
                get_tz(tz).map_err(|e| anyhow::anyhow!("site \"{}\": {e}", site.id))?;
            }
            check_days_off(&site.days_off)
                .map_err(|e| anyhow::anyhow!("site \"{}\": {e}", site.id))?;
        }
        let mut ids = HashSet::new();
        for g in &self.schedule_groups {
            check_id("schedule group", &g.id)?;
            check_name("schedule group", &g.name)?;
            if !ids.insert(g.id.as_str()) {
                bail!("two schedule groups have the id \"{}\"", g.id);
            }
        }
        for z in &self.zones {
            if let Some(site) = &z.site {
                if self.site(site).is_none() {
                    bail!("zone \"{}\": no site \"{site}\"", z.id);
                }
            }
        }
        let mut ids = HashSet::new();
        for sch in &self.schedules {
            let what = format!("schedule \"{}\"", sch.id);
            check_id("schedule", &sch.id)?;
            if !sch.name.is_empty() {
                check_name("schedule", &sch.name)?;
            }
            if !ids.insert(sch.id.as_str()) {
                bail!("two schedules have the id \"{}\"", sch.id);
            }
            let Some(a) = self.announcements.iter().find(|a| a.id == sch.announcement) else {
                bail!("{what}: no announcement \"{}\"", sch.announcement);
            };
            if a.audio == Audio::Live {
                bail!(
                    "{what}: \"{}\" is live (someone talks), so it can't be scheduled",
                    a.name
                );
            }
            if let Some(g) = &sch.group {
                if !self.schedule_groups.iter().any(|x| x.id == *g) {
                    bail!("{what}: no schedule group \"{g}\"");
                }
            }
            if let Some(site) = &sch.site {
                let Some(site) = self.site(site) else {
                    bail!("{what}: no site \"{site}\"");
                };
                if self.zones_at(a, Some(&site.id)).is_empty() {
                    bail!(
                        "{what}: \"{}\" has no zones at {}, so it would play nowhere",
                        a.name,
                        site.name
                    );
                }
            }
            match &sch.when {
                When::Weekly { days, times } => {
                    if days.is_empty() {
                        bail!("{what} has no days");
                    }
                    if days.iter().collect::<HashSet<_>>().len() != days.len() {
                        bail!("{what} has a day twice");
                    }
                    if times.is_empty() {
                        bail!("{what} has no times");
                    }
                    if times.len() > MAX_TIMES {
                        bail!("{what} has more than {MAX_TIMES} times");
                    }
                    for t in times {
                        parse_time(t).map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
                    }
                }
                When::Once(at) => {
                    parse_datetime(at).map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
                }
            }
        }
        if self.inputs.len() > MAX_INPUTS {
            bail!("more than {MAX_INPUTS} inputs");
        }
        let (mut ids, mut names, mut pins) = (HashSet::new(), HashSet::new(), HashMap::new());
        for i in &self.inputs {
            let what = format!("input \"{}\"", i.id);
            check_id("input", &i.id)?;
            check_name("input", &i.name)?;
            if !ids.insert(i.id.as_str()) {
                bail!("two inputs have the id \"{}\"", i.id);
            }
            if !names.insert(i.name.to_lowercase()) {
                bail!("two inputs are called \"{}\"", i.name);
            }
            let chip = i.chip_path();
            let n = chip.strip_prefix("/dev/gpiochip").unwrap_or("");
            if n.is_empty() || n.len() > 3 || !n.chars().all(|c| c.is_ascii_digit()) {
                bail!(
                    "{what}: chip \"{}\" should be gpiochipN, e.g. gpiochip0",
                    chip
                );
            }
            if i.line > 1023 {
                bail!("{what}: line {} is out of range (0 to 1023)", i.line);
            }
            if i.debounce_ms() > MAX_DEBOUNCE_MS {
                bail!("{what}: debounce_ms is at most {MAX_DEBOUNCE_MS}");
            }
            if i.enabled {
                if let Some(other) = pins.insert((chip, i.line), i.id.as_str()) {
                    bail!(
                        "inputs \"{other}\" and \"{}\" both use line {}",
                        i.id,
                        i.line
                    );
                }
            }
            let Some(a) = self.announcements.iter().find(|a| a.id == i.announcement) else {
                bail!("{what}: no announcement \"{}\"", i.announcement);
            };
            if a.audio == Audio::Live {
                bail!(
                    "{what}: \"{}\" is live (someone talks), so a button can't play it",
                    a.name
                );
            }
        }
        self.tz()?;
        Ok(())
    }
}

/// Ids are referred to from other entries (and later from URLs): letters,
/// digits, `-` and `_`.
fn check_id(what: &str, id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= MAX_NAME
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!("invalid {what} id \"{id}\": use 1 to {MAX_NAME} letters, digits, '-' and '_'");
    }
    Ok(())
}

fn check_name(what: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME {
        bail!("{what} name \"{name}\" must be 1 to {MAX_NAME} characters");
    }
    if name.chars().any(char::is_control) {
        bail!("{what} name \"{name}\" contains control characters");
    }
    Ok(())
}

/// A chime is a name or `{"file": NAME}`.
fn check_chime(what: &str, v: &Option<Value>) -> Result<()> {
    match v {
        None => Ok(()),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(()),
        Some(Value::Object(o)) if o.len() == 1 => match o.get("file").and_then(Value::as_str) {
            Some(f) if valid_media_name(f) => Ok(()),
            _ => bail!("{what}: expected {{\"file\": NAME}} with a media file name"),
        },
        Some(_) => bail!("{what}: expected a chime name or {{\"file\": NAME}}"),
    }
}

fn check_days_off(days: &[DayOff]) -> Result<()> {
    for off in days {
        let what = match off.name.as_str() {
            "" => format!("day off {}", off.from),
            n => format!("day off \"{n}\""),
        };
        if !off.name.is_empty() {
            check_name("day off", &off.name)?;
        }
        let (from, to) = off.range().map_err(|e| anyhow::anyhow!("{what}: {e}"))?;
        if to < from {
            bail!("{what} ends before it starts");
        }
    }
    Ok(())
}

fn check_input_gain(what: &str, v: Option<f64>) -> Result<()> {
    check_number(what, v)?;
    if v.is_some_and(|v| v.abs() > MAX_INPUT_GAIN_DB) {
        bail!("{what} must be -{MAX_INPUT_GAIN_DB} to {MAX_INPUT_GAIN_DB} dB");
    }
    Ok(())
}

/// The mcast server's limit on an input gain.
const MAX_INPUT_GAIN_DB: f64 = 40.0;

fn check_number(what: &str, v: Option<f64>) -> Result<()> {
    if v.is_some_and(|v| !v.is_finite()) {
        bail!("{what} must be a number");
    }
    Ok(())
}

/// The server's rule for media names: file and folder names joined by
/// `/`, none empty or starting with `.`.
pub(crate) fn valid_media_name(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    name.len() <= 1024
        && parts.len() <= 8
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 255
                && !p.starts_with('.')
                && !p.chars().any(|c| c == '\\' || c.is_control())
        })
}
