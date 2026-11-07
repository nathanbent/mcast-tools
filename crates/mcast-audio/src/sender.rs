//! Multicast RTP audio sender.
//!
//! Pipeline: [lead-in, chime + gap] then source (tone / WAV / capture) →
//! fades → channel mapping →
//! [`Processor`] (voice/music processing, at the source rate) →
//! [`Converter`] (sample rate) → [`Packetizer`] (codec + RTP) → socket,
//! or, when rendering, back through the codec's decoder into a WAV file.

use crate::chime::Chime;
use crate::codec::{Codec, Decoder, OpusOptions, StreamSpec};
use crate::convert::{map_channels, Converter};
use crate::device;
use crate::file::AudioFile;
use crate::packetizer::Packetizer;
use crate::pad::Pad;
use crate::process::{Band, Loudness, Mode, ProcessOptions, Processor};
use anyhow::{anyhow, bail, Context, Result};
use mcast_core::net::{open_sender, MulticastOptions};
use mcast_core::random_u32;
use mcast_core::rtcp::{RtcpConfig, SenderRtcp};
use mcast_core::rtp;
use mcast_core::sap::{self, Announcer};
use mcast_core::sdp::SessionDescription;
use std::fs::File;
use std::io::BufWriter;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FADE_IN: Duration = Duration::from_millis(50);
const FADE_OUT: Duration = Duration::from_millis(300);

#[derive(Debug, Clone)]
pub enum Source {
    /// Sine test tone.
    Tone { frequency: f32, amplitude: f32 },
    /// Soft chord progression (see `pad`); `amplitude` is its peak.
    Pad { amplitude: f32 },
    /// Audio file (WAV, MP3, FLAC, Ogg Vorbis, AAC/M4A, ...), converted to
    /// the stream's rate and channels.
    File { path: PathBuf, looped: bool },
    /// Live capture from an input device (default device if `None`).
    Capture { device: Option<String> },
}

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub net: MulticastOptions,
    pub stream: StreamSpec,
    /// For L16/L24 with a file source, send at the file's own rate and channel
    /// count instead of `stream`'s.
    pub match_file_format: bool,
    /// Audio per packet; `None` uses the codec's default (1 ms PCM, 20 ms others).
    pub ptime: Option<Duration>,
    /// RTP payload type; `None` uses the codec's static type, or 96 / 107.
    pub payload_type: Option<u8>,
    pub opus: OpusOptions,
    pub process: ProcessOptions,
    pub source: Source,
    /// Start this far into a file (with a short fade-in).
    pub start: Option<Duration>,
    /// Send this much of the source (with a short fade-out), then stop.
    pub duration: Option<Duration>,
    /// Silence before the audio, so the first word isn't clipped.
    pub lead_in: Duration,
    /// Pre-tone played after the lead-in and before the source.
    pub chime: Option<Chime>,
    /// Silence after file and tone sources, so the last word isn't cut off.
    pub tail: Duration,
    /// Write what a receiver would decode to this WAV file instead of sending.
    pub render: Option<PathBuf>,
    /// Announce the stream with SAP so receivers can find it.
    pub announce: Option<Announce>,
    /// Send RTCP sender reports (and BYE at the end) on the RTP port + 1.
    pub rtcp: Option<RtcpConfig>,
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
    /// Captured samples dropped because processing fell behind the device.
    pub capture_dropped: u64,
}

/// Live counters for a running sender, readable from another thread.
#[derive(Debug, Default)]
pub struct Progress {
    pub packets: AtomicU64,
    pub send_errors: AtomicU64,
}

/// Result of measuring a file's loudness before sending.
#[derive(Debug, Clone, Copy)]
pub struct LoudnessInfo {
    /// Integrated loudness after processing, before normalisation (LUFS).
    pub measured: Option<f64>,
    /// Gain applied to reach the mode's target.
    pub gain_db: f64,
    pub target: f64,
}

