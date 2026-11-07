//! Multicast RTP audio sender.
//!
//! Pipeline: source (tone / WAV / capture) → [`Converter`] (channels and
//! sample rate) → [`Packetizer`] (codec + RTP) → socket.

use crate::codec::{Codec, OpusOptions, StreamSpec};
use crate::convert::Converter;
use crate::device;
use crate::packetizer::Packetizer;
use anyhow::{anyhow, bail, Context, Result};
use mcast_core::net::{open_sender, MulticastOptions};
use mcast_core::random_u32;
use mcast_core::sap::{self, Announcer};
use mcast_core::sdp::SessionDescription;
use std::fs::File;
use std::io::BufReader;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum Source {
    /// Sine test tone.
    Tone { frequency: f32, amplitude: f32 },
    /// WAV file, converted to the stream's rate and channels.
    Wav { path: PathBuf, looped: bool },
    /// Live capture from an input device (default device if `None`).
    Capture { device: Option<String> },
}

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub net: MulticastOptions,
    pub stream: StreamSpec,
    /// For L16/L24 with a WAV source, send at the file's own rate and channel
    /// count instead of `stream`'s.
    pub match_wav_format: bool,
    /// Audio per packet; `None` uses the codec's default (1 ms PCM, 20 ms others).
    pub ptime: Option<Duration>,
    /// RTP payload type; `None` uses the codec's static type, or 96 / 107.
    pub payload_type: Option<u8>,
    pub opus: OpusOptions,
    pub source: Source,
    /// Stop after this much audio has been sent.
    pub duration: Option<Duration>,
    /// Announce the stream with SAP so receivers can find it.
    pub announce: Option<Announce>,
}

#[derive(Debug, Clone)]
pub struct Announce {
    /// Session name shown to receivers.
    pub name: String,
    /// SAP group; `None` picks one from the stream's scope.
    pub sap_group: Option<SocketAddr>,
    pub interval: Duration,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SenderStats {
    pub packets: u64,
    pub frames: u64,
    pub send_errors: u64,
}

enum Input {
    Tone {
        step: f32,
        phase: f32,
        amplitude: f32,
    },
    Wav {
        reader: hound::WavReader<BufReader<File>>,
        looped: bool,
    },
    Capture {
        device: Option<String>,
    },
}

pub struct Sender {
    socket: UdpSocket,
    spec: StreamSpec,
    ptime: Duration,
    packetizer: Packetizer,
    converter: Converter,
    /// Rate and channels the source produces.
    source_format: (u32, u16),
    input: Input,
    duration: Option<Duration>,
    sdp: SessionDescription,
    announce: Option<(MulticastOptions, Duration)>,
}

impl Sender {
    /// Open the source and socket. Inspect `spec()` before calling `run`.
    pub fn new(cfg: SenderConfig) -> Result<Self> {
        let mut spec = cfg.stream;
        let (input, source_format) = match cfg.source {
            Source::Tone {
                frequency,
                amplitude,
            } => (
                Input::Tone {
                    step: std::f32::consts::TAU * frequency / spec.sample_rate as f32,
                    phase: 0.0,
                    amplitude,
                },
                (spec.sample_rate, spec.channels),
            ),
            Source::Wav { path, looped } => {
                let reader = hound::WavReader::open(&path)
                    .with_context(|| format!("opening {}", path.display()))?;
                let ws = reader.spec();
                if cfg.match_wav_format && spec.codec.pcm_encoding().is_some() {
                    spec = StreamSpec::new(spec.codec, ws.sample_rate, ws.channels);
                }
                (Input::Wav { reader, looped }, (ws.sample_rate, ws.channels))
            }
            Source::Capture { device } => {
                let dev = device::input_device(device.as_deref())?;
                let format = device::input_format(&dev)?;
                (Input::Capture { device }, format)
            }
        };
        if spec.channels == 0
            || spec.sample_rate == 0
            || source_format.0 == 0
            || source_format.1 == 0
        {
            bail!("invalid audio format {spec}");
        }

        let ptime = cfg.ptime.unwrap_or_else(|| spec.default_ptime());
        spec.check_ptime(ptime).map_err(|e| anyhow!(e))?;
        let frames = spec.output_format().frames_for(ptime);
        let payload_bytes = match spec.codec {
            Codec::L16 | Codec::L24 => frames * spec.output_format().frame_bytes(),
            Codec::Pcmu | Codec::Pcma => frames,
            Codec::G722 => frames / 2,
            Codec::Opus => 0, // compressed; always small
        };
        let max_payload = 1472 - 12; // fits a 1500-byte Ethernet MTU over IPv4
        if payload_bytes > max_payload {
            bail!("{payload_bytes} byte packets exceed the MTU; lower --ptime");
        }

        let pt = cfg
            .payload_type
            .unwrap_or_else(|| spec.default_payload_type());
        let encoder = spec
            .encoder(&cfg.opus)
            .map_err(|e| anyhow!("creating {} encoder: {e}", spec.codec))?;
        let socket = open_sender(&cfg.net)
            .with_context(|| format!("opening multicast sender for {}", cfg.net.group))?;
        let origin = socket.local_addr()?.ip();
        let name = cfg
            .announce
            .as_ref()
            .map_or("mcast audio", |a| a.name.as_str());
        let sdp = crate::session::describe(
            name,
            origin,
            cfg.net.group,
            cfg.net.ttl,
            spec,
            pt,
            spec.output_format().duration_of(frames),
            cfg.opus.fec,
        );
        let announce = cfg.announce.map(|a| {
            let mut net = cfg.net.clone();
            net.group = a
                .sap_group
                .unwrap_or_else(|| sap::announce_group_for(cfg.net.group.ip()));
            // Announcements are control traffic; don't mark them as voice.
            net.tos = None;
            (net, a.interval)
        });
        Ok(Self {
            socket,
            spec,
            ptime,
            packetizer: Packetizer::new(spec, encoder, frames, pt, random_u32()),
            converter: Converter::new(
                source_format.0,
                source_format.1,
                spec.sample_rate,
                spec.channels,
            ),
            source_format,
            input,
            duration: cfg.duration,
            sdp,
            announce,
        })
    }

