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
    /// Chime before the audio, as in the mcast API: `"default"`, a built-in
    /// name such as `"ding-dong"`, or `{"file": "NAME"}`. Unset: none
    /// (but see [`Audio::ChimeOnly`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chime: Option<Value>,
    /// Chime after the audio, in the same form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_chime: Option<Value>,
    /// Processing: `"voice"`, `"music"` or `"none"`.
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

impl Announcement {
    /// The send request for one target, in the mcast API's JSON. `group` is
    /// the target's resolved address.
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

impl Config {
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
        }
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

fn check_number(what: &str, v: Option<f64>) -> Result<()> {
    if v.is_some_and(|v| !v.is_finite()) {
        bail!("{what} must be a number");
    }
    Ok(())
}

/// The server's rule for media names: a plain file name.
pub(crate) fn valid_media_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}
