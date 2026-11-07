use anyhow::{anyhow, bail, Result};
use clap::{Args, Parser, Subcommand};
use mcast_audio::codec::{OpusBandwidth, OpusMode};
use mcast_audio::process::Mode;
use mcast_audio::{receiver, sender, Codec, StreamInfo, StreamSpec};
use mcast_control::request::{ChimeRequest, SinkRequest, SourceRequest, DEFAULT_GROUP};
use mcast_control::{describe, RecvRequest, SendRequest};
use mcast_core::sap::{self, Directory, DirectoryEvent};
use mcast_core::Interface;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod presets;
mod update;

#[derive(Parser)]
#[command(name = "mcast", version = env!("MCAST_VERSION"), about = "Multicast tools")]
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
    /// Save options for `audio send`, `recv`, `discover`, `monitor` or `update` under a name; run with `mcast @NAME`
    #[command(subcommand)]
    Preset(PresetCommand),
    /// Update mcast: download the latest release, or rebuild it with cargo
    Update(update::UpdateArgs),
    /// Run the web GUI and HTTP/JSON API for sending, receiving, discovery and monitoring
    Serve(ServeArgs),
    /// Terminal front end for a running `mcast serve` (works over SSH)
    Tui(TuiArgs),
}

#[derive(Subcommand)]
#[command(after_help = "Examples:
  mcast preset create page-1 -g 239.192.2.1:10000 -e g711u --tos 184 --no-sap
  mcast preset set page-1 -p -C -L 800          # add or change options
  mcast preset unset page-1 chime               # remove one (by name or flag)
  mcast @page-1 --file page.wav                 # run it; options given here win
  mcast audio send -P page-1 --file page.wav    # same
  mcast preset create lobby --recv -s lobby -v -6
  mcast preset create zones --monitor 239.192.2.1:10000 239.192.2.2:10000 --sap
  mcast @zones --log                            # groups given here replace the preset's
  mcast preset create sap2 --discover --sap-group 239.195.255.255:9875 --sdp
  mcast preset create dev --update -b dev --path ~/src/mcast-tools

Presets are kept in ~/.config/mcast/presets.conf ($MCAST_PRESETS overrides),
one option per line; the file can be edited by hand.")]
enum PresetCommand {
    /// Create a preset for `audio send` (or `audio recv` with --recv, `audio
    /// discover` with --discover, `monitor` with --monitor, `update` with
    /// --update), with any options
    Create {
        name: String,
        /// The preset is for `mcast audio recv` (put this before the options)
        #[arg(long)]
        recv: bool,
        /// The preset is for `mcast audio discover` (put this before the options)
        #[arg(long, conflicts_with = "recv")]
        discover: bool,
        /// The preset is for `mcast monitor` (put this before the options)
        #[arg(long, conflicts_with_all = ["recv", "discover"])]
        monitor: bool,
        /// The preset is for `mcast update` (put this before the options)
        #[arg(long, conflicts_with_all = ["recv", "discover", "monitor"])]
        update: bool,
        /// Options, exactly as for `mcast audio send` (or recv, discover, monitor, update)
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "OPTIONS"
        )]
        options: Vec<OsString>,
    },
    /// Add or change options (creating a send preset if needed); they replace
    /// the same option, or ones they conflict with, e.g. --file replaces --tone
    Set {
        name: String,
        /// Options, exactly as for `mcast audio send` (or recv, discover, monitor, update); see their --help
        #[arg(
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "OPTIONS"
        )]
        options: Vec<OsString>,
    },
    /// Remove options, named like `group`, `--group` or `-g` (`groups` for monitor's groups)
    Unset {
        name: String,
        #[arg(required = true, allow_hyphen_values = true, value_name = "OPTION")]
        options: Vec<String>,
    },
    /// Run a preset; extra options are added and override it. Shortcut: mcast @NAME
    Run {
        name: String,
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "OPTIONS"
        )]
        options: Vec<OsString>,
    },
    /// List presets, or show one, as the commands they run
    #[command(visible_aliases = ["ls", "show"])]
    List { name: Option<String> },
    /// Copy a preset to a new name
    #[command(visible_alias = "cp")]
    Copy { from: String, to: String },
    /// Delete a preset
    #[command(visible_alias = "rm")]
    Delete { name: String },
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
    /// Start from a saved preset (`mcast preset`); options given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
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
    /// Processing tuned for the content and codec: voice, music or none [default: none]
    #[arg(short, long)]
    mode: Option<Mode>,
    /// Shortcut for --mode voice
    #[arg(short = 'p', long = "voice")]
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
    /// Start from a saved preset (`mcast preset`); options given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
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
    /// Volume change in dB, e.g. -6 or 3 (boosts are soft-limited to avoid clipping)
    #[arg(short, long, allow_negative_numbers = true)]
    volume: Option<f64>,
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
    /// Start from a saved preset (`mcast preset`); options and groups given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
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
    /// Start from a saved preset (`mcast preset`); options given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
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

