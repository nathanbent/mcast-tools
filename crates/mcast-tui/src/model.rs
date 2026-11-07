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
    pub request: Value,
}

impl Job {
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "starting" | "running" | "stopping")
    }

    fn stat(&self, key: &str) -> f64 {
        self.stats[key].as_f64().unwrap_or(0.0)
    }

    /// Whether the job sends text that can be typed into.
    pub fn text_sender(&self) -> bool {
        self.kind == "text" && self.request["send"].as_bool() != Some(false)
    }

    fn text(&self, key: &str) -> String {
        self.stats[key].as_str().unwrap_or("").to_string()
    }

    pub fn title(&self) -> String {
        let verb = match self.kind.as_str() {
            "send" => "Paging",
            "recv" => "Playing",
            "text" => "Text",
            "file_send" => "Sending files",
            "file_recv" => "Receiving files",
            other => other,
        };
        match (&self.name, &self.group) {
            (Some(n), _) => format!("{verb} \"{n}\""),
            (None, Some(g)) => format!("{verb} {g}"),
            _ => verb.to_string(),
        }
    }

    pub fn stats_line(&self) -> String {
        match self.kind.as_str() {
            "text" => {
                return format!(
                    "{} sent · {} received · {} others heard",
                    self.stat("lines_sent"),
                    self.stat("lines_received"),
                    self.stat("senders")
                )
            }
            "file_send" => {
                let file = self.text("file");
                if file.is_empty() {
                    return "starting".into();
                }
                let count = self.stat("count") as u64;
                let of = if count > 1 {
                    format!(" ({} of {count})", self.stat("index"))
                } else {
                    String::new()
                };
                let progress = match self.stats["total"].as_f64() {
                    _ if self.stats["looping"].as_bool() == Some(true) => " · looping".to_string(),
                    Some(t) if t > 0.0 => {
                        format!(" · {:.0} %", (self.stat("sent") * 100.0 / t).min(100.0))
                    }
                    _ => String::new(),
                };
                return format!("{file}{of}{progress} · {} packets", self.stat("sent"));
            }
            "file_recv" => {
                let now = match self.stats["receiving"].as_str() {
                    Some(f) => format!("receiving {f} ({} %)", self.stat("percent")),
                    None => "waiting for files".into(),
                };
                let failed = self.stat("failed");
                return format!(
                    "{now} · {} saved{}",
                    self.stat("received"),
                    if failed > 0.0 {
                        format!(" · {failed} failed")
                    } else {
                        String::new()
                    }
                );
            }
            _ => {}
        }
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
    pub text: Option<TextStream>,
    pub file: Option<FileStream>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TextStream {
    pub group: String,
    pub t140_pt: u8,
    pub red_pt: Option<u8>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileStream {
    pub group: String,
    pub payload_type: u8,
}

/// A line on a text channel.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TextLine {
    pub job: u64,
    pub from: String,
    pub text: String,
    pub own: bool,
    pub at_ms: u64,
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

/// A built-in chime the server can play.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Chime {
    pub name: String,
    pub category: String,
    pub description: String,
    /// Hidden from lists (but still playable).
    pub hidden: bool,
}

/// A phonebook entry: a group saved under a name.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BookEntry {
    pub name: String,
    pub group: String,
    pub description: String,
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
