//! Audio: send (pages, tones, live input) and receive.

use super::{hostname, ms, opt_str, secs, NetRequest};
use anyhow::{bail, Result};
use mcast_audio::chime::{BuiltIn, Chime, ChimeSource};
use mcast_audio::codec::{OpusBandwidth, OpusMode, OpusOptions};
use mcast_audio::process::{Mode, ProcessOptions};
use mcast_audio::{receiver, sender, AudioFormat, Codec, StreamInfo, StreamSpec};
use mcast_core::rtcp::RtcpConfig;
use mcast_core::sap::{self, Directory, DirectoryEvent};
use mcast_core::Interface;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const DEFAULT_GROUP: &str = "239.69.0.1:5004";

fn default_group() -> SocketAddr {
    DEFAULT_GROUP.parse().expect("valid default group")
}

/// Codec, rate and channels; unset fields take the codec's defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormatRequest {
    pub encoding: Option<Codec>,
    pub rate: Option<u32>,
    pub channels: Option<u16>,
}

impl FormatRequest {
    /// Stream spec (codec-fixed rates/channels win). L16/L24 default to
    /// 48 kHz stereo; G.711, G.722 and Opus to mono.
    pub fn spec(&self) -> StreamSpec {
        let d = AudioFormat::default();
        let codec = self.encoding.unwrap_or(Codec::L16);
        let default_channels = if codec == Codec::L16 || codec == Codec::L24 {
            d.channels
        } else {
            1
        };
        StreamSpec::new(
            codec,
            self.rate.unwrap_or(d.sample_rate),
            self.channels.unwrap_or(default_channels),
        )
    }

    pub fn any_set(&self) -> bool {
        self.rate.is_some() || self.channels.is_some() || self.encoding.is_some()
    }
}

/// What to send.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum SourceRequest {
    /// Sine tone.
    Tone {
        #[serde(default = "default_tone")]
        frequency: f32,
        #[serde(default = "default_level")]
        level_dbfs: f32,
    },
    /// A soft, slowly changing chord progression; the level is its peak.
    Pad {
        #[serde(default = "default_level")]
        level_dbfs: f32,
    },
    /// An audio file (WAV, MP3, FLAC, Ogg, AAC/M4A, ALAC).
    File {
        path: PathBuf,
        #[serde(default, rename = "loop")]
        looped: bool,
    },
    /// Audio files one after another, as one stream (a playlist), with
    /// `gap_ms` of silence between them.
    Files {
        paths: Vec<PathBuf>,
        #[serde(default)]
        gap_ms: u32,
    },
    /// Live input; `None` is the default device, otherwise a substring of its name.
    Capture {
        #[serde(default)]
        device: Option<String>,
    },
    /// No audio, just the chime: the default chime unless `chime` or
    /// `end_chime` says otherwise.
    Chime,
    /// Audio streamed in as it happens (`POST /jobs/{id}/live`), as 16-bit
    /// little-endian PCM at `rate` with `channels`. Ends, with the end
    /// chime, when the stream does.
    Live {
        #[serde(default = "default_live_rate")]
        rate: u32,
        #[serde(default = "one_channel")]
        channels: u16,
    },
}

/// Most files a `files` source plays.
pub const MAX_FILES: usize = 100;

fn default_live_rate() -> u32 {
    48_000
}

fn one_channel() -> u16 {
    1
}

fn default_tone() -> f32 {
    440.0
}

fn default_level() -> f32 {
    -12.0
}

impl Default for SourceRequest {
    fn default() -> Self {
        SourceRequest::Tone {
            frequency: default_tone(),
            level_dbfs: default_level(),
        }
    }
}

/// A chime before or after the audio. In JSON: `"default"`, a built-in name
/// such as `"falling"`, or `{"file": "tone.wav"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ChimeJson", into = "ChimeJson")]
pub enum ChimeRequest {
    /// `$MCAST_CHIME` (or `$MCAST_END_CHIME`) when set, otherwise the rising
    /// chime before the audio and the falling one after it.
    Default,
    BuiltIn(BuiltIn),
    /// An audio file.
    File(PathBuf),
}

