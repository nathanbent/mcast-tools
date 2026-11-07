use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use mcast_audio::chime::{Chime, ChimeSource};
use mcast_audio::codec::{OpusBandwidth, OpusMode, OpusOptions};
use mcast_audio::process::{Mode, ProcessOptions};
use mcast_audio::{receiver, sender, AudioFormat, Codec, StreamInfo, StreamSpec};
use mcast_core::sap::{self, Directory, DirectoryEvent};
use mcast_core::{Interface, MulticastOptions};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DEFAULT_GROUP: &str = "239.69.0.1:5004";

#[derive(Parser)]
#[command(name = "mcast", version, about = "Multicast tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

// Parsed once at startup; variant size doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Audio over RTP multicast: send, receive, discover
    #[command(subcommand)]
    Audio(AudioCommand),
    /// Watch multicast streams: rate, loss, jitter, TTL, DSCP and audio level
    Monitor(MonitorArgs),
}

#[derive(Subcommand)]
enum AudioCommand {
    /// Stream a test tone, WAV file, or live input to a multicast group
    Send(SendArgs),
    /// Play (or record) a stream; finds one via SAP unless --group is given
    Recv(RecvArgs),
    /// List audio streams announced with SAP
    Discover(DiscoverArgs),
    /// List audio input and output devices
    Devices,
}

#[derive(Args)]
struct NetArgs {
    /// Interface: IPv4 address (IPv4 groups) or interface index (IPv6 groups)
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    /// Multicast TTL / hop limit
    #[arg(long, default_value_t = 16)]
    ttl: u32,
    /// Don't loop packets back to receivers on this host
    #[arg(long)]
    no_loopback: bool,
}

impl NetArgs {
    fn options(&self, group: SocketAddr) -> Result<MulticastOptions> {
        let mut o = MulticastOptions::new(group)?;
        o.interface = self.iface;
        o.ttl = self.ttl;
        o.loopback = !self.no_loopback;
        Ok(o)
    }
}

#[derive(Args)]
struct FormatArgs {
    /// Sample rate in Hz [default: 48000]
    #[arg(short, long)]
    rate: Option<u32>,
    /// Channel count [default: 2]
    #[arg(short, long)]
    channels: Option<u16>,
    /// Encoding: l16, l24, pcmu/g711u, pcma/g711a, g722, opus [default: l16]
    #[arg(short, long)]
    encoding: Option<Codec>,
}