#[derive(Args)]
struct ServeArgs {
    /// Address to listen on. Anything but loopback needs --token
    #[arg(short, long, default_value = "127.0.0.1:8650")]
    listen: SocketAddr,
    /// Bearer token clients must send (Authorization: Bearer TOKEN)
    #[arg(long, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Directory for uploaded audio [default: ~/.local/share/mcast/media]
    #[arg(long, value_name = "DIR")]
    media_dir: Option<PathBuf>,
    /// Largest upload in MB
    #[arg(long, default_value_t = 200)]
    max_upload_mb: u64,
    /// Interface for jobs, discovery and monitoring: IPv4 address or interface index
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    #[command(flatten)]
    sap: SapListenArgs,
    /// Also monitor these groups (ADDR:PORT), besides SAP-announced streams
    #[arg(long = "watch", value_name = "ADDR:PORT")]
    watch: Vec<SocketAddr>,
    /// Don't run the stream monitor
    #[arg(long, conflicts_with = "watch")]
    no_monitor: bool,
    /// API only: don't serve the web GUI
    #[arg(long)]
    no_web: bool,
    /// Serve the web GUI from this directory instead of the built-in copy
    /// (for working on crates/mcast-web/assets)
    #[arg(long, value_name = "DIR", conflicts_with = "no_web")]
    web_dir: Option<PathBuf>,
}

#[derive(Args)]
struct TuiArgs {
    /// The server to control
    #[arg(
        short,
        long,
        default_value = "http://127.0.0.1:8650",
        env = "MCAST_URL"
    )]
    url: String,
    /// Its API token
    #[arg(long, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse_from(presets::expand(std::env::args_os().collect())?);
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
        Command::Preset(c) => preset(c),
        Command::Update(a) => update::run(a),
        Command::Serve(a) => serve(a, stop),
        Command::Tui(a) => mcast_tui::run(&a.url, a.token),
        Command::Audio(AudioCommand::Devices) => {
            for line in mcast_audio::device::list()? {
                println!("{line}");
            }
            Ok(())
        }
    }
}

fn preset(c: PresetCommand) -> Result<()> {
    let mut store = presets::Store::load()?;
    match c {
        PresetCommand::Create {
            name,
            recv,
            discover,
            monitor,
            update,
            options,
        } => {
            let kind = if recv {
                presets::Kind::Recv
            } else if discover {
                presets::Kind::Discover
            } else if monitor {
                presets::Kind::Monitor
            } else if update {
                presets::Kind::Update
            } else {
                presets::Kind::Send
            };
            let p = store.create(&name, kind, &options)?;
            println!("created {}: {}", p.name, p.command_line());
        }
        PresetCommand::Set { name, options } => {
            let (p, created) = store.set(&name, &options)?;
            let verb = if created { "created" } else { "updated" };
            println!("{verb} {}: {}", p.name, p.command_line());
        }
        PresetCommand::Unset { name, options } => {
            let p = store.unset(&name, &options)?;
            println!("updated {}: {}", p.name, p.command_line());
        }
        PresetCommand::Run { name, .. } => {
            // `presets::expand` rewrites `preset run NAME` before parsing.
            bail!("can't run preset \"{name}\"");
        }
        PresetCommand::List { name: Some(name) } => {
            println!("{}", store.get(&name)?.command_line());
            return Ok(());
        }
        PresetCommand::List { name: None } => {
            if store.presets.is_empty() {
                eprintln!(
                    "no presets in {}; create one with `mcast preset create NAME [OPTIONS]`",
                    store.path.display()
                );
            }
            let width = store
                .presets
                .iter()
                .map(|p| p.name.len())
                .max()
                .unwrap_or(0);
            for p in &store.presets {
                println!("{:<width$}  {}", p.name, p.command_line());
            }
            return Ok(());
        }
        PresetCommand::Copy { from, to } => {
            let p = store.copy(&from, &to)?;
            println!("created {}: {}", p.name, p.command_line());
        }
        PresetCommand::Delete { name } => {
            store.delete(&name)?;
            println!("deleted {name}");
        }
    }
    store.save()
}

fn send_request(a: &SendArgs) -> Result<SendRequest> {
    // -p is shorthand for -m voice; saying both is fine, contradicting it isn't.
    let mode = match (a.voice, a.mode) {
        (true, Some(m)) if m != Mode::Voice => {
            bail!("-p means --mode voice, but --mode {m} was also given")
        }
        (true, _) => Mode::Voice,
        (false, m) => m.unwrap_or_default(),
    };
    let source = if let Some(path) = &a.wav {
        SourceRequest::File {
            path: path.clone(),
            looped: a.looped,
        }
    } else if let Some(dev) = &a.capture {
        SourceRequest::Capture {
            device: Some(dev.clone()).filter(|d| d != "default"),
        }
    } else {
        SourceRequest::Tone {
            frequency: a.tone.unwrap_or(440.0),
            level_dbfs: a.level,
        }
    };
    Ok(SendRequest {
        group: a.group,
        interface: Some(a.net.iface),
        ttl: a.net.ttl,
        loopback: !a.net.no_loopback,
        encoding: a.format.encoding,
        rate: a.format.rate,
        channels: a.format.channels,
        tos: a.tos,
        ptime_ms: a.ptime,
        payload_type: a.pt,
        opus_bitrate_kbps: a.bitrate,
        opus_mode: a.opus_mode,
        opus_bandwidth: a.opus_bandwidth,
        opus_fec: !a.no_fec,
        source,
        mode: Some(mode),
        volume_db: a.volume,
        lead_in_ms: a.lead_in,
        tail_ms: a.tail,
        chime: a.chime.as_deref().map(|c| match c {
            "default" => ChimeRequest::Default,
            file => ChimeRequest::File(file.into()),
        }),
        chime_gap_ms: a.chime_gap,
        start_s: a.start,
        duration_s: a.duration,
        render: a.render.clone(),
        name: a.name.clone(),
        sap: !a.no_sap,
        sap_group: a.sap_group,
        sap_interval_s: a.sap_interval,
    })
}