    /// The stream as it will be sent (codec, rate, channels).
    pub fn spec(&self) -> StreamSpec {
        self.spec
    }

    pub fn ptime(&self) -> Duration {
        self.ptime
    }

    /// Rate and channels of the source, before conversion.
    pub fn source_format(&self) -> (u32, u16) {
        self.source_format
    }

    pub fn payload_type(&self) -> u8 {
        self.sdp
            .media
            .first()
            .and_then(|m| m.formats.first())
            .and_then(|f| f.parse().ok())
            .unwrap_or(0)
    }

    pub fn frames_per_packet(&self) -> usize {
        self.packetizer.frames_per_packet()
    }

    /// SDP describing this stream (usable with ffplay/VLC as a .sdp file).
    pub fn sdp(&self) -> &SessionDescription {
        &self.sdp
    }

    /// SAP group the stream will be announced on, if announcing.
    pub fn sap_group(&self) -> Option<SocketAddr> {
        self.announce.as_ref().map(|(net, _)| net.group)
    }

    /// Stream until the source ends, `duration` elapses, or `stop` is set.
    pub fn run(mut self, stop: &AtomicBool) -> Result<SenderStats> {
        // Announces until dropped at the end of `run`, then sends a SAP deletion.
        let _announcer = match &self.announce {
            Some((net, interval)) => Some(
                Announcer::start(net, &self.sdp, *interval)
                    .with_context(|| format!("starting SAP announcements on {}", net.group))?,
            ),
            None => None,
        };
        let limit = self
            .duration
            .map(|d| (d.as_secs_f64() / self.ptime.as_secs_f64()).ceil() as u64);
        let capture = matches!(self.input, Input::Capture { .. });
        let mut out = Output {
            socket: self.socket.try_clone()?,
            paced: !capture,
            start: Instant::now(),
            ptime: self.ptime,
            limit,
            stats: SenderStats::default(),
            fatal: None,
        };
        let mut converted = Vec::new();

        if let Input::Capture { device } = &self.input {
            let dev = device::input_device(device.as_deref())?;
            let (tx, rx) = mpsc::sync_channel(64);
            let _stream = device::capture(&dev, tx)?;
            while !stop.load(Ordering::Relaxed) && !out.done() {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(chunk) => {
                        converted.clear();
                        self.converter.process(&chunk, &mut converted);
                        self.packetizer
                            .push(&converted, |p| out.send(p))
                            .map_err(|e| anyhow!("encoding: {e}"))?;
                        out.check()?;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        } else {
            // Read roughly one packet of source audio at a time.
            let (rate, channels) = self.source_format;
            let chunk_frames = ((rate as f64 * self.ptime.as_secs_f64()).ceil() as usize).max(64);
            let mut buf = vec![0.0f32; chunk_frames * channels as usize];
            let mut ended = false;
            while !stop.load(Ordering::Relaxed) && !out.done() {
                let n = self.fill(&mut buf)?;
                converted.clear();
                self.converter.process(&buf[..n], &mut converted);
                if n < buf.len() {
                    self.converter.flush(&mut converted);
                    ended = true;
                }
                self.packetizer
                    .push(&converted, |p| out.send(p))
                    .map_err(|e| anyhow!("encoding: {e}"))?;
                out.check()?;
                if ended {
                    self.packetizer
                        .finish(|p| out.send(p))
                        .map_err(|e| anyhow!("encoding: {e}"))?;
                    out.check()?;
                    break;
                }
            }
        }
        let mut stats = out.stats;
        stats.frames = stats.packets * self.packetizer.frames_per_packet() as u64;
        Ok(stats)
    }

    /// Fill `out` from a paced source; returns the number of samples written
    /// (fewer than `out.len()` only at the end of the source).
    fn fill(&mut self, out: &mut [f32]) -> Result<usize> {
        let channels = self.source_format.1 as usize;
        match &mut self.input {
            Input::Tone {
                step,
                phase,
                amplitude,
            } => {
                for frame in out.chunks_exact_mut(channels) {
                    frame.fill(phase.sin() * *amplitude);
                    *phase = (*phase + *step) % std::f32::consts::TAU;
                }
                Ok(out.len())
            }
            Input::Wav { reader, looped } => {
                let spec = reader.spec();
                let mut written = 0;
                while written < out.len() {
                    let got = read_wav(reader, spec, &mut out[written..])?;
                    written += got;
                    if got == 0 {
                        if !*looped || reader.duration() == 0 {
                            break;
                        }
                        reader.seek(0)?;
                    }
                }
                Ok(written)
            }
            Input::Capture { .. } => unreachable!("capture is not a paced source"),
        }
    }
}

/// Sends packets, pacing them in real time for file and tone sources.
struct Output {
    socket: UdpSocket,
    paced: bool,
    start: Instant,
    ptime: Duration,
    limit: Option<u64>,
    stats: SenderStats,
    fatal: Option<std::io::Error>,
}

impl Output {
    fn done(&self) -> bool {
        self.fatal.is_some() || self.limit.is_some_and(|l| self.sent() >= l)
    }

    fn sent(&self) -> u64 {
        self.stats.packets + self.stats.send_errors
    }

    fn send(&mut self, pkt: &[u8]) {
        if self.done() {
            return;
        }
        if self.paced {
            let due = self.start + self.ptime.mul_f64(self.sent() as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        match self.socket.send(pkt) {
            Ok(_) => self.stats.packets += 1,
            // Transient conditions (full buffers, no route yet) shouldn't kill a stream.
            Err(e)
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted)
                    || e.raw_os_error() == Some(105) /* ENOBUFS */ =>
            {
                self.stats.send_errors += 1
            }
            Err(e) => self.fatal = Some(e),
        }
    }

    fn check(&mut self) -> Result<()> {
        match self.fatal.take() {
            Some(e) => Err(e).context("sending packet"),
            None => Ok(()),
        }
    }
}

fn read_wav(
    reader: &mut hound::WavReader<BufReader<File>>,
    spec: hound::WavSpec,
    out: &mut [f32],
) -> Result<usize> {
    let mut n = 0;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (dst, s) in out.iter_mut().zip(reader.samples::<f32>()) {
                *dst = s?;
                n += 1;
            }
        }
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            for (dst, s) in out.iter_mut().zip(reader.samples::<i32>()) {
                *dst = s? as f32 / scale;
                n += 1;
            }
        }
    }
    Ok(n)
}