impl FormatArgs {
    /// Stream spec (codec-fixed rates/channels win). L16/L24 default to
    /// 48 kHz stereo; G.711, G.722 and Opus to mono.
    fn spec(&self) -> StreamSpec {
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

    fn any_set(&self) -> bool {
        self.rate.is_some() || self.channels.is_some() || self.encoding.is_some()
    }
}

#[derive(Args)]
struct SapListenArgs {
    /// SAP group(s) to listen on [default: 239.255.255.255, 239.195.255.255, 224.2.127.254 port 9875]
    #[arg(long = "sap-group", value_name = "ADDR:PORT")]
    sap_groups: Vec<SocketAddr>,
}

impl SapListenArgs {
    fn open(&self, iface: Interface) -> Result<Directory> {
        let groups = if self.sap_groups.is_empty() {
            sap::default_listen_groups()
        } else {
            self.sap_groups.clone()
        };
        // Forget sessions that stop announcing (senders re-announce every few seconds to ~minutes).
        Ok(Directory::open(&groups, iface, Duration::from_secs(300))?)
    }
}

#[derive(Args)]
struct SendArgs {
    /// Multicast group and port, e.g. 239.69.0.1:5004 or [ff15::69]:5004
    #[arg(short, long, default_value = DEFAULT_GROUP)]
    group: SocketAddr,
    #[command(flatten)]
    net: NetArgs,
    #[command(flatten)]
    format: FormatArgs,
    /// Packet time in ms [default: 1 for L16/L24 (AES67), 20 for G.711/G.722/Opus]
    #[arg(long)]
    ptime: Option<f64>,
    /// RTP payload type [default: 0 PCMU, 8 PCMA, 9 G.722, 107 Opus, 96 L16/L24]
    #[arg(long)]
    pt: Option<u8>,
    /// IP TOS byte for the audio packets, e.g. 184 (DSCP EF) for voice QoS
    #[arg(long)]
    tos: Option<u32>,
    /// Opus bitrate in kbit/s
    #[arg(long, default_value_t = 64)]
    bitrate: u32,
    /// Opus tuning: voice or music [default: follows --mode; voice for
    /// wideband/narrowband Opus with no mode, else music]
    #[arg(long)]
    opus_mode: Option<OpusMode>,
    /// Opus maximum audio bandwidth: nb (4 kHz), wb (8 kHz), swb (12 kHz), fb (20 kHz)
    #[arg(long)]
    opus_bandwidth: Option<OpusBandwidth>,
    /// Disable Opus in-band forward error correction
    #[arg(long)]
    no_fec: bool,
    /// Send a sine tone at this frequency (Hz) [default source]
    #[arg(long, group = "source")]
    tone: Option<f32>,
    /// Tone level in dBFS
    #[arg(long, default_value_t = -12.0, allow_negative_numbers = true)]
    level: f32,
    /// Send an audio file: WAV, MP3, FLAC, Ogg Vorbis, AAC/M4A, ALAC
    /// (resampled to the codec's rate; L16/L24 keep the file's rate and
    /// channels unless --rate/--channels are given)
    #[arg(
        long = "file",
        visible_alias = "wav",
        group = "source",
        value_name = "FILE"
    )]
    wav: Option<PathBuf>,
    /// Loop the file
    #[arg(long = "loop", requires = "wav")]
    looped: bool,
    /// Capture from an input device (substring of its name, or "default")
    #[arg(long, group = "source", num_args = 0..=1, default_missing_value = "default")]
    capture: Option<String>,
    /// Processing tuned for the content and codec: voice, music or none
    #[arg(short, long, default_value = "none")]
    mode: Mode,
    /// Shortcut for --mode voice
    #[arg(short = 'p', long = "voice", conflicts_with = "mode")]
    voice: bool,
    /// Volume change in dB after processing, e.g. -6 or 3 (limited to avoid clipping)
    #[arg(short, long, allow_negative_numbers = true)]
    volume: Option<f64>,
    /// Lead-in silence in ms, so the first word isn't clipped
    #[arg(short = 'L', long, default_value_t = 0.0)]
    lead_in: f64,
    /// Silence after the audio in ms, so the end isn't cut off
    #[arg(short = 'E', long, default_value_t = 300.0)]
    tail: f64,
    /// Pre-tone before the audio: an audio file, or no value for the built-in
    /// chime ($MCAST_CHIME, if set, replaces the built-in one)
    #[arg(short = 'C', long, value_name = "FILE", num_args = 0..=1, default_missing_value = "default")]
    chime: Option<String>,
    /// Gap between the chime and the audio, in ms
    #[arg(short = 'G', long, default_value_t = 400.0, requires = "chime")]
    chime_gap: f64,
    /// Start this many seconds into the file (adds a short fade-in)
    #[arg(short, long, requires = "wav")]
    start: Option<f64>,
    /// Send this many seconds of the source (adds a short fade-out), then stop
    #[arg(short, long, short_alias = 't')]
    duration: Option<f64>,
    /// Render to a WAV of exactly what a receiver would decode, instead of sending
    #[arg(short = 'o', long, value_name = "FILE")]
    render: Option<PathBuf>,
    /// Stream name announced to receivers [default: "<hostname> audio"]
    #[arg(short, long)]
    name: Option<String>,
    /// Don't announce the stream with SAP
    #[arg(long)]
    no_sap: bool,
    /// SAP group to announce on [default: 239.255.255.255:9875 for 239.x streams]
    #[arg(long, value_name = "ADDR:PORT")]
    sap_group: Option<SocketAddr>,
    /// Seconds between SAP announcements
    #[arg(long, default_value_t = 5.0)]
    sap_interval: f64,
    /// Also write the stream's SDP to this file (for ffplay, VLC, etc.)
    #[arg(long, value_name = "FILE")]
    sdp: Option<PathBuf>,
}