fn audio_send(a: SendArgs, stop: &AtomicBool) -> Result<()> {
    let req = send_request(&a)?;
    if a.render.is_some() && req.endless() {
        bail!("--render needs an end: a file without --loop, or --duration");
    }
    let s = sender::Sender::new(req.to_config(Interface::Any)?)?;
    if let Some(path) = &a.sdp {
        std::fs::write(path, s.sdp().to_string())?;
        eprintln!("wrote SDP to {}", path.display());
    }
    let spec = s.spec();
    eprintln!("{}", describe::send_summary(&s, &req));
    if let Some(note) = describe::loudness_note(&s) {
        eprintln!("{note}");
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
    if stats.capture_dropped > 0 {
        eprintln!(
            "warning: dropped {} captured samples (processing fell behind the input device)",
            stats.capture_dropped
        );
    }
    Ok(())
}

fn audio_recv(a: RecvArgs, stop: &AtomicBool) -> Result<()> {
    let req = RecvRequest {
        group: a.group,
        stream: a.stream.clone(),
        wait_s: a.wait,
        sap_groups: a.sap.sap_groups.clone(),
        interface: Some(a.net.iface),
        ttl: a.net.ttl,
        loopback: !a.net.no_loopback,
        encoding: a.format.encoding,
        rate: a.format.rate,
        channels: a.format.channels,
        latency_ms: a.latency,
        volume_db: a.volume,
        sink: match a.wav {
            Some(path) => SinkRequest::Wav { path },
            None => SinkRequest::Play { device: a.device },
        },
        duration_s: a.duration,
        stats_interval_s: a.stats,
    };
    let resolved = match req.direct() {
        Some(r) => {
            match (r.stream, r.fallback) {
                (Some(spec), _) => {
                    eprintln!("joining {} expecting {spec}. Ctrl-C to stop.", r.group)
                }
                (None, Some(fallback)) => eprintln!(
                    "joining {}; codec from payload type (G.711/G.722), else {fallback}. Ctrl-C to stop.",
                    r.group
                ),
                (None, None) => eprintln!("joining {}. Ctrl-C to stop.", r.group),
            }
            r
        }
        None => {
            match &a.stream {
                Some(n) => eprintln!("looking for a stream named \"{n}\" via SAP..."),
                None => eprintln!("looking for streams via SAP..."),
            }
            let found = req
                .resolve(Interface::Any, stop, |info| eprintln!("  skipping {info}"))
                .map_err(|e| anyhow!("{e:#}; check `mcast audio discover`, or pass --group"))?;
            let Some(r) = found else {
                return Ok(());
            };
            if req.format().any_set() {
                eprintln!("note: --rate/--channels/--encoding ignored; using the announced format");
            }
            if let Some(info) = &r.announced {
                eprintln!("found {info}");
            }
            if a.stream.is_none() {
                eprintln!(
                    "(use --stream NAME to choose; `mcast audio discover` lists all streams)"
                );
            }
            let spec = r.stream.expect("announced streams have a spec");
            eprintln!("joining {} as {spec}. Ctrl-C to stop.", r.group);
            r
        }
    };
    let cfg = req.to_config(&resolved, Interface::Any)?;
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

fn serve(a: ServeArgs, stop: Arc<AtomicBool>) -> Result<()> {
    let media_dir = a.media_dir.unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mcast/media")
    });
    let cfg = mcast_server::ServerConfig {
        listen: a.listen,
        token: a.token,
        media_dir,
        interface: a.iface,
        sap_groups: if a.sap.sap_groups.is_empty() {
            sap::default_listen_groups()
        } else {
            a.sap.sap_groups
        },
        monitor: (!a.no_monitor).then_some(a.watch),
        max_upload: a.max_upload_mb.saturating_mul(1 << 20),
    };
    let web = if a.no_web {
        axum::Router::new()
    } else {
        mcast_web::router(a.web_dir)
    };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(mcast_server::serve(cfg, web, async move {
        // SIGTERM (systemd, docker stop) or Ctrl-C, which the ctrlc handler turns into `stop`.
        let sigterm = async {
            #[cfg(unix)]
            if let Ok(mut t) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                t.recv().await;
                return;
            }
            std::future::pending::<()>().await
        };
        let ctrl_c = async {
            while !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        };
        tokio::select! {
            _ = sigterm => {}
            _ = ctrl_c => {}
        }
    }))
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