enum Input {
    Tone {
        step: f32,
        phase: f32,
        amplitude: f32,
    },
    Pad(Pad),
    File {
        file: AudioFile,
        looped: bool,
    },
}

/// A paced source (tone or file) with start offset, length limit and fades.
struct Reader {
    input: Input,
    channels: usize,
    /// Frames delivered so far.
    pos: u64,
    limit: Option<u64>,
    fade_in: Option<u64>,
    fade_out: Option<(u64, u64)>,
}

impl Reader {
    fn open(
        source: &Source,
        rate: u32,
        channels: u16,
        start: Option<Duration>,
        duration: Option<Duration>,
    ) -> Result<Self> {
        let input = match source {
            Source::Tone {
                frequency,
                amplitude,
            } => Input::Tone {
                step: std::f32::consts::TAU * frequency / rate as f32,
                phase: 0.0,
                amplitude: *amplitude,
            },
            Source::Pad { amplitude } => Input::Pad(Pad::new(rate, *amplitude)),
            Source::File { path, looped } => {
                let mut file = AudioFile::open(path)?;
                if let Some(s) = start {
                    if !file.skip((s.as_secs_f64() * rate as f64) as u64)? {
                        bail!(
                            "--start {:.1}s is past the end of {}",
                            s.as_secs_f64(),
                            path.display()
                        );
                    }
                }
                Input::File {
                    file,
                    looped: *looped,
                }
            }
            Source::Capture { .. } => unreachable!("capture isn't read through Reader"),
        };
        let frames = |d: Duration| (d.as_secs_f64() * rate as f64).round() as u64;
        let limit = duration.map(frames);
        Ok(Self {
            input,
            channels: channels as usize,
            pos: 0,
            limit,
            fade_in: start.map(|_| frames(FADE_IN)),
            fade_out: limit.map(|l| {
                let len = frames(FADE_OUT).min(l);
                (l - len, len)
            }),
        })
    }

    /// Fill `out` (interleaved); returns samples written, fewer only at the end.
    fn fill(&mut self, out: &mut [f32]) -> Result<usize> {
        let mut want = out.len();
        if let Some(l) = self.limit {
            want = want.min((l.saturating_sub(self.pos)) as usize * self.channels);
        }
        let out = &mut out[..want];
        let n = match &mut self.input {
            Input::Tone {
                step,
                phase,
                amplitude,
            } => {
                for frame in out.chunks_exact_mut(self.channels) {
                    frame.fill(phase.sin() * *amplitude);
                    *phase = (*phase + *step) % std::f32::consts::TAU;
                }
                out.len()
            }
            Input::Pad(pad) => {
                pad.fill(out, self.channels);
                out.len()
            }
            Input::File { file, looped } => {
                let mut written = 0;
                let mut rewound = false;
                while written < out.len() {
                    let got = file.read(&mut out[written..])?;
                    written += got;
                    if got > 0 {
                        rewound = false;
                    } else if *looped && !rewound {
                        file.rewind()?;
                        rewound = true; // stop on an empty file rather than spinning
                    } else {
                        break;
                    }
                }
                written
            }
        };
        // Fades.
        for (i, frame) in out[..n].chunks_exact_mut(self.channels).enumerate() {
            let p = self.pos + i as u64;
            let mut g = 1.0f32;
            if let Some(len) = self.fade_in {
                if p < len {
                    g *= p as f32 / len as f32;
                }
            }
            if let Some((start, len)) = self.fade_out {
                if p >= start && len > 0 {
                    g *= 1.0 - ((p - start) as f32 / len as f32).min(1.0);
                }
            }
            if g < 1.0 {
                frame.iter_mut().for_each(|s| *s *= g);
            }
        }
        self.pos += (n / self.channels) as u64;
        Ok(n)
    }
}

enum Target {
    Network(UdpSocket),
    Render {
        decoder: Box<dyn Decoder>,
        writer: hound::WavWriter<BufWriter<File>>,
        samples: Vec<f32>,
    },
}