#[derive(Args)]
struct RecvArgs {
    /// Multicast group and port; skips SAP discovery. The codec comes from
    /// --encoding, or from the payload type for G.711/G.722 (else L16/48000/2)
    #[arg(short, long)]
    group: Option<SocketAddr>,
    /// Pick the announced stream whose name contains this text
    #[arg(short, long, conflicts_with = "group")]
    stream: Option<String>,
    /// Seconds to wait for a SAP announcement
    #[arg(short, long, default_value_t = 15.0)]
    wait: f64,
    #[command(flatten)]
    sap: SapListenArgs,
    #[command(flatten)]
    net: NetArgs,
    #[command(flatten)]
    format: FormatArgs,
    /// Playout latency in milliseconds
    #[arg(short, long, default_value_t = 40.0)]
    latency: f64,
    /// Output device (substring of its name)
    #[arg(long, conflicts_with = "wav")]
    device: Option<String>,
    /// Record to a WAV file instead of playing
    #[arg(long)]
    wav: Option<PathBuf>,
    /// Stop after this many seconds
    #[arg(short, long)]
    duration: Option<f64>,
    /// Seconds between stats lines (0 to disable)
    #[arg(long, default_value_t = 2.0)]
    stats: f64,
}

#[derive(Args)]
struct MonitorArgs {
    /// Groups to watch (ADDR:PORT). With none, watches every stream announced via SAP
    #[arg(value_name = "GROUP")]
    groups: Vec<SocketAddr>,
    /// Also watch SAP-announced streams when groups are given
    #[arg(long)]
    sap: bool,
    #[command(flatten)]
    sap_listen: SapListenArgs,
    /// Interface: IPv4 address (IPv4 groups) or interface index (IPv6 groups)
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    /// Codec for level metering when the payload type is dynamic and not announced
    #[arg(short, long)]
    encoding: Option<Codec>,
    /// Seconds between updates
    #[arg(long, default_value_t = 1.0)]
    interval: f64,
    /// Stop after this many seconds (default: run until Ctrl-C)
    #[arg(short, long)]
    duration: Option<f64>,
    /// Print a line per stream per interval instead of a live table
    #[arg(long)]
    log: bool,
}

#[derive(Args)]
struct DiscoverArgs {
    #[command(flatten)]
    sap: SapListenArgs,
    /// Interface: IPv4 address (IPv4 groups) or interface index (IPv6 groups)
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    /// Stop after this many seconds (default: run until Ctrl-C)
    #[arg(short, long)]
    duration: Option<f64>,
    /// Print each stream's full SDP
    #[arg(long)]
    sdp: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))?;
    }
    match cli.command {
        Command::Audio(AudioCommand::Send(a)) => audio_send(a, &stop),
        Command::Audio(AudioCommand::Recv(a)) => audio_recv(a, &stop),
        Command::Audio(AudioCommand::Discover(a)) => audio_discover(a, &stop),
        Command::Monitor(a) => monitor(a, &stop),
        Command::Audio(AudioCommand::Devices) => {
            for line in mcast_audio::device::list()? {
                println!("{line}");
            }
            Ok(())
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "mcast".into())
}

