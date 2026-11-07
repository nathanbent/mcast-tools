//! Multicast RTP audio receiver: plays to a device or records to WAV.

use crate::codec::{Decoder, StreamSpec};
use crate::device;
use crate::format::{AudioFormat, Encoding};
use crate::jitter::JitterBuffer;
use anyhow::{anyhow, Context, Result};
use mcast_core::net::{open_receiver, MulticastOptions};
use mcast_core::rtp::{self, SeqEvent, SequenceTracker};
use std::fs::File;
use std::io::{BufWriter, ErrorKind};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Gaps longer than this many packets are not concealed.
const MAX_CONCEAL_PACKETS: u16 = 100;

#[derive(Debug, Clone)]
pub enum Sink {
    /// Play through an output device (default device if `None`).
    Play { device: Option<String> },
    /// Record to a WAV file.
    Wav { path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    pub net: MulticastOptions,
    /// How to decode the stream. `None` infers it from the RTP payload type,
    /// which works for static types (G.711 PCMU/PCMA, G.722, L16 44.1 kHz).
    pub stream: Option<StreamSpec>,
    /// Used for dynamic payload types when `stream` is `None`.
    pub fallback: Option<StreamSpec>,
    /// Ignore packets with any other payload type.
    pub payload_type: Option<u8>,
    /// Playout delay used by the jitter buffer.
    pub latency: Duration,
    pub sink: Sink,
    /// Stop after this long.
    pub duration: Option<Duration>,
    /// How often to emit `Event::Stats`.
    pub stats_interval: Duration,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ReceiverStats {
    pub packets: u64,
    pub lost: u64,
    pub concealed: u64,
    pub late: u64,
    pub invalid: u64,
    pub underruns: u64,
    pub overflows: u64,
    pub buffered: Duration,
}

pub enum Event<'a> {
    Stats(&'a ReceiverStats),
    NewSource {
        ssrc: u32,
        from: SocketAddr,
        payload_type: u8,
        spec: StreamSpec,
    },
    /// A dynamic payload type arrived and no codec was configured.
    UnknownPayloadType {
        payload_type: u8,
    },
    /// Packets couldn't be decoded (reported once per source).
    DecodeError {
        message: String,
    },
}

enum Output {
    Play {
        format: AudioFormat,
        buffer: Arc<Mutex<JitterBuffer>>,
        _stream: cpal::Stream,
    },
    Wav {
        format: AudioFormat,
        writer: hound::WavWriter<BufWriter<File>>,
    },
}

impl Output {
    fn open(sink: &Sink, format: AudioFormat, latency: Duration) -> Result<Self> {
        Ok(match sink {
            Sink::Play { device } => {
                let dev = device::output_device(device.as_deref())?;
                let target = format.frames_for(latency);
                let buffer = Arc::new(Mutex::new(JitterBuffer::new(format.channels, target)));
                let stream = device::play(&dev, &format, buffer.clone())?;
                Output::Play {
                    format,
                    buffer,
                    _stream: stream,
                }
            }
            Sink::Wav { path } => {
                let spec = hound::WavSpec {
                    channels: format.channels,
                    sample_rate: format.sample_rate,
                    bits_per_sample: format.encoding.bits(),
                    sample_format: hound::SampleFormat::Int,
                };
                let writer = hound::WavWriter::create(path, spec)
                    .with_context(|| format!("creating {}", path.display()))?;
                Output::Wav { format, writer }
            }
        })
    }

    fn format(&self) -> AudioFormat {
        match self {
            Output::Play { format, .. } | Output::Wav { format, .. } => *format,
        }
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        match self {
            Output::Play { buffer, .. } => buffer.lock().unwrap().push(samples),
            Output::Wav { writer, format } => {
                for &s in samples {
                    let s = s.clamp(-1.0, 1.0);
                    match format.encoding {
                        Encoding::L16 => {
                            writer.write_sample((s * i16::MAX as f32).round() as i16)?
                        }
                        Encoding::L24 => writer.write_sample((s * 8_388_607.0).round() as i32)?,
                    }
                }
            }
        }
        Ok(())
    }

    fn reset(&mut self) {
        if let Output::Play { buffer, .. } = self {
            buffer.lock().unwrap().reset();
        }
    }
}

/// The stream currently being received.
struct Current {
    ssrc: u32,
    decoder: Box<dyn Decoder>,
    tracker: SequenceTracker,
    /// Samples in the last decoded packet, for sizing silence on loss.
    last_samples: usize,
    /// False if this source can't be written to the open output.
    accept: bool,
    reported_error: bool,
}

/// Receive until `stop` is set or `duration` elapses.
pub fn run(
    cfg: ReceiverConfig,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Event<'_>),
) -> Result<ReceiverStats> {
    let socket = open_receiver(&cfg.net)
        .with_context(|| format!("joining multicast group {}", cfg.net.group))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;

    let start = Instant::now();
    let mut last_stats = start;
    let mut stats = ReceiverStats::default();
    let mut lost_before = 0; // losses from previous sources
    let mut output: Option<Output> = None;
    let mut current: Option<Current> = None;
    let mut warned_pt = None;
    let mut buf = vec![0u8; 65_536];
    let mut decoded = Vec::new();

    while !stop.load(Ordering::Relaxed) && cfg.duration.map_or(true, |d| start.elapsed() < d) {
        if last_stats.elapsed() >= cfg.stats_interval {
            last_stats = Instant::now();
            refresh(&mut stats, lost_before, current.as_ref(), output.as_ref());
            on_event(Event::Stats(&stats));
        }

        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(e) => return Err(e).context("receiving"),
        };
        let Ok(pkt) = rtp::parse(&buf[..len]) else {
            stats.invalid += 1;
            continue;
        };
        let pt = pkt.header.payload_type;
        if cfg.payload_type.is_some_and(|want| want != pt) {
            stats.invalid += 1;
            continue;
        }

        // A new sender (or restarted stream): pick a decoder and make sure
        // the output matches its format.
        if current.as_ref().map(|c| c.ssrc) != Some(pkt.header.ssrc) {
            let Some(spec) = cfg
                .stream
                .or_else(|| StreamSpec::from_static_payload_type(pt))
                .or(cfg.fallback)
            else {
                stats.invalid += 1;
                if warned_pt != Some(pt) {
                    warned_pt = Some(pt);
                    on_event(Event::UnknownPayloadType { payload_type: pt });
                }
                continue;
            };
            let decoder = spec
                .decoder()
                .map_err(|e| anyhow!("creating {} decoder: {e}", spec.codec))?;
            let format = spec.output_format();
            let mut accept = true;
            match &mut output {
                Some(o) if o.format() == format => o.reset(),
                Some(Output::Wav { format: open, .. }) => {
                    // A WAV file can't change format midway; ignore this source.
                    accept = false;
                    on_event(Event::DecodeError {
                        message: format!("ignoring {spec} stream: recording is already {open}"),
                    });
                }
                _ => output = Some(Output::open(&cfg.sink, format, cfg.latency)?),
            }
            if let Some(old) = current.take() {
                lost_before += old.tracker.lost;
            }
            current = Some(Current {
                ssrc: pkt.header.ssrc,
                decoder,
                tracker: SequenceTracker::new(),
                last_samples: 0,
                accept,
                reported_error: false,
            });
            on_event(Event::NewSource {
                ssrc: pkt.header.ssrc,
                from,
                payload_type: pt,
                spec,
            });
        }
        let (Some(cur), Some(out)) = (current.as_mut(), output.as_mut()) else {
            continue;
        };
        if !cur.accept {
            continue;
        }
        stats.packets += 1;

        decoded.clear();
        match cur.tracker.update(pkt.header.sequence) {
            SeqEvent::Late => continue, // already played past it
            SeqEvent::Reset => out.reset(),
            SeqEvent::Gap(n) if n <= MAX_CONCEAL_PACKETS => {
                if cur.decoder.conceal(n as usize, pkt.payload, &mut decoded) {
                    stats.concealed += n as u64;
                } else {
                    decoded.resize(n as usize * cur.last_samples, 0.0);
                }
            }
            SeqEvent::Gap(_) | SeqEvent::InOrder => {}
        }
        let before = decoded.len();
        if let Err(message) = cur.decoder.decode(pkt.payload, &mut decoded) {
            stats.invalid += 1;
            if !cur.reported_error {
                cur.reported_error = true;
                on_event(Event::DecodeError { message });
            }
        }
        if decoded.len() > before {
            cur.last_samples = decoded.len() - before;
        }
        out.write(&decoded)?;
    }

    refresh(&mut stats, lost_before, current.as_ref(), output.as_ref());
    if let Some(Output::Wav { writer, .. }) = output {
        writer.finalize()?;
    }
    Ok(stats)
}

fn refresh(
    stats: &mut ReceiverStats,
    lost_before: u64,
    current: Option<&Current>,
    output: Option<&Output>,
) {
    if let Some(c) = current {
        stats.lost = lost_before + c.tracker.lost;
        stats.late = c.tracker.late;
    }
    if let Some(Output::Play { buffer, format, .. }) = output {
        let jb = buffer.lock().unwrap();
        stats.underruns = jb.underruns;
        stats.overflows = jb.overflows;
        stats.buffered = format.duration_of(jb.buffered_frames());
    }
}
