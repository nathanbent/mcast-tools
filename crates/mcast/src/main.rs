use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use mcast_audio::{receiver, sender, AudioFormat, Encoding, StreamInfo};
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

#[derive(Subcommand)]
enum Command {
    /// Uncompressed PCM audio over RTP multicast
    #[command(subcommand)]
    Audio(AudioCommand),
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
    /// Sample encoding: l16 or l24 [default: l16]
    #[arg(short, long)]
    encoding: Option<Encoding>,
}

impl FormatArgs {
    fn format(&self) -> AudioFormat {
        let d = AudioFormat::default();
        AudioFormat {
            sample_rate: self.rate.unwrap_or(d.sample_rate),
            channels: self.channels.unwrap_or(d.channels),
            encoding: self.encoding.unwrap_or(d.encoding),
        }
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
    /// Packet time in milliseconds (AES67 uses 1)
    #[arg(long, default_value_t = 1.0)]
    ptime: f64,
    /// RTP payload type (default: RFC 3551 static type, else 96)
    #[arg(long)]
    pt: Option<u8>,
    /// Send a sine tone at this frequency (Hz) [default source]
    #[arg(long, group = "source")]
    tone: Option<f32>,
    /// Tone level in dBFS
    #[arg(long, default_value_t = -12.0, allow_negative_numbers = true)]
    level: f32,
    /// Send a WAV file (its rate and channel count are used)
    #[arg(long, group = "source")]
    wav: Option<PathBuf>,
    /// Loop the WAV file
    #[arg(long = "loop", requires = "wav")]
    looped: bool,
    /// Capture from an input device (substring of its name, or "default")
    #[arg(long, group = "source", num_args = 0..=1, default_missing_value = "default")]
    capture: Option<String>,
    /// Stop after this many seconds
    #[arg(short, long)]
    duration: Option<f64>,
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
    /// Multicast group and port; skips SAP discovery and uses the format flags
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
    let source = if let Some(path) = a.wav {
        sender::Source::Wav {
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
    let announce = (!a.no_sap).then(|| sender::Announce {
        name: a
            .name
            .clone()
            .unwrap_or_else(|| format!("{} audio", hostname())),
        sap_group: a.sap_group,
        interval: Duration::from_secs_f64(a.sap_interval.max(0.5)),
    });
    let s = sender::Sender::new(sender::SenderConfig {
        net: a.net.options(a.group)?,
        format: a.format.format(),
        ptime: Duration::from_secs_f64(a.ptime / 1000.0),
        payload_type: a.pt,
        source,
        duration: a.duration.map(Duration::from_secs_f64),
        announce,
    })?;
    if let Some(path) = &a.sdp {
        std::fs::write(path, s.sdp().to_string())?;
        eprintln!("wrote SDP to {}", path.display());
    }
    let fmt = s.format();
    eprintln!(
        "sending {fmt} to {} ({} frames/packet). Ctrl-C to stop.",
        a.group,
        s.frames_per_packet()
    );
    match s.sap_group() {
        Some(g) => eprintln!(
            "announcing \"{}\" via SAP on {g}; receive with: mcast audio recv",
            s.sdp().name
        ),
        None => eprintln!(
            "receive with: mcast audio recv -g {} -r {} -c {} -e {}",
            a.group,
            fmt.sample_rate,
            fmt.channels,
            fmt.encoding.to_string().to_lowercase()
        ),
    }
    let stats = s.run(stop)?;
    eprintln!(
        "sent {} packets ({:.1}s of audio), {} send errors",
        stats.packets,
        stats.frames as f64 / fmt.sample_rate as f64,
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
            if info.format().is_none() {
                eprintln!("  skipping {info}");
                continue;
            }
            return Ok(Some(info));
        }
    }
    Ok(None)
}

fn audio_recv(a: RecvArgs, stop: &AtomicBool) -> Result<()> {
    let (group, fmt) = match a.group {
        Some(g) => (g, a.format.format()),
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
            (info.group, info.format().expect("checked playable"))
        }
    };

    let sink = match a.wav {
        Some(path) => receiver::Sink::Wav { path },
        None => receiver::Sink::Play { device: a.device },
    };
    eprintln!("joining {group} expecting {fmt}. Ctrl-C to stop.");
    let cfg = receiver::ReceiverConfig {
        net: a.net.options(group)?,
        format: fmt,
        latency: Duration::from_secs_f64(a.latency / 1000.0),
        sink,
        duration: a.duration.map(Duration::from_secs_f64),
        stats_interval: if a.stats > 0.0 {
            Duration::from_secs_f64(a.stats)
        } else {
            Duration::MAX
        },
    };
    let stats = receiver::run(cfg, stop, |event| {
        match event {
        receiver::Event::Stats(s) => eprintln!("{}", format_stats(s)),
        receiver::Event::NewSource { ssrc, from, payload_type } => {
            eprintln!("stream from {from} (ssrc {ssrc:08x}, pt {payload_type})")
        }
        receiver::Event::FormatMismatch { payload_len } => eprintln!(
            "warning: {payload_len}-byte payload doesn't fit {fmt}; check --rate/--channels/--encoding"
        ),
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
        "packets {} lost {} late {} invalid {} | buffer {:.1}ms underruns {} overflows {}",
        s.packets,
        s.lost,
        s.late,
        s.invalid,
        s.buffered.as_secs_f64() * 1000.0,
        s.underruns,
        s.overflows
    )
}