fn audio_send(a: SendArgs, stop: &AtomicBool) -> Result<()> {
    let mode = if a.voice { Mode::Voice } else { a.mode };
    let endless = (a.wav.is_none() || a.looped) && a.duration.is_none();
    if a.render.is_some() && endless {
        bail!("--render needs an end: a file without --loop, or --duration");
    }
    let source = if let Some(path) = a.wav {
        sender::Source::File {
            path,
            looped: a.looped,
        }
    } else if let Some(dev) = a.capture {
        sender::Source::Capture {
            device: (dev != "default").then_some(dev),
        }
    } else {
        sender::Source::Tone {
            frequency: a.tone.unwrap_or(440.0),
            amplitude: 10f32.powf(a.level / 20.0),
        }
    };
    // Like mcast-play.sh: Opus tuning follows the content; with no mode,
    // band-limited Opus (its "opuswb") is treated as speech.
    let opus_mode = a
        .opus_mode
        .or(mode.opus_mode())
        .unwrap_or(match a.opus_bandwidth {
            Some(OpusBandwidth::Narrow | OpusBandwidth::Wide) => OpusMode::Voice,
            _ => OpusMode::Music,
        });
    let ms = |v: f64| Duration::from_secs_f64(v.max(0.0) / 1000.0);
    let chime = a.chime.as_deref().map(|c| {
        let path = match c {
            "default" => std::env::var_os("MCAST_CHIME").filter(|p| !p.is_empty()),
            file => Some(file.into()),
        };
        Chime {
            source: path.map_or(ChimeSource::BuiltIn, |p| ChimeSource::File(p.into())),
            gap: ms(a.chime_gap),
        }
    });
    let announce = (!a.no_sap && a.render.is_none()).then(|| sender::Announce {
        name: a
            .name
            .clone()
            .unwrap_or_else(|| format!("{} audio", hostname())),
        sap_group: a.sap_group,
        interval: Duration::from_secs_f64(a.sap_interval.max(0.5)),
    });
    let mut net = a.net.options(a.group)?;
    net.tos = a.tos;
    let s = sender::Sender::new(sender::SenderConfig {
        net,
        stream: a.format.spec(),
        match_file_format: a.format.rate.is_none() && a.format.channels.is_none(),
        ptime: a.ptime.map(|ms| Duration::from_secs_f64(ms / 1000.0)),
        payload_type: a.pt,
        opus: OpusOptions {
            mode: opus_mode,
            bitrate: a.bitrate * 1000,
            max_bandwidth: a.opus_bandwidth,
            fec: !a.no_fec,
            ..OpusOptions::default()
        },
        process: ProcessOptions {
            mode,
            volume_db: a.volume,
        },
        source,
        start: a.start.map(Duration::from_secs_f64),
        duration: a.duration.map(Duration::from_secs_f64),
        lead_in: ms(a.lead_in),
        chime: chime.clone(),
        tail: ms(a.tail),
        render: a.render.clone(),
        announce,
    })?;
    if let Some(path) = &a.sdp {
        std::fs::write(path, s.sdp().to_string())?;
        eprintln!("wrote SDP to {}", path.display());
    }
    let spec = s.spec();
    let (src_rate, src_ch) = s.source_format();
    let conversion = if (src_rate, src_ch) != (spec.sample_rate, spec.channels) {
        format!(", converted from {src_rate} Hz/{src_ch}ch")
    } else {
        String::new()
    };
    let destination = match &a.render {
        Some(path) => format!("rendering to {}", path.display()),
        None => format!("sending to {}", a.group),
    };
    let mut details = format!(
        "pt {}, {} ms packets{conversion}",
        s.payload_type(),
        s.ptime().as_secs_f64() * 1000.0
    );
    if mode != Mode::None {
        details += &format!(", {mode} processing");
    }
    if let Some(v) = a.volume {
        details += &format!(", {v:+} dB");
    }
    if let Some(c) = &chime {
        details += &match &c.source {
            ChimeSource::BuiltIn => ", built-in chime".to_string(),
            ChimeSource::File(p) => format!(", chime {}", p.display()),
        };
    }
    eprintln!("{spec} {destination} ({details})");
    if let Some(l) = s.loudness() {
        match l.measured {
            Some(m) => eprintln!(
                "loudness: measured {m:.1} LUFS after processing; {:+.1} dB to reach {} LUFS",
                l.gain_db, l.target
            ),
            None => eprintln!("loudness: source is silent; no normalisation"),
        }
    }
    if a.render.is_some() {
        let stats = s.run(stop)?;
        eprintln!(
            "rendered {:.1}s of audio",
            stats.frames as f64 / spec.sample_rate as f64
        );
        return Ok(());
    }
    eprintln!("Ctrl-C to stop.");
    match s.sap_group() {
        Some(g) => eprintln!(
            "announcing \"{}\" via SAP on {g}; receive with: mcast audio recv",
            s.sdp().name
        ),
        None => {
            let mut cmd = format!(
                "mcast audio recv -g {} -e {}",
                a.group,
                spec.codec.to_string().to_lowercase()
            );
            if spec.codec.pcm_encoding().is_some() || spec.codec == Codec::Opus {
                cmd += &format!(" -c {}", spec.channels);
            }
            if spec.codec.pcm_encoding().is_some() {
                cmd += &format!(" -r {}", spec.sample_rate);
            }
            eprintln!("receive with: {cmd}");
        }
    }
    let stats = s.run(stop)?;
    eprintln!(
        "sent {} packets ({:.1}s of audio), {} send errors",
        stats.packets,
        stats.frames as f64 / spec.sample_rate as f64,
        stats.send_errors
    );
    Ok(())
}

