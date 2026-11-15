//! `mcast audio`: send, receive and discover audio streams.

use crate::monitor::ms;
use crate::{chimes, FormatArgs, NetArgs, RtcpArgs, SapListenArgs};
use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use mcast_audio::codec::{OpusBandwidth, OpusMode};
use mcast_audio::process::Mode;
use mcast_audio::{receiver, sender, Codec, StreamInfo};
use mcast_control::request::{ChimeRequest, SinkRequest, SourceRequest, DEFAULT_GROUP};
use mcast_control::{describe, RecvRequest, SendRequest};
use mcast_core::sap::DirectoryEvent;
use mcast_core::Interface;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(crate) fn run(cmd: AudioCommand, stop: &AtomicBool) -> Result<()> {
    match cmd {
        AudioCommand::Send(a) => audio_send(a, stop),
        AudioCommand::Recv(a) => audio_recv(a, stop),
        AudioCommand::Discover(a) => audio_discover(a, stop),
        AudioCommand::Chimes { command } => chimes::run(command),
        AudioCommand::Devices => {
            for line in mcast_audio::device::list()? {
                println!("{line}");
            }
            Ok(())
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum AudioCommand {
    /// Stream a test tone, WAV file, or live input to a multicast group
    Send(SendArgs),
    /// Play (or record) a stream; finds one via SAP unless --group is given
    Recv(RecvArgs),
    /// List streams announced with SAP (audio, text and files)
    Discover(DiscoverArgs),
    /// List audio input and output devices
    Devices,
    /// The built-in chimes: list them, hide ones you don't want from the lists, save one as WAV
    Chimes {
        #[command(subcommand)]
        command: Option<chimes::ChimesCommand>,
    },
}

#[derive(Args)]
pub(crate) struct SendArgs {
    /// Start from a saved preset (`mcast preset`); options given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
    /// Multicast group and port, e.g. 239.69.0.1:5004 or [ff15::69]:5004, or a phonebook name (`mcast book`)
    #[arg(short, long, default_value = DEFAULT_GROUP, value_parser = crate::book::parse_group)]
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
    /// Send a sine tone at this frequency (Hz), or "pad" for a soft, slowly
    /// changing chord [default source: 440]
    #[arg(long, group = "source", value_name = "HZ|pad", value_parser = parse_tone)]
    tone: Option<Tone>,
    /// Tone level in dBFS (for pad, its peak)
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
    /// Send just a chime, no audio: the -C chime (default rising) and/or the
    /// --end-chime
    #[arg(long, group = "source")]
    chime_only: bool,
    /// Processing tuned for the content and codec: voice, music or none [default: none]
    #[arg(short, long)]
    mode: Option<Mode>,
    /// Shortcut for --mode voice
    #[arg(short = 'p', long = "voice")]
    voice: bool,
    /// Volume change in dB after processing, e.g. -6 or 3 (limited to avoid clipping)
    #[arg(short, long, allow_negative_numbers = true)]
    volume: Option<f64>,
    /// Gain in dB on the input before processing (not the chimes), e.g. 12 for a quiet microphone
    #[arg(long, allow_negative_numbers = true)]
    input_gain: Option<f64>,
    /// Normalise loudness even with --mode none (voice and music always do)
    #[arg(long)]
    normalize: bool,
    /// Lead-in silence in ms, so the first word isn't clipped
    #[arg(short = 'L', long, default_value_t = 0.0)]
    lead_in: f64,
    /// Silence after the audio in ms, so the end isn't cut off
    #[arg(short = 'E', long, default_value_t = 300.0)]
    tail: f64,
    /// Chime before the audio: a built-in (see `mcast audio chimes`: rising,
    /// ding-dong, westminster, church-bell, beep, siren-wail, ...), an audio
    /// file, or no value for the default (rising, or $MCAST_CHIME if set)
    #[arg(short = 'C', long, value_name = "NAME|FILE", num_args = 0..=1, default_missing_value = "default")]
    chime: Option<String>,
    /// Gap between the chime and the audio, in ms
    #[arg(short = 'G', long, default_value_t = 400.0, requires = "chime")]
    chime_gap: f64,
    /// Chime after the audio, to mark the end of the page: a built-in, an
    /// audio file, or no value for the default (falling, or $MCAST_END_CHIME)
    #[arg(long, value_name = "NAME|FILE", num_args = 0..=1, default_missing_value = "default")]
    end_chime: Option<String>,
    /// Gap between the audio and the end chime, in ms
    #[arg(long, default_value_t = 400.0, requires = "end_chime")]
    end_chime_gap: f64,
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
    #[command(flatten)]
    rtcp: RtcpArgs,
}

/// `--tone` value: a sine frequency or the pad.
#[derive(Clone, Copy, Debug)]
enum Tone {
    Sine(f32),
    Pad,
}

fn parse_tone(s: &str) -> Result<Tone, String> {
    if s.eq_ignore_ascii_case("pad") {
        return Ok(Tone::Pad);
    }
    match s.parse::<f32>() {
        Ok(f) if f > 0.0 && f.is_finite() => Ok(Tone::Sine(f)),
        _ => Err(format!(
            "expected a frequency in Hz or \"pad\", got \"{s}\""
        )),
    }
}

#[derive(Args)]
pub(crate) struct RecvArgs {
    /// Start from a saved preset (`mcast preset`); options given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
    /// Multicast group and port; skips SAP discovery. The codec comes from
    /// --encoding, or from the payload type for G.711/G.722 (else L16/48000/2).
    /// ADDR:PORT or a phonebook name
    #[arg(short, long, value_parser = crate::book::parse_group)]
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
    /// Name to report ourselves as over RTCP, e.g. the room [default: hostname]
    #[arg(short, long)]
    name: Option<String>,
    #[command(flatten)]
    rtcp: RtcpArgs,
}

#[derive(Args)]
pub(crate) struct DiscoverArgs {
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
    } else if a.chime_only {
        SourceRequest::Chime
    } else if let Some(dev) = &a.capture {
        SourceRequest::Capture {
            device: Some(dev.clone()).filter(|d| d != "default"),
        }
    } else {
        match a.tone.unwrap_or(Tone::Sine(440.0)) {
            Tone::Sine(frequency) => SourceRequest::Tone {
                frequency,
                level_dbfs: a.level,
            },
            Tone::Pad => SourceRequest::Pad {
                level_dbfs: a.level,
            },
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
        input_gain_db: a.input_gain,
        normalize: a.normalize,
        lead_in_ms: a.lead_in,
        tail_ms: a.tail,
        chime: a.chime.as_deref().map(ChimeRequest::parse_arg),
        chime_gap_ms: a.chime_gap,
        end_chime: a.end_chime.as_deref().map(ChimeRequest::parse_arg),
        end_chime_gap_ms: a.end_chime_gap,
        start_s: a.start,
        duration_s: a.duration,
        render: a.render.clone(),
        name: a.name.clone(),
        sap: !a.no_sap,
        sap_group: a.sap_group,
        sap_interval_s: a.sap_interval,
        rtcp: !a.rtcp.no_rtcp,
        rtcp_interval_s: a.rtcp.rtcp_interval,
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
    if let Some(g) = s.rtcp_group() {
        eprintln!("RTCP sender reports on {g} (--no-rtcp to turn off)");
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
        rtcp: !a.rtcp.no_rtcp,
        rtcp_interval_s: a.rtcp.rtcp_interval,
        name: a.name.clone(),
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
        receiver::Event::SenderInfo { ssrc, cname, name } => {
            let who = match (name, cname) {
                (Some(n), Some(c)) => format!("\"{n}\" ({c})"),
                (Some(n), None) => format!("\"{n}\""),
                (None, Some(c)) => c,
                (None, None) => "sender".into(),
            };
            eprintln!("rtcp: ssrc {ssrc:08x} is {who}")
        }
        receiver::Event::Bye { ssrc, reason } => match reason {
            Some(r) => eprintln!("rtcp: ssrc {ssrc:08x} ended the stream: {r}"),
            None => eprintln!("rtcp: ssrc {ssrc:08x} ended the stream"),
        },
        receiver::Event::Warning { message } => eprintln!("warning: {message}"),
    })?;
    eprintln!("done: {}", format_stats(&stats));
    Ok(())
}

fn audio_discover(a: DiscoverArgs, stop: &AtomicBool) -> Result<()> {
    let mut dir = a.sap.open(a.iface)?;
    eprintln!("listening for SAP announcements. Ctrl-C to stop.");
    let start = Instant::now();
    let limit = a.duration.map(Duration::from_secs_f64);
    while !stop.load(Ordering::Relaxed) && limit.is_none_or(|l| start.elapsed() < l) {
        for event in dir.poll(Duration::from_millis(500))? {
            let (mark, s, why) = match &event {
                DirectoryEvent::Added(s) => ("+", s, ""),
                DirectoryEvent::Changed(s) => ("~", s, ""),
                DirectoryEvent::Deleted(s) => ("-", s, " (ended)"),
                DirectoryEvent::Expired(s) => ("-", s, " (timed out)"),
            };
            let desc = if let Some(info) = StreamInfo::from_sdp(&s.sdp) {
                info.to_string()
            } else if let Some(text) = mcast_text::TextInfo::from_sdp(&s.sdp) {
                text.to_string()
            } else if let Some(file) = mcast_file::FileInfo::from_sdp(&s.sdp) {
                file.to_string()
            } else {
                format!("\"{}\" (no stream we know)", s.sdp.name)
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
    let mut line = format!(
        "packets {} lost {} (concealed {}) reordered {} late {} invalid {} | buffer {:.1}ms underruns {} overflows {}",
        s.packets,
        s.lost,
        s.concealed,
        s.reordered,
        s.late,
        s.invalid,
        s.buffered.as_secs_f64() * 1000.0,
        s.underruns,
        s.overflows
    );
    if let Some(ppm) = s.drift_ppm {
        line += &format!(" drift {ppm:+.0}ppm");
    }
    if let Some(t) = s.transit_secs {
        let (t, b) = (t * 1000.0, s.buffered.as_secs_f64() * 1000.0);
        line += &format!(" | transit {}, playing {} behind sender", ms(t), ms(t + b));
    }
    line
}
