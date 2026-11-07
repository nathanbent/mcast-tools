use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use mcast_audio::{receiver, sender, AudioFormat, Encoding};
use mcast_core::{Interface, MulticastOptions};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    /// Join a multicast group and play (or record) the stream
    Recv(RecvArgs),
    /// List audio input and output devices
    Devices,
}

#[derive(Args)]
struct NetArgs {
    /// Multicast group and port, e.g. 239.69.0.1:5004 or [ff15::69]:5004
    #[arg(short, long, default_value = "239.69.0.1:5004")]
    group: SocketAddr,
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
    fn options(&self) -> Result<MulticastOptions> {
        let mut o = MulticastOptions::new(self.group)?;
        o.interface = self.iface;
        o.ttl = self.ttl;
        o.loopback = !self.no_loopback;
        Ok(o)
    }
}

#[derive(Args)]
struct FormatArgs {
    /// Sample rate in Hz
    #[arg(short, long, default_value_t = 48_000)]
    rate: u32,
    /// Channel count
    #[arg(short, long, default_value_t = 2)]
    channels: u16,
    /// Sample encoding: l16 or l24
    #[arg(short, long, default_value = "l16")]
    encoding: Encoding,
}

impl FormatArgs {
    fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.rate,
            channels: self.channels,
            encoding: self.encoding,
        }
    }
}

#[derive(Args)]
struct SendArgs {
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
}

#[derive(Args)]
struct RecvArgs {
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
        Command::Audio(AudioCommand::Devices) => {
            for line in mcast_audio::device::list()? {
                println!("{line}");
            }
            Ok(())
        }
    }
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
    let s = sender::Sender::new(sender::SenderConfig {
        net: a.net.options()?,
        format: a.format.format(),
        ptime: Duration::from_secs_f64(a.ptime / 1000.0),
        payload_type: a.pt,
        source,
        duration: a.duration.map(Duration::from_secs_f64),
    })?;
    let fmt = s.format();
    eprintln!(
        "sending {fmt} to {} ({} frames/packet). Ctrl-C to stop.",
        a.net.group,
        s.frames_per_packet()
    );
    eprintln!(
        "receive with: mcast audio recv -g {} -r {} -c {} -e {}",
        a.net.group,
        fmt.sample_rate,
        fmt.channels,
        fmt.encoding.to_string().to_lowercase()
    );
    let stats = s.run(stop)?;
    eprintln!(
        "sent {} packets ({:.1}s of audio), {} send errors",
        stats.packets,
        stats.frames as f64 / fmt.sample_rate as f64,
        stats.send_errors
    );
    Ok(())
}

fn audio_recv(a: RecvArgs, stop: &AtomicBool) -> Result<()> {
    let sink = match a.wav {
        Some(path) => receiver::Sink::Wav { path },
        None => receiver::Sink::Play { device: a.device },
    };
    let fmt = a.format.format();
    eprintln!("joining {} expecting {fmt}. Ctrl-C to stop.", a.net.group);
    let cfg = receiver::ReceiverConfig {
        net: a.net.options()?,
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
