//! Multicast RTP audio receiver: plays to a device or records to WAV.

use crate::device;
use crate::format::{AudioFormat, Encoding};
use crate::jitter::JitterBuffer;
use anyhow::{Context, Result};
use mcast_core::net::{open_receiver, MulticastOptions};
use mcast_core::rtp::{self, SeqEvent, SequenceTracker};
use std::fs::File;
use std::io::{BufWriter, ErrorKind};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Gaps longer than this many packets are not filled with silence.
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
    pub format: AudioFormat,
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
    },
    /// Payload length doesn't fit the configured format.
    FormatMismatch {
        payload_len: usize,
    },
}

enum Output {
    Play {
        buffer: Arc<Mutex<JitterBuffer>>,
        _stream: cpal::Stream,
    },
    Wav {
        writer: hound::WavWriter<BufWriter<File>>,
        encoding: Encoding,
    },
}

impl Output {
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        match self {
            Output::Play { buffer, .. } => buffer.lock().unwrap().push(samples),
            Output::Wav { writer, encoding } => {
                for &s in samples {
                    let s = s.clamp(-1.0, 1.0);
                    match encoding {
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

    fn silence(&mut self, samples: usize) -> Result<()> {
        match self {
            Output::Play { buffer, .. } => {
                let mut jb = buffer.lock().unwrap();
                let frames = samples / jb.channels();
                jb.push_silence(frames);
                Ok(())
            }
            Output::Wav { .. } => self.write(&vec![0.0; samples]),
        }
    }

    fn reset(&mut self) {
        if let Output::Play { buffer, .. } = self {
            buffer.lock().unwrap().reset();
        }
    }
}

/// Receive until `stop` is set or `duration` elapses.
pub fn run(
    cfg: ReceiverConfig,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Event<'_>),
) -> Result<ReceiverStats> {
    let format = cfg.format;
    let socket = open_receiver(&cfg.net)
        .with_context(|| format!("joining multicast group {}", cfg.net.group))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;

    let mut output = match &cfg.sink {
        Sink::Play { device } => {
            let dev = device::output_device(device.as_deref())?;
            let target = format.frames_for(cfg.latency);
            let buffer = Arc::new(Mutex::new(JitterBuffer::new(format.channels, target)));
            let stream = device::play(&dev, &format, buffer.clone())?;
            Output::Play {
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
            Output::Wav {
                writer,
                encoding: format.encoding,
            }
        }
    };

    let start = Instant::now();
    let mut last_stats = start;
    let mut stats = ReceiverStats::default();
    let mut tracker = SequenceTracker::new();
    let mut ssrc: Option<u32> = None;
    let mut warned_format = false;
    let mut buf = vec![0u8; 65_536];
    let mut decoded = Vec::new();

    while !stop.load(Ordering::Relaxed) && cfg.duration.map_or(true, |d| start.elapsed() < d) {
        if last_stats.elapsed() >= cfg.stats_interval {
            last_stats = Instant::now();
            refresh(&mut stats, &tracker, &output, &format);
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
        if pkt.payload.len() % format.frame_bytes() != 0 {
            stats.invalid += 1;
            if !warned_format {
                warned_format = true;
                on_event(Event::FormatMismatch {
                    payload_len: pkt.payload.len(),
                });
            }
            continue;
        }

        if ssrc != Some(pkt.header.ssrc) {
            ssrc = Some(pkt.header.ssrc);
            tracker = SequenceTracker::new();
            output.reset();
            on_event(Event::NewSource {
                ssrc: pkt.header.ssrc,
                from,
                payload_type: pkt.header.payload_type,
            });
        }
        stats.packets += 1;

        let samples_per_packet = pkt.payload.len() / format.encoding.bytes_per_sample();
        match tracker.update(pkt.header.sequence) {
            SeqEvent::Late => continue, // already played past it
            SeqEvent::Reset => output.reset(),
            SeqEvent::Gap(n) if n <= MAX_CONCEAL_PACKETS => {
                output.silence(n as usize * samples_per_packet)?
            }
            SeqEvent::Gap(_) | SeqEvent::InOrder => {}
        }
        decoded.clear();
        format.decode(pkt.payload, &mut decoded);
        output.write(&decoded)?;
    }

    refresh(&mut stats, &tracker, &output, &format);
    if let Output::Wav { writer, .. } = output {
        writer.finalize()?;
    }
    Ok(stats)
}

fn refresh(
    stats: &mut ReceiverStats,
    tracker: &SequenceTracker,
    output: &Output,
    format: &AudioFormat,
) {
    stats.lost = tracker.lost;
    stats.late = tracker.late;
    if let Output::Play { buffer, .. } = output {
        let jb = buffer.lock().unwrap();
        stats.underruns = jb.underruns;
        stats.overflows = jb.overflows;
        stats.buffered = format.duration_of(jb.buffered_frames());
    }
}