/// Wait for a SAP-announced stream we can play, optionally matching `name`.
fn find_stream(
    dir: &mut Directory,
    name: Option<&str>,
    wait: Duration,
    stop: &AtomicBool,
) -> Result<Option<StreamInfo>> {
    let wanted = name.map(str::to_lowercase);
    let deadline = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        for event in dir.poll(deadline.saturating_duration_since(Instant::now()))? {
            let (DirectoryEvent::Added(s) | DirectoryEvent::Changed(s)) = event else {
                continue;
            };
            let Some(info) = StreamInfo::from_sdp(&s.sdp) else {
                continue;
            };
            if let Some(w) = &wanted {
                if !info.name.to_lowercase().contains(w) {
                    eprintln!("  skipping {info}");
                    continue;
                }
            }
            if info.spec().is_none() {
                eprintln!("  skipping {info}");
                continue;
            }
            return Ok(Some(info));
        }
    }
    Ok(None)
}

fn audio_recv(a: RecvArgs, stop: &AtomicBool) -> Result<()> {
    // (fixed spec, payload-type filter, fallback for dynamic payload types)
    let (group, stream, payload_type, fallback) = match a.group {
        Some(g) => {
            let explicit = a.format.any_set().then(|| a.format.spec());
            let fallback = StreamSpec::new(Codec::L16, 48_000, 2);
            match explicit {
                Some(spec) => eprintln!("joining {g} expecting {spec}. Ctrl-C to stop."),
                None => eprintln!(
                    "joining {g}; codec from payload type (G.711/G.722), else {fallback}. Ctrl-C to stop."
                ),
            }
            (g, explicit, None, Some(fallback))
        }
        None => {
            match &a.stream {
                Some(n) => eprintln!("looking for a stream named \"{n}\" via SAP..."),
                None => eprintln!("looking for streams via SAP..."),
            }
            let mut dir = a.sap.open(a.net.iface)?;
            let wait = Duration::from_secs_f64(a.wait);
            let Some(info) = find_stream(&mut dir, a.stream.as_deref(), wait, stop)? else {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                bail!(
                    "no playable stream announced within {}s; check `mcast audio discover`, or pass --group",
                    a.wait
                );
            };
            if a.format.any_set() {
                eprintln!("note: --rate/--channels/--encoding ignored; using the announced format");
            }
            eprintln!("found {info}");
            if a.stream.is_none() {
                eprintln!(
                    "(use --stream NAME to choose; `mcast audio discover` lists all streams)"
                );
            }
            let spec = info.spec().expect("find_stream returns playable streams");
            eprintln!("joining {} as {spec}. Ctrl-C to stop.", info.group);
            (info.group, Some(spec), Some(info.payload_type), None)
        }
    };

    let sink = match a.wav {
        Some(path) => receiver::Sink::Wav { path },
        None => receiver::Sink::Play { device: a.device },
    };
    let cfg = receiver::ReceiverConfig {
        net: a.net.options(group)?,
        stream,
        fallback,
        payload_type,
        latency: Duration::from_secs_f64(a.latency / 1000.0),
        sink,
        duration: a.duration.map(Duration::from_secs_f64),
        stats_interval: if a.stats > 0.0 {
            Duration::from_secs_f64(a.stats)
        } else {
            Duration::MAX
        },
    };
    let stats = receiver::run(cfg, stop, |event| match event {
        receiver::Event::Stats(s) => eprintln!("{}", format_stats(s)),
        receiver::Event::NewSource {
            ssrc,
            from,
            payload_type,
            spec,
        } => {
            eprintln!("stream from {from} (ssrc {ssrc:08x}, pt {payload_type}): decoding as {spec}")
        }
        receiver::Event::UnknownPayloadType { payload_type } => eprintln!(
            "warning: payload type {payload_type} is dynamic; pass --encoding (e.g. -e opus)"
        ),
        receiver::Event::DecodeError { message } => {
            eprintln!("warning: {message}; check --encoding/--rate/--channels")
        }
    })?;
    eprintln!("done: {}", format_stats(&stats));
    Ok(())
}