impl ChimeRequest {
    /// From the command line: "default", a built-in name, or else a file.
    /// (Built-in names win; use `./bell` for a file called `bell`.)
    pub fn parse_arg(s: &str) -> Self {
        match s {
            "default" => ChimeRequest::Default,
            _ => s
                .parse()
                .map_or_else(|_| ChimeRequest::File(s.into()), ChimeRequest::BuiltIn),
        }
    }

    fn resolve(&self, env: &str, default: BuiltIn, gap_ms: f64) -> Chime {
        let source = match self {
            ChimeRequest::Default => std::env::var_os(env)
                .filter(|p| !p.is_empty())
                .map_or(ChimeSource::BuiltIn(default), |p| {
                    ChimeSource::File(p.into())
                }),
            ChimeRequest::BuiltIn(b) => ChimeSource::BuiltIn(*b),
            ChimeRequest::File(p) => ChimeSource::File(p.clone()),
        };
        Chime {
            source,
            gap: ms(gap_ms),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ChimeJson {
    Name(String),
    File(ChimeFile),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChimeFile {
    file: PathBuf,
}

impl TryFrom<ChimeJson> for ChimeRequest {
    type Error = String;

    fn try_from(j: ChimeJson) -> Result<Self, String> {
        Ok(match j {
            ChimeJson::Name(n) if n == "default" => ChimeRequest::Default,
            ChimeJson::Name(n) => ChimeRequest::BuiltIn(n.parse()?),
            ChimeJson::File(f) => ChimeRequest::File(f.file),
        })
    }
}

impl From<ChimeRequest> for ChimeJson {
    fn from(c: ChimeRequest) -> Self {
        match c {
            ChimeRequest::Default => ChimeJson::Name("default".into()),
            ChimeRequest::BuiltIn(b) => ChimeJson::Name(b.name().into()),
            ChimeRequest::File(file) => ChimeJson::File(ChimeFile { file }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SendRequest {
    pub group: SocketAddr,
    /// "any", an IPv4 interface address, or an interface index; `None` uses
    /// the caller's default.
    #[serde(with = "opt_str")]
    pub interface: Option<Interface>,
    /// TTL / hop limit.
    pub ttl: u32,
    /// Deliver our own packets to receivers on this host.
    pub loopback: bool,
    /// l16, l24, pcmu/g711u, pcma/g711a, g722 or opus.
    #[serde(with = "opt_str")]
    pub encoding: Option<Codec>,
    pub rate: Option<u32>,
    pub channels: Option<u16>,
    /// IP TOS byte, e.g. 184 (DSCP EF).
    pub tos: Option<u32>,
    /// Packet time in ms; `None` uses the codec's default.
    pub ptime_ms: Option<f64>,
    pub payload_type: Option<u8>,
    pub opus_bitrate_kbps: u32,
    /// Opus tuning; `None` follows `mode`.
    #[serde(with = "opt_str")]
    pub opus_mode: Option<OpusMode>,
    #[serde(with = "opt_str")]
    pub opus_bandwidth: Option<OpusBandwidth>,
    pub opus_fec: bool,
    pub source: SourceRequest,
    /// Processing: "voice", "music" or "none".
    #[serde(with = "opt_str")]
    pub mode: Option<Mode>,
    pub volume_db: Option<f64>,
    pub lead_in_ms: f64,
    pub tail_ms: f64,
    pub chime: Option<ChimeRequest>,
    pub chime_gap_ms: f64,
    /// Chime after the audio, before the tail.
    pub end_chime: Option<ChimeRequest>,
    /// Silence between the audio and the end chime.
    pub end_chime_gap_ms: f64,
    /// Start this many seconds into a file.
    pub start_s: Option<f64>,
    /// Send this many seconds, then stop.
    pub duration_s: Option<f64>,
    /// Write what a receiver would decode to this WAV instead of sending.
    pub render: Option<PathBuf>,
    /// Name announced via SAP; `None` is "<hostname> audio".
    pub name: Option<String>,
    /// Announce the stream with SAP.
    pub sap: bool,
    pub sap_group: Option<SocketAddr>,
    pub sap_interval_s: f64,
    /// Send RTCP sender reports (and BYE at the end) on the RTP port + 1.
    pub rtcp: bool,
    /// Average seconds between RTCP reports.
    pub rtcp_interval_s: f64,
}

impl Default for SendRequest {
    fn default() -> Self {
        Self {
            group: default_group(),
            interface: None,
            ttl: 16,
            loopback: true,
            encoding: None,
            rate: None,
            channels: None,
            tos: None,
            ptime_ms: None,
            payload_type: None,
            opus_bitrate_kbps: 64,
            opus_mode: None,
            opus_bandwidth: None,
            opus_fec: true,
            source: SourceRequest::default(),
            mode: None,
            volume_db: None,
            lead_in_ms: 0.0,
            tail_ms: 300.0,
            chime: None,
            chime_gap_ms: 400.0,
            end_chime: None,
            end_chime_gap_ms: 400.0,
            start_s: None,
            duration_s: None,
            render: None,
            name: None,
            sap: true,
            sap_group: None,
            sap_interval_s: 5.0,
            rtcp: true,
            rtcp_interval_s: 5.0,
        }
    }
}

impl SendRequest {
    pub fn net(&self) -> NetRequest {
        NetRequest {
            interface: self.interface,
            ttl: self.ttl,
            loopback: self.loopback,
        }
    }

    pub fn format(&self) -> FormatRequest {
        FormatRequest {
            encoding: self.encoding,
            rate: self.rate,
            channels: self.channels,
        }
    }

    /// Whether the stream runs until stopped.
    pub fn endless(&self) -> bool {
        let looped_or_live = match &self.source {
            SourceRequest::File { looped, .. } => *looped,
            SourceRequest::Chime | SourceRequest::Files { .. } => false,
            _ => true,
        };
        looped_or_live && self.duration_s.is_none()
    }

    pub fn mode(&self) -> Mode {
        self.mode.unwrap_or_default()
    }

    /// The library config for this request.
    pub fn to_config(&self, default_iface: Interface) -> Result<sender::SenderConfig> {
        if self.render.is_some() && self.endless() {
            bail!("rendering needs an end: a file without looping, or a duration");
        }
        if self.start_s.is_some() && !matches!(self.source, SourceRequest::File { .. }) {
            bail!("a start offset needs a file source");
        }
        let mode = self.mode();
        let source = match &self.source {
            SourceRequest::File { path, looped } => sender::Source::File {
                path: path.clone(),
                looped: *looped,
            },
            SourceRequest::Files { paths, gap_ms } => {
                if paths.is_empty() || paths.len() > MAX_FILES {
                    bail!("a files source plays 1 to {MAX_FILES} files");
                }
                if *gap_ms > 60_000 {
                    bail!("gap_ms is at most 60000");
                }
                sender::Source::Files {
                    paths: paths.clone(),
                    gap: Duration::from_millis(*gap_ms as u64),
                }
            }
            SourceRequest::Capture { device } => sender::Source::Capture {
                device: device.clone().filter(|d| d != "default"),
            },
            SourceRequest::Tone {
                frequency,
                level_dbfs,
            } => sender::Source::Tone {
                frequency: *frequency,
                amplitude: 10f32.powf(*level_dbfs / 20.0),
            },
            SourceRequest::Chime => sender::Source::Silent,
            SourceRequest::Pad { level_dbfs } => sender::Source::Pad {
                amplitude: 10f32.powf(*level_dbfs / 20.0),
            },
            SourceRequest::Live { rate, channels } => {
                if !(8_000..=96_000).contains(rate) || !(1..=2).contains(channels) {
                    bail!("a live source needs a rate of 8000 to 96000 and 1 or 2 channels");
                }
                sender::Source::Live {
                    feed: mcast_audio::live::LiveFeed::new(*rate, *channels),
                }
            }
        };
        // Like mcast-play.sh: Opus tuning follows the content; with no mode,
        // band-limited Opus (its "opuswb") is treated as speech.
        let opus_mode = self
            .opus_mode
            .or(mode.opus_mode())
            .unwrap_or(match self.opus_bandwidth {
                Some(OpusBandwidth::Narrow | OpusBandwidth::Wide) => OpusMode::Voice,
                _ => OpusMode::Music,
            });
        let name = self
            .name
            .clone()
            .unwrap_or_else(|| format!("{} audio", hostname()));
        let rtcp = self.rtcp.then(|| RtcpConfig {
            name: Some(name.clone()),
            interval: Duration::from_secs_f64(self.rtcp_interval_s.max(0.1)),
        });
        let announce = (self.sap && self.render.is_none()).then(|| sender::Announce {
            name,
            sap_group: self.sap_group,
            interval: Duration::from_secs_f64(self.sap_interval_s.max(0.5)),
        });
        let mut net = self.net().options(self.group, default_iface)?;
        net.tos = self.tos;
        Ok(sender::SenderConfig {
            net,
            stream: self.format().spec(),
            match_file_format: self.rate.is_none() && self.channels.is_none(),
            ptime: self.ptime_ms.map(ms),
            payload_type: self.payload_type,
            opus: OpusOptions {
                mode: opus_mode,
                bitrate: self.opus_bitrate_kbps * 1000,
                max_bandwidth: self.opus_bandwidth,
                fec: self.opus_fec,
                ..OpusOptions::default()
            },
            process: ProcessOptions {
                mode,
                volume_db: self.volume_db,
            },
            source,
            start: self.start_s.map(|s| secs(s, "start")).transpose()?,
            duration: self.duration_s.map(|s| secs(s, "duration")).transpose()?,
            lead_in: ms(self.lead_in_ms),
            chime: self.chime(),
            end_chime: self.end_chime(),
            tail: ms(self.tail_ms),
            render: self.render.clone(),
            announce,
            rtcp,
        })
    }

    pub fn chime(&self) -> Option<Chime> {
        let chime_only = self.source == SourceRequest::Chime && self.end_chime.is_none();
        let c = match &self.chime {
            Some(c) => c,
            None if chime_only => &ChimeRequest::Default,
            None => return None,
        };
        Some(c.resolve("MCAST_CHIME", BuiltIn::Rising, self.gap(self.chime_gap_ms)))
    }

    pub fn end_chime(&self) -> Option<Chime> {
        let c = self.end_chime.as_ref()?;
        let gap = self.gap(self.end_chime_gap_ms);
        Some(c.resolve("MCAST_END_CHIME", BuiltIn::Falling, gap))
    }

    /// With no audio between them, chimes need no gap.
    fn gap(&self, ms: f64) -> f64 {
        if self.source == SourceRequest::Chime {
            0.0
        } else {
            ms
        }
    }
}

/// Where received audio goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum SinkRequest {
    /// An output device (`None` is the default; otherwise a substring of its name).
    Play {
        #[serde(default)]
        device: Option<String>,
    },
    /// A WAV file.
    Wav { path: PathBuf },
}

impl Default for SinkRequest {
    fn default() -> Self {
        SinkRequest::Play { device: None }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecvRequest {
    /// Join this group directly; otherwise find a stream announced via SAP.
    pub group: Option<SocketAddr>,
    /// Pick the announced stream whose name contains this text.
    pub stream: Option<String>,
    /// Seconds to wait for a SAP announcement.
    pub wait_s: f64,
    /// SAP groups to listen on; empty uses the defaults.
    pub sap_groups: Vec<SocketAddr>,
    /// "any", an IPv4 interface address, or an interface index; `None` uses
    /// the caller's default.
    #[serde(with = "opt_str")]
    pub interface: Option<Interface>,
    /// TTL / hop limit.
    pub ttl: u32,
    /// Deliver our own packets to receivers on this host.
    pub loopback: bool,
    /// With `group`: how to decode (ignored for announced streams).
    /// l16, l24, pcmu/g711u, pcma/g711a, g722 or opus.
    #[serde(with = "opt_str")]
    pub encoding: Option<Codec>,
    pub rate: Option<u32>,
    pub channels: Option<u16>,
    pub latency_ms: f64,
    pub volume_db: Option<f64>,
    pub sink: SinkRequest,
    pub duration_s: Option<f64>,
    /// Seconds between stats updates (0 disables them).
    pub stats_interval_s: f64,
    /// Listen for the sender's RTCP and send receiver reports (RTP port + 1).
    pub rtcp: bool,
    pub rtcp_interval_s: f64,
    /// Our name in receiver reports, e.g. the room; `None` is the hostname.
    pub name: Option<String>,
}

impl Default for RecvRequest {
    fn default() -> Self {
        Self {
            group: None,
            stream: None,
            wait_s: 15.0,
            sap_groups: Vec::new(),
            interface: None,
            ttl: 16,
            loopback: true,
            encoding: None,
            rate: None,
            channels: None,
            latency_ms: 40.0,
            volume_db: None,
            sink: SinkRequest::default(),
            duration_s: None,
            stats_interval_s: 2.0,
            rtcp: true,
            rtcp_interval_s: 5.0,
            name: None,
        }
    }
}

/// The stream a receive request settled on.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub group: SocketAddr,
    /// Fixed decoding, if known.
    pub stream: Option<StreamSpec>,
    /// Only accept this payload type.
    pub payload_type: Option<u8>,
    /// Decoding for dynamic payload types when `stream` is `None`.
    pub fallback: Option<StreamSpec>,
    /// The SAP announcement it came from.
    pub announced: Option<StreamInfo>,
}

impl Resolved {
    pub fn from_info(info: StreamInfo) -> Self {
        let spec = info.spec().expect("only playable streams are resolved");
        Self {
            group: info.group,
            stream: Some(spec),
            payload_type: Some(info.payload_type),
            fallback: None,
            announced: Some(info),
        }
    }
}

impl RecvRequest {
    pub fn net(&self) -> NetRequest {
        NetRequest {
            interface: self.interface,
            ttl: self.ttl,
            loopback: self.loopback,
        }
    }

    pub fn format(&self) -> FormatRequest {
        FormatRequest {
            encoding: self.encoding,
            rate: self.rate,
            channels: self.channels,
        }
    }

    pub fn sap_groups(&self) -> Vec<SocketAddr> {
        if self.sap_groups.is_empty() {
            sap::default_listen_groups()
        } else {
            self.sap_groups.clone()
        }
    }

    /// With an explicit group, the stream to join; `None` means SAP discovery is needed.
    pub fn direct(&self) -> Option<Resolved> {
        let group = self.group?;
        Some(Resolved {
            group,
            stream: self.format().any_set().then(|| self.format().spec()),
            payload_type: None,
            fallback: Some(StreamSpec::new(Codec::L16, 48_000, 2)),
            announced: None,
        })
    }

    /// Whether an announced stream is one this request would play.
    pub fn matches(&self, info: &StreamInfo) -> bool {
        info.spec().is_some()
            && self.stream.as_ref().map_or(true, |w| {
                info.name.to_lowercase().contains(&w.to_lowercase())
            })
    }

    /// Find the stream: the explicit group, else wait up to `wait_s` for a
    /// matching SAP announcement. `on_skip` sees announcements that don't
    /// match. `Ok(None)` means `stop` was set first.
    pub fn resolve(
        &self,
        default_iface: Interface,
        stop: &AtomicBool,
        mut on_skip: impl FnMut(&StreamInfo),
    ) -> Result<Option<Resolved>> {
        if let Some(r) = self.direct() {
            return Ok(Some(r));
        }
        let iface = self.interface.unwrap_or(default_iface);
        // Forget sessions that stop announcing (senders re-announce every few seconds to ~minutes).
        let mut dir = Directory::open(&self.sap_groups(), iface, Duration::from_secs(300))?;
        let deadline = Instant::now() + secs(self.wait_s, "wait")?;
        while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
            for event in dir.poll(deadline.saturating_duration_since(Instant::now()))? {
                let (DirectoryEvent::Added(s) | DirectoryEvent::Changed(s)) = event else {
                    continue;
                };
                let Some(info) = StreamInfo::from_sdp(&s.sdp) else {
                    continue;
                };
                if !self.matches(&info) {
                    on_skip(&info);
                    continue;
                }
                return Ok(Some(Resolved::from_info(info)));
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match &self.stream {
            Some(n) => bail!(
                "no playable stream named \"{n}\" announced within {}s",
                self.wait_s
            ),
            None => bail!("no playable stream announced within {}s", self.wait_s),
        }
    }

    pub fn to_config(
        &self,
        r: &Resolved,
        default_iface: Interface,
    ) -> Result<receiver::ReceiverConfig> {
        let sink = match &self.sink {
            SinkRequest::Wav { path } => receiver::Sink::Wav { path: path.clone() },
            SinkRequest::Play { device } => receiver::Sink::Play {
                device: device.clone(),
            },
        };
        Ok(receiver::ReceiverConfig {
            net: self.net().options(r.group, default_iface)?,
            stream: r.stream,
            fallback: r.fallback,
            payload_type: r.payload_type,
            latency: ms(self.latency_ms),
            volume_db: self.volume_db,
            sink,
            duration: self.duration_s.map(|s| secs(s, "duration")).transpose()?,
            stats_interval: if self.stats_interval_s > 0.0 {
                Duration::from_secs_f64(self.stats_interval_s)
            } else {
                Duration::MAX
            },
            rtcp: self.rtcp.then(|| RtcpConfig {
                name: Some(self.name.clone().unwrap_or_else(hostname)),
                interval: Duration::from_secs_f64(self.rtcp_interval_s.max(0.1)),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_json_matches_cli_defaults() {
        let r: SendRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r, SendRequest::default());
        let cfg = r.to_config(Interface::Any).unwrap();
        assert_eq!(cfg.net.group, default_group());
        assert_eq!(cfg.net.ttl, 16);
        assert_eq!(cfg.stream, StreamSpec::new(Codec::L16, 48_000, 2));
        assert_eq!(cfg.tail, Duration::from_millis(300));
        assert!(cfg.announce.is_some());
    }

    #[test]
    fn parses_a_page() {
        let r: SendRequest = serde_json::from_str(
            r#"{"group": "239.192.2.1:10000", "encoding": "g711u", "mode": "voice",
                "source": {"type": "file", "path": "page.wav"}, "chime": "default",
                "lead_in_ms": 1000, "tos": 184, "sap": false, "interface": "10.0.0.5"}"#,
        )
        .unwrap();
        let cfg = r.to_config(Interface::Any).unwrap();
        assert_eq!(cfg.stream.codec, Codec::Pcmu);
        assert_eq!(cfg.process.mode, Mode::Voice);
        assert_eq!(cfg.opus.mode, OpusMode::Voice);
        assert_eq!(cfg.net.tos, Some(184));
        assert_eq!(cfg.net.interface, "10.0.0.5".parse().unwrap());
        assert_eq!(cfg.lead_in, Duration::from_secs(1));
        assert!(cfg.chime.is_some());
        assert!(cfg.end_chime.is_none());
        assert!(cfg.announce.is_none());
        assert!(!r.endless());
        // Round-trips.
        let back: SendRequest = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn rejects_unknown_fields_and_values() {
        assert!(serde_json::from_str::<SendRequest>(r#"{"grop": "239.1.1.1:5000"}"#).is_err());
        assert!(serde_json::from_str::<SendRequest>(r#"{"encoding": "mp3"}"#).is_err());
        assert!(
            serde_json::from_str::<SendRequest>(r#"{"group": "10.0.0.1:5000"}"#)
                .unwrap()
                .to_config(Interface::Any)
                .is_err()
        );
    }

    #[test]
    fn pad_source() {
        let r: SendRequest = serde_json::from_str(r#"{"source": {"type": "pad"}}"#).unwrap();
        let cfg = r.to_config(Interface::Any).unwrap();
        assert!(
            matches!(cfg.source, sender::Source::Pad { amplitude } if (amplitude - 0.251).abs() < 0.001)
        );
        assert!(r.endless());
    }

    #[test]
    fn render_needs_an_end() {
        let r = SendRequest {
            render: Some("out.wav".into()),
            ..SendRequest::default()
        };
        assert!(r.to_config(Interface::Any).is_err());
    }

    #[test]
    fn receive_request_defaults() {
        let r: RecvRequest =
            serde_json::from_str(r#"{"group": "239.192.2.1:10000", "encoding": "opus"}"#).unwrap();
        let d = r.direct().unwrap();
        assert_eq!(d.stream.unwrap().codec, Codec::Opus);
        let cfg = r.to_config(&d, Interface::Any).unwrap();
        assert_eq!(cfg.latency, Duration::from_millis(40));
        assert!(matches!(cfg.sink, receiver::Sink::Play { device: None }));
    }

    #[test]
    fn parses_chimes() {
        let r: SendRequest = serde_json::from_str(
            r#"{"chime": "ding-dong", "end_chime": {"file": "bye.wav"}, "end_chime_gap_ms": 250}"#,
        )
        .unwrap();
        assert_eq!(r.chime, Some(ChimeRequest::BuiltIn(BuiltIn::DingDong)));
        assert_eq!(r.end_chime, Some(ChimeRequest::File("bye.wav".into())));
        let end = r.end_chime().unwrap();
        assert_eq!(end.source, ChimeSource::File("bye.wav".into()));
        assert_eq!(end.gap, Duration::from_millis(250));
        let back: SendRequest = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(back, r);

        let r: SendRequest = serde_json::from_str(r#"{"end_chime": "default"}"#).unwrap();
        if std::env::var_os("MCAST_END_CHIME").is_none() {
            assert_eq!(
                r.end_chime().unwrap().source,
                ChimeSource::BuiltIn(BuiltIn::Falling)
            );
        }
        for bad in [
            r#"{"chime": "foghorn"}"#,
            r#"{"chime": {"file": "a.wav", "x": 1}}"#,
        ] {
            assert!(serde_json::from_str::<SendRequest>(bad).is_err(), "{bad}");
        }
        assert_eq!(
            ChimeRequest::parse_arg("bell"),
            ChimeRequest::BuiltIn(BuiltIn::Bell)
        );
        assert_eq!(
            ChimeRequest::parse_arg("bell.wav"),
            ChimeRequest::File("bell.wav".into())
        );
        assert_eq!(ChimeRequest::parse_arg("default"), ChimeRequest::Default);
    }

    #[test]
    fn chime_only_defaults_to_the_rising_chime() {
        let r: SendRequest = serde_json::from_str(r#"{"source": {"type": "chime"}}"#).unwrap();
        assert!(!r.endless());
        let cfg = r.to_config(Interface::Any).unwrap();
        assert!(matches!(cfg.source, sender::Source::Silent));
        let chime = cfg.chime.unwrap();
        assert_eq!(chime.gap, Duration::ZERO);
        if std::env::var_os("MCAST_CHIME").is_none() {
            assert_eq!(chime.source, ChimeSource::BuiltIn(BuiltIn::Rising));
        }
        assert!(cfg.end_chime.is_none());

        // An end chime alone is a chime too.
        let r: SendRequest =
            serde_json::from_str(r#"{"source": {"type": "chime"}, "end_chime": "bell"}"#).unwrap();
        assert!(r.chime().is_none());
        assert_eq!(
            r.end_chime().unwrap().source,
            ChimeSource::BuiltIn(BuiltIn::Bell)
        );
    }
}
