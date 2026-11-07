//! What the API returns, as far as the TUI needs it. Kept separate from the
//! server's own types, like the web GUI's: unknown fields are ignored and
//! missing ones default.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: u64,
    pub kind: String,
    pub state: String,
    pub group: Option<String>,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub notes: Vec<String>,
    pub created_ms: u64,
    pub ended_ms: Option<u64>,
    pub error: Option<String>,
    pub stats: Value,
}

impl Job {
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "starting" | "running" | "stopping")
    }

    fn stat(&self, key: &str) -> f64 {
        self.stats[key].as_f64().unwrap_or(0.0)
    }

    pub fn title(&self) -> String {
        let verb = if self.kind == "send" {
            "Paging"
        } else {
            "Playing"
        };
        match (&self.name, &self.group) {
            (Some(n), _) => format!("{verb} \"{n}\""),
            (None, Some(g)) => format!("{verb} {g}"),
            _ => verb.to_string(),
        }
    }

    pub fn stats_line(&self) -> String {
        if self.kind == "send" {
            let s = self.stat("seconds") as u64;
            let mut line = format!(
                "{}:{:02} · {} packets",
                s / 60,
                s % 60,
                self.stat("packets")
            );
            if self.stat("send_errors") > 0.0 {
                line += &format!(" · {} send errors", self.stat("send_errors"));
            }
            line
        } else {
            let mut line = format!(
                "{} packets · {} lost · buffer {:.0} ms",
                self.stat("packets"),
                self.stat("lost"),
                self.stat("buffered_ms")
            );
            if self.stat("underruns") > 0.0 {
                line += &format!(" · {} underruns", self.stat("underruns"));
            }
            line
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Stream {
    pub name: String,
    pub origin: String,
    pub audio: Option<Audio>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub group: String,
    pub encoding: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub playable: bool,
}

impl Audio {
    pub fn format(&self) -> String {
        // G.722's SDP clock rate is 8000 (RFC 3551), but its audio is 16 kHz.
        let khz = if self.encoding.eq_ignore_ascii_case("g722") {
            16.0
        } else {
            self.sample_rate as f64 / 1000.0
        };
        let ch = if self.channels == 1 {
            "mono".to_string()
        } else {
            format!("{} ch", self.channels)
        };
        format!("{} {khz} kHz {ch}", self.encoding)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub streams: Vec<MonitorRow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MonitorRow {
    pub group: String,
    pub name: Option<String>,
    pub source: String,
    pub codec: Option<String>,
    pub kbit_per_sec: f64,
    pub loss_percent: f64,
    pub jitter_ms: Option<f64>,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    pub idle_ms: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MediaFile {
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Device {
    pub name: String,
    pub kind: String,
    pub default: bool,
}

pub fn bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1 << 20 => format!("{} KB", n / 1024),
        n => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Probe {
    pub id: String,
    pub online: bool,
    /// Kept as JSON so editing one field sends the rest back unchanged.
    pub config: Value,
    pub custom_config: bool,
    pub config_applied: bool,
    pub hostname: Option<String>,
    pub version: Option<String>,
    pub unicast_addr: Option<String>,
    pub last_seen_ms: Option<u64>,
    pub uptime_s: Option<f64>,
    pub queued: u64,
    pub dropped: u64,
    pub latest: Option<Report>,
    pub comparisons: Vec<Comparison>,
}

/// How a probe is doing, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Health {
    Problem,
    Offline,
    Loss,
    Ok,
    NeverSeen,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Health::Problem => "problem",
            Health::Offline => "offline",
            Health::Loss => "loss",
            Health::Ok => "online",
            Health::NeverSeen => "never seen",
        }
    }
}

impl Probe {
    pub fn label(&self) -> Option<&str> {
        self.config["label"].as_str()
    }

    pub fn flag(&self, key: &str) -> bool {
        self.config[key].as_bool().unwrap_or(false)
    }

    pub fn interval_s(&self) -> f64 {
        self.config["report_interval_s"].as_f64().unwrap_or(10.0)
    }

    pub fn groups(&self) -> Vec<String> {
        self.config["groups"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|g| g.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The multicast rows of its latest report.
    pub fn multicast(&self) -> Vec<&ProbeRow> {
        self.latest
            .as_ref()
            .map_or_else(Vec::new, |r| r.multicast())
    }

    pub fn health(&self) -> Health {
        if self.last_seen_ms.is_none() {
            return Health::NeverSeen;
        }
        if !self.online {
            return Health::Offline;
        }
        let verdict = |v: &[&str]| {
            self.comparisons
                .iter()
                .any(|c| v.contains(&c.verdict.as_str()))
        };
        if verdict(&["multicast_loss", "no_multicast"]) {
            return Health::Problem;
        }
        if verdict(&["both_loss", "unicast_loss"]) || self.multicast().iter().any(|r| r.lost > 0) {
            return Health::Loss;
        }
        Health::Ok
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Report {
    pub seq: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub streams: Vec<ProbeRow>,
}

impl Report {
    pub fn multicast(&self) -> Vec<&ProbeRow> {
        self.streams
            .iter()
            .filter(|r| is_multicast(&r.group))
            .collect()
    }

    /// Multicast packets and losses over the interval.
    pub fn totals(&self) -> (u64, u64) {
        self.multicast()
            .iter()
            .fold((0, 0), |(p, l), r| (p + r.packets, l + r.lost))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProbeRow {
    pub group: String,
    pub name: Option<String>,
    pub packets: u64,
    pub lost: u64,
    pub loss_percent: f64,
    pub packets_per_sec: f64,
    pub jitter_ms: Option<f64>,
    pub max_gap_ms: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Comparison {
    pub ssrc: u32,
    pub name: Option<String>,
    pub group: Option<String>,
    pub multicast: Option<PathStats>,
    pub unicast: PathStats,
    pub verdict: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PathStats {
    pub packets: u64,
    pub lost: u64,
}

pub fn is_multicast(group: &str) -> bool {
    group
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|a| a.ip().is_multicast())
}