fn audio_discover(a: DiscoverArgs, stop: &AtomicBool) -> Result<()> {
    let mut dir = a.sap.open(a.iface)?;
    eprintln!("listening for SAP announcements. Ctrl-C to stop.");
    let start = Instant::now();
    let limit = a.duration.map(Duration::from_secs_f64);
    while !stop.load(Ordering::Relaxed) && limit.map_or(true, |l| start.elapsed() < l) {
        for event in dir.poll(Duration::from_millis(500))? {
            let (mark, s, why) = match &event {
                DirectoryEvent::Added(s) => ("+", s, ""),
                DirectoryEvent::Changed(s) => ("~", s, ""),
                DirectoryEvent::Deleted(s) => ("-", s, " (ended)"),
                DirectoryEvent::Expired(s) => ("-", s, " (timed out)"),
            };
            let desc = match StreamInfo::from_sdp(&s.sdp) {
                Some(info) => info.to_string(),
                None => format!("\"{}\" (no audio stream)", s.sdp.name),
            };
            println!("{mark} {desc} from {}{why}", s.origin);
            if a.sdp && matches!(event, DirectoryEvent::Added(_) | DirectoryEvent::Changed(_)) {
                for line in s.sdp.to_string().lines() {
                    println!("    {line}");
                }
            }
        }
    }
    Ok(())
}

fn format_stats(s: &receiver::ReceiverStats) -> String {
    format!(
        "packets {} lost {} (concealed {}) late {} invalid {} | buffer {:.1}ms underruns {} overflows {}",
        s.packets,
        s.lost,
        s.concealed,
        s.late,
        s.invalid,
        s.buffered.as_secs_f64() * 1000.0,
        s.underruns,
        s.overflows
    )
}

