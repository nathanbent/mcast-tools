//! The panel's own data: zones and announcements.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;

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

impl Config {
    /// The time zone schedules are in.
    pub fn tz(&self) -> Result<jiff::tz::TimeZone> {
        match &self.time_zone {
            Some(name) => jiff::tz::TimeZone::get(name).map_err(|_| {
                anyhow::anyhow!("unknown time zone \"{name}\" (use a name like Europe/London)")
            }),
            None => Ok(jiff::tz::TimeZone::system()),
        }
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
}

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
}

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
            lead_in_ms: self.lead_in_ms.or(d.lead_in_ms),
            ..self.clone()
        }
    }

    /// The send request for one target, in the mcast API's JSON. `group` is
    /// the target's resolved address. Call it on [`Announcement::with_defaults`].
    pub fn send_request(&self, target: &Target, group: &str) -> Value {
        let mut r = Map::new();
        r.insert("group".into(), group.into());
        r.insert("name".into(), self.name.clone().into());
        r.insert(
            "source".into(),
            match &self.audio {
                Audio::Media(path) => json!({ "type": "file", "path": path }),
                Audio::ChimeOnly => json!({ "type": "chime" }),
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
            let audio = matches!(&a.audio, Audio::Media(m) if within(m, name));
            if audio || file_of(&a.chime) || file_of(&a.end_chime) {
                users.push(format!("announcement {}", a.name));
            }
        }
        users
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
            if let Audio::Media(m) = &mut a.audio {
                if let Some(new) = moved(m) {
                    *m = new;
                }
            }
            chime(&mut a.chime);
            chime(&mut a.end_chime);
        }
        for h in &mut self.hidden_media {
            if let Some(new) = moved(h) {
                *h = new;
            }
        }
    }

    /// Whether schedules are off on this date.
    pub fn is_day_off(&self, date: jiff::civil::Date) -> bool {
        self.days_off
            .iter()
            .filter_map(|d| d.range().ok())
            .any(|(from, to)| from <= date && date <= to)
    }

    /// The last day of the days off `date` is in, if it's in any (runs that
    /// touch or overlap are followed through).
    pub fn days_off_until(&self, date: jiff::civil::Date) -> Option<jiff::civil::Date> {
        let ranges: Vec<_> = self
            .days_off
            .iter()
            .filter_map(|d| d.range().ok())
            .collect();
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
            if let Audio::Media(m) = &a.audio {
                if !valid_media_name(m) {
                    bail!("{what}: \"{m}\" isn't a media file name");
                }
            }
            if a.zones.is_empty() {
                bail!("{what} has no zones");
            }
            for z in &a.zones {
                if self.zone(z).is_none() {
                    bail!("{what}: no zone \"{z}\"");
                }
            }
            if !(1..=MAX_REPEAT).contains(&a.repeat) {
                bail!("{what}: repeat must be 1 to {MAX_REPEAT}");
            }
            if !(0.0..=MAX_REPEAT_GAP_S).contains(&a.repeat_gap_s) {
                bail!("{what}: repeat_gap_s must be 0 to {MAX_REPEAT_GAP_S}");
            }
            check_number(&format!("{what}: volume_db"), a.volume_db)?;
            check_number(&format!("{what}: lead_in_ms"), a.lead_in_ms)?;
            check_chime(&format!("{what}: chime"), &a.chime)?;
            check_chime(&format!("{what}: end_chime"), &a.end_chime)?;
        }
        let d = &self.defaults;
        check_number("defaults: volume_db", d.volume_db)?;
        check_number("defaults: lead_in_ms", d.lead_in_ms)?;
        check_chime("defaults: chime", &d.chime)?;
        check_chime("defaults: end_chime", &d.end_chime)?;
        check_encoding("defaults", &d.encoding)?;
        for h in &self.hidden_media {
            if !valid_media_name(h) {
                bail!("hidden_media: \"{h}\" isn't a media file or folder name");
            }
        }
        for off in &self.days_off {
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
            if !self.announcements.iter().any(|a| a.id == sch.announcement) {
                bail!("{what}: no announcement \"{}\"", sch.announcement);
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