pub struct Sender {
    /// Taken by `run`.
    target: Option<Target>,
    spec: StreamSpec,
    ptime: Duration,
    packetizer: Packetizer,
    converter: Converter,
    processor: Processor,
    /// Rate and channels the source produces.
    source_format: (u32, u16),
    source: Source,
    reader: Option<Reader>,
    duration: Option<Duration>,
    lead_in: Duration,
    /// Rendered chime and gap, at the processing rate and stream channels.
    chime: Vec<f32>,
    tail: Duration,
    loudness: Option<LoudnessInfo>,
    sdp: SessionDescription,
    announce: Option<(MulticastOptions, Duration)>,
    rtcp: Option<SenderRtcp>,
}

impl Sender {
    /// Open the source and socket (or render file), and measure a file's
    /// loudness if processing needs it. Inspect `spec()` before calling `run`.
    pub fn new(cfg: SenderConfig) -> Result<Self> {
        let mut spec = cfg.stream;
        let source_format = match &cfg.source {
            Source::Tone { .. } | Source::Pad { .. } => (spec.sample_rate, spec.channels),
            Source::File { path, .. } => {
                let f = AudioFile::open(path)?;
                let (file_rate, file_channels) = (f.sample_rate(), f.channels());
                if cfg.match_file_format && spec.codec.pcm_encoding().is_some() {
                    spec = StreamSpec::new(spec.codec, file_rate, file_channels);
                }
                (file_rate, file_channels)
            }
            Source::Capture { device } => {
                device::input_format(&device::input_device(device.as_deref())?)?
            }
        };
        if spec.channels == 0
            || spec.sample_rate == 0
            || source_format.0 == 0
            || source_format.1 == 0
        {
            bail!("invalid audio format {spec}");
        }
        // Tones are generated at the stream rate.
        let source_format = match cfg.source {
            Source::Tone { .. } | Source::Pad { .. } => (spec.sample_rate, spec.channels),
            _ => source_format,
        };
        if cfg.start.is_some() && !matches!(cfg.source, Source::File { .. }) {
            bail!("--start only applies to file sources");
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
        if payload_bytes > 1472 - 12 {
            bail!("{payload_bytes} byte packets exceed a 1500-byte MTU; lower --ptime");
        }

        // Processing runs at the source rate on the stream's channel count.
        let band = Band::for_stream(&spec, &cfg.opus);
        let (rate, channels) = (source_format.0, spec.channels);
        let reader = match cfg.source {
            Source::Capture { .. } => None,
            _ => Some(Reader::open(
                &cfg.source,
                rate,
                source_format.1,
                cfg.start,
                cfg.duration,
            )?),
        };
        let mut loudness_info = None;
        let loudness = match (&cfg.source, cfg.process.mode.loudness_target()) {
            (_, None) => Loudness::Dynamic, // unused
            (Source::File { .. }, Some(target)) => {
                let measured = measure(&cfg, rate, source_format.1, channels, band)?;
                let gain_db = measured.map_or(0.0, |m| (target - m).clamp(-30.0, 30.0));
                loudness_info = Some(LoudnessInfo {
                    measured,
                    gain_db,
                    target,
                });
                Loudness::Static { gain_db }
            }
            _ => Loudness::Dynamic,
        };
        let processor = Processor::new(cfg.process, band, loudness, rate, channels as usize);
        let chime = match &cfg.chime {
            Some(c) => c.render(band, cfg.process.volume_db, rate, channels)?,
            None => Vec::new(),
        };

        let pt = cfg
            .payload_type
            .unwrap_or_else(|| spec.default_payload_type());
        let encoder = spec
            .encoder(&cfg.opus)
            .map_err(|e| anyhow!("creating {} encoder: {e}", spec.codec))?;
        let (target, origin) = match &cfg.render {
            Some(path) => {
                let wav = hound::WavSpec {
                    channels: spec.channels,
                    sample_rate: spec.sample_rate,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                let target = Target::Render {
                    decoder: spec.decoder().map_err(|e| anyhow!(e))?,
                    writer: hound::WavWriter::create(path, wav)
                        .with_context(|| format!("creating {}", path.display()))?,
                    samples: Vec::new(),
                };
                (target, IpAddr::V4(Ipv4Addr::LOCALHOST))
            }
            None => {
                let socket = open_sender(&cfg.net)
                    .with_context(|| format!("opening multicast sender for {}", cfg.net.group))?;
                let origin = socket.local_addr()?.ip();
                (Target::Network(socket), origin)
            }
        };
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
        let ssrc = random_u32();
        let rtcp = match (&cfg.rtcp, &target) {
            (Some(r), Target::Network(_)) => Some(
                SenderRtcp::open(
                    &cfg.net,
                    ssrc,
                    spec.rtp_clock_rate(),
                    r.name.as_deref(),
                    r.interval,
                )
                .context("opening RTCP sender (use --no-rtcp to skip)")?,
            ),
            _ => None,
        };
        let announce = cfg.announce.filter(|_| cfg.render.is_none()).map(|a| {
            let mut net = cfg.net.clone();
            net.group = a
                .sap_group
                .unwrap_or_else(|| sap::announce_group_for(cfg.net.group.ip()));
            // Announcements are control traffic; don't mark them as voice.
            net.tos = None;
            (net, a.interval)
        });
        Ok(Self {
            target: Some(target),
            spec,
            ptime,
            packetizer: Packetizer::new(spec, encoder, frames, pt, ssrc),
            converter: Converter::new(rate, channels, spec.sample_rate, spec.channels),
            processor,
            source_format,
            source: cfg.source,
            reader,
            duration: cfg.duration,
            lead_in: cfg.lead_in,
            chime,
            tail: cfg.tail,
            loudness: loudness_info,
            sdp,
            announce,
            rtcp,
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

    /// Length of the chime and its gap, if there is one.
    pub fn chime_duration(&self) -> Duration {
        let frames = self.chime.len() / self.spec.channels as usize;
        Duration::from_secs_f64(frames as f64 / self.source_format.0 as f64)
    }

    /// Loudness measurement for file sources with processing on.
    pub fn loudness(&self) -> Option<LoudnessInfo> {
        self.loudness
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

    /// Where RTCP sender reports go, if sending them.
    pub fn rtcp_group(&self) -> Option<SocketAddr> {
        self.rtcp.as_ref().and_then(|r| r.group())
    }

    /// Stream until the source ends, `duration` elapses, or `stop` is set.
    pub fn run(self, stop: &AtomicBool) -> Result<SenderStats> {
        self.run_with_progress(stop, Arc::default())
    }

    /// Like [`Sender::run`], updating `progress` as packets go out.
    pub fn run_with_progress(
        mut self,
        stop: &AtomicBool,
        progress: Arc<Progress>,
    ) -> Result<SenderStats> {
        // Announces until dropped at the end of `run`, then sends a SAP deletion.
        let _announcer = match &self.announce {
            Some((net, interval)) => Some(
                Announcer::start(net, &self.sdp, *interval)
                    .with_context(|| format!("starting SAP announcements on {}", net.group))?,
            ),
            None => None,
        };
        let capture = matches!(self.source, Source::Capture { .. });
        let target = self.target.take().expect("run consumes the sender");
        let network = matches!(target, Target::Network(_));
        let mut out = Output {
            // Lead-in and chime are always paced; live capture paces itself after that.
            paced: network,
            target,
            start: Instant::now(),
            ptime: self.ptime,
            limit: None,
            stats: SenderStats::default(),
            progress,
            fatal: None,
            rtcp: self.rtcp.take(),
        };
        let (rate, src_channels) = self.source_format;
        let spec = self.spec;
        let silence = |d: Duration| {
            vec![
                0.0f32;
                (d.as_secs_f64() * spec.sample_rate as f64).round() as usize
                    * spec.channels as usize
            ]
        };
        let encoding = |e: String| anyhow!("encoding: {e}");

        if !self.lead_in.is_zero() {
            self.packetizer
                .push(&silence(self.lead_in), |p| out.send(p))
                .map_err(encoding)?;
            out.check()?;
        }
        if !self.chime.is_empty() {
            let mut converted = Vec::new();
            self.converter.process(&self.chime, &mut converted);
            self.packetizer
                .push(&converted, |p| out.send(p))
                .map_err(encoding)?;
            out.check()?;
        }
        out.paced = network && !capture;
        // Paced sources stop on their own; capture stops after `duration` of live audio.
        out.limit = self
            .duration
            .filter(|_| capture)
            .map(|d| out.sent() + (d.as_secs_f64() / self.ptime.as_secs_f64()).ceil() as u64);

        let mut stages = Stages::default();
        if let Source::Capture { device } = &self.source {
            let dev = device::input_device(device.as_deref())?;
            // One second of headroom between the device callback and us.
            let (ring, mut captured) = rtrb::RingBuffer::new(rate as usize * src_channels as usize);
            let dropped = Arc::new(AtomicU64::new(0));
            let _stream = device::capture(&dev, ring, std::thread::current(), dropped.clone())?;
            let mut chunk = vec![0.0f32; 4096 / src_channels as usize * src_channels as usize];
            while !stop.load(Ordering::Relaxed) && !out.done() {
                let n = captured.slots().min(chunk.len()) / src_channels as usize
                    * src_channels as usize;
                if n == 0 {
                    if captured.is_abandoned() {
                        break;
                    }
                    // The capture callback unparks us when it writes.
                    std::thread::park_timeout(Duration::from_millis(100));
                    continue;
                }
                let (got, _) = captured.pop_partial_slice(&mut chunk[..n]);
                let got = got.len();
                stages.run(&mut self, &chunk[..got], false, &mut out)?;
            }
            out.stats.capture_dropped = dropped.load(Ordering::Relaxed);
        } else {
            // Read roughly one packet of source audio at a time.
            let chunk_frames = ((rate as f64 * self.ptime.as_secs_f64()).ceil() as usize).max(64);
            let mut buf = vec![0.0f32; chunk_frames * src_channels as usize];
            let mut reader = self.reader.take().expect("paced sources have a reader");
            while !stop.load(Ordering::Relaxed) && !out.done() {
                let n = reader.fill(&mut buf)?;
                let last = n < buf.len();
                stages.run(&mut self, &buf[..n], last, &mut out)?;
                if last {
                    self.packetizer
                        .push(&silence(self.tail), |p| out.send(p))
                        .map_err(encoding)?;
                    self.packetizer.finish(|p| out.send(p)).map_err(encoding)?;
                    out.check()?;
                    break;
                }
            }
        }
        if let Some(r) = &mut out.rtcp {
            r.bye(None);
        }
        let mut stats = out.stats;
        stats.frames = stats.packets * self.packetizer.frames_per_packet() as u64;
        if let Target::Render { writer, .. } = out.target {
            writer.finalize()?;
        }
        Ok(stats)
    }
}

/// Scratch buffers for source chunk → channels → processing → resampling → packets.
#[derive(Default)]
struct Stages {
    mapped: Vec<f32>,
    processed: Vec<f32>,
    converted: Vec<f32>,
}

impl Stages {
    fn run(&mut self, s: &mut Sender, chunk: &[f32], last: bool, out: &mut Output) -> Result<()> {
        self.mapped.clear();
        map_channels(
            chunk,
            s.source_format.1 as usize,
            s.spec.channels as usize,
            &mut self.mapped,
        );
        self.processed.clear();
        s.processor.process(&self.mapped, &mut self.processed);
        if last {
            s.processor.flush(&mut self.processed);
        }
        self.converted.clear();
        s.converter.process(&self.processed, &mut self.converted);
        if last {
            s.converter.flush(&mut self.converted);
        }
        s.packetizer
            .push(&self.converted, |p| out.send(p))
            .map_err(|e| anyhow!("encoding: {e}"))?;
        out.check()
    }
}

/// First pass over a WAV file: run the processing chain up to loudness
/// normalisation and measure the integrated loudness.
fn measure(
    cfg: &SenderConfig,
    rate: u32,
    src_channels: u16,
    channels: u16,
    band: Band,
) -> Result<Option<f64>> {
    let mut reader = Reader::open(&cfg.source, rate, src_channels, cfg.start, cfg.duration)?;
    // With looping and no duration, one pass of the file is representative.
    if let Input::File { looped, .. } = &mut reader.input {
        if cfg.duration.is_none() {
            *looped = false;
        }
    }
    let mut p = Processor::new(
        cfg.process,
        band,
        Loudness::Measure,
        rate,
        channels as usize,
    );
    let mut buf = vec![0.0f32; rate as usize * src_channels as usize];
    let (mut mapped, mut sink) = (Vec::new(), Vec::new());
    loop {
        let n = reader.fill(&mut buf)?;
        mapped.clear();
        map_channels(
            &buf[..n],
            src_channels as usize,
            channels as usize,
            &mut mapped,
        );
        sink.clear();
        p.process(&mapped, &mut sink);
        if n < buf.len() {
            p.flush(&mut sink);
            break;
        }
    }
    Ok(p.measured_loudness())
}

/// Sends packets (paced in real time for file and tone sources), or decodes
/// them into a WAV file when rendering.
struct Output {
    target: Target,
    paced: bool,
    start: Instant,
    ptime: Duration,
    limit: Option<u64>,
    stats: SenderStats,
    progress: Arc<Progress>,
    fatal: Option<anyhow::Error>,
    rtcp: Option<SenderRtcp>,
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
        match &mut self.target {
            Target::Network(socket) => match socket.send(pkt) {
                Ok(_) => {
                    self.stats.packets += 1;
                    self.progress.packets.store(self.stats.packets, Ordering::Relaxed);
                    if let Some(r) = &mut self.rtcp {
                        r.sent(pkt, Instant::now());
                    }
                }
                // Transient conditions (full buffers, no route yet) shouldn't kill a stream.
                Err(e)
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted)
                        || e.raw_os_error() == Some(105) /* ENOBUFS */ =>
                {
                    self.stats.send_errors += 1;
                    self.progress
                        .send_errors
                        .store(self.stats.send_errors, Ordering::Relaxed);
                }
                Err(e) => self.fatal = Some(anyhow::Error::from(e).context("sending packet")),
            },
            Target::Render {
                decoder,
                writer,
                samples,
            } => {
                let result = rtp::parse(pkt)
                    .map_err(|e| anyhow!("{e}"))
                    .and_then(|p| {
                        samples.clear();
                        decoder
                            .decode(p.payload, samples)
                            .map_err(|e| anyhow!("decoding: {e}"))
                    })
                    .and_then(|_| {
                        for &s in samples.iter() {
                            writer.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
                        }
                        Ok(())
                    });
                match result {
                    Ok(()) => {
                        self.stats.packets += 1;
                        self.progress
                            .packets
                            .store(self.stats.packets, Ordering::Relaxed);
                    }
                    Err(e) => self.fatal = Some(e.context("rendering")),
                }
            }
        }
    }

    fn check(&mut self) -> Result<()> {
        match self.fatal.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Mode {
    /// The Opus tuning that suits this content (`mcast-play.sh` behaviour).
    pub fn opus_mode(self) -> Option<crate::codec::OpusMode> {
        match self {
            Mode::Voice => Some(crate::codec::OpusMode::Voice),
            Mode::Music => Some(crate::codec::OpusMode::Music),
            Mode::None => None,
        }
    }
}