fn monitor(a: MonitorArgs, stop: &AtomicBool) -> Result<()> {
    use mcast_monitor::{Monitor, MonitorConfig, Target};
    use std::io::{IsTerminal, Write};

    let watch_sap = a.groups.is_empty() || a.sap;
    let sap_groups = if a.sap_listen.sap_groups.is_empty() {
        sap::default_listen_groups()
    } else {
        a.sap_listen.sap_groups.clone()
    };
    let mut m = Monitor::start(MonitorConfig {
        targets: a.groups.iter().map(|&g| Target::new(g)).collect(),
        sap: watch_sap.then_some(sap_groups),
        interface: a.iface,
        default_spec: a.encoding.map(|c| StreamSpec::new(c, 48_000, 1)),
    })?;
    let live = !a.log && std::io::stdout().is_terminal();
    let interval = Duration::from_secs_f64(a.interval.max(0.1));
    let start = Instant::now();
    let limit = a.duration.map(Duration::from_secs_f64);
    if !live {
        eprintln!(
            "watching {}{}. Ctrl-C to stop.",
            if a.groups.is_empty() {
                String::new()
            } else {
                format!("{} group(s)", a.groups.len())
            },
            match (watch_sap, a.groups.is_empty()) {
                (true, true) => "SAP-announced streams",
                (true, false) => " and SAP-announced streams",
                _ => "",
            }
        );
    }
    let mut next = Instant::now() + interval;
    while !stop.load(Ordering::Relaxed) && limit.map_or(true, |l| start.elapsed() < l) {
        m.poll(next);
        next += interval;
        let rows = m.report();
        let groups = m.groups();
        let mut out = std::io::stdout().lock();
        if live {
            write!(out, "\x1b[H\x1b[2J")?;
            writeln!(
                out,
                "mcast monitor | {} | {} stream(s) | Ctrl-C to stop\n",
                fmt_elapsed(start.elapsed()),
                rows.len()
            )?;
            write!(out, "{}", monitor_table(&groups, &rows, true))?;
        } else {
            let t = fmt_elapsed(start.elapsed());
            for line in monitor_table(&groups, &rows, false).lines().skip(1) {
                writeln!(out, "{t}  {line}")?;
            }
        }
        out.flush()?;
    }
    let totals = m.totals();
    println!(
        "\nsummary after {} (jitter is the maximum seen):",
        fmt_elapsed(start.elapsed())
    );
    print!("{}", monitor_table(&m.groups(), &totals, false));
    Ok(())
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn level_meter(dbfs: f64) -> String {
    // 10 cells from -60 to 0 dBFS.
    let filled = (((dbfs + 60.0) / 6.0).round() as i64).clamp(0, 10) as usize;
    format!("{}{}", "█".repeat(filled), "·".repeat(10 - filled))
}

fn monitor_table(
    groups: &[mcast_monitor::GroupStatus],
    rows: &[mcast_monitor::StreamReport],
    meters: bool,
) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<30} {:<21} {:<13} {:>7} {:>8} {:>6} {:>5} {:>7} {:>6} {:>4} {:<5} LEVEL / STATUS",
        "STREAM",
        "SOURCE",
        "CODEC",
        "PKT/S",
        "KBIT/S",
        "LOSS",
        "LATE",
        "JITTER",
        "PTIME",
        "TTL",
        "DSCP",
    );
    for g in groups {
        let label = match &g.name {
            Some(n) => format!(
                "{} {}",
                truncate(n, 29 - g.group.to_string().len().min(28)),
                g.group
            ),
            None => g.group.to_string(),
        };
        let mine: Vec<_> = rows.iter().filter(|r| r.group == g.group).collect();
        if mine.is_empty() {
            let status = if let Some(e) = &g.error {
                format!("error: {e}")
            } else if g.ended {
                "ended".into()
            } else {
                "waiting for packets".into()
            };
            let _ = writeln!(s, "{label:<30} {:<21} {status}", "-");
            continue;
        }
        for (i, r) in mine.iter().enumerate() {
            let name = if i == 0 { label.clone() } else { String::new() };
            let opt =
                |v: Option<f64>, unit: &str| v.map_or("-".to_string(), |v| format!("{v:.1}{unit}"));
            let codec = match (&r.codec, r.payload_type, r.is_rtp()) {
                (_, _, false) => "not RTP".to_string(),
                (Some(c), _, _) => c.clone(),
                (None, Some(pt), _) => format!("pt {pt}"),
                (None, None, _) => "-".into(),
            };
            let dscp = r.dscp().map_or("-".into(), mcast_monitor::dscp_name);
            let status = if r.idle > Duration::from_secs(2) {
                format!("no packets for {}s", r.idle.as_secs())
            } else if g.ended {
                "ended".into()
            } else {
                match r.peak_dbfs {
                    Some(p) if p < -60.0 => "silent".into(),
                    Some(p) if meters => format!("{} {p:>5.1} dBFS", level_meter(p)),
                    Some(p) => format!(
                        "peak {p:.1} dBFS, rms {:.1}",
                        r.rms_dbfs.unwrap_or(f64::NAN)
                    ),
                    None => String::new(),
                }
            };
            let _ = writeln!(
                s,
                "{:<30} {:<21} {:<13} {:>7.1} {:>8.1} {:>5.1}% {:>5} {:>7} {:>6} {:>4} {:<5} {}",
                truncate(&name, 30),
                r.source.to_string(),
                truncate(&codec, 13),
                r.packets_per_sec,
                r.kbit_per_sec,
                r.loss_percent,
                r.late,
                opt(r.jitter_ms, "ms"),
                opt(r.ptime_ms, "ms"),
                r.ttl.map_or("-".into(), |t| t.to_string()),
                dscp,
                status
            );
        }
    }
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
