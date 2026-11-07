//! Multicast RTP audio sender.

use crate::device;
use crate::format::AudioFormat;
use crate::packetizer::{random_u32, Packetizer};
use anyhow::{bail, Context, Result};
use mcast_core::net::{open_sender, MulticastOptions};
use std::fs::File;
use std::io::BufReader;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum Source {
    /// Sine test tone.
    Tone { frequency: f32, amplitude: f32 },
    /// WAV file; its sample rate and channel count override the configured format.
    Wav { path: PathBuf, looped: bool },
    /// Live capture from an input device (default device if `None`).
    Capture { device: Option<String> },
}

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub net: MulticastOptions,
    pub format: AudioFormat,
    /// Audio duration per packet.
    pub ptime: Duration,
    /// RTP payload type; `None` picks the RFC 3551 static type or 96.
    pub payload_type: Option<u8>,
    pub source: Source,
    /// Stop after this much audio has been sent.
    pub duration: Option<Duration>,
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
        channels: usize,
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
    format: AudioFormat,
    packetizer: Packetizer,
    input: Input,
    duration: Option<Duration>,
}

impl Sender {
    /// Open the source and socket. Inspect `format()` before calling `run`.
    pub fn new(cfg: SenderConfig) -> Result<Self> {
        let mut format = cfg.format;
        let input = match cfg.source {
            Source::Tone {
                frequency,
                amplitude,
            } => Input::Tone {
                step: std::f32::consts::TAU * frequency / format.sample_rate as f32,
                phase: 0.0,
                amplitude,
                channels: format.channels as usize,
            },
            Source::Wav { path, looped } => {
                let reader = hound::WavReader::open(&path)
                    .with_context(|| format!("opening {}", path.display()))?;
                let spec = reader.spec();
                format.sample_rate = spec.sample_rate;
                format.channels = spec.channels;
                Input::Wav { reader, looped }
            }
            Source::Capture { device } => Input::Capture { device },
        };
        if format.channels == 0 || format.sample_rate == 0 {
            bail!("invalid audio format {format}");
        }
        let frames = format.frames_for(cfg.ptime);
        let max_payload = 1472 - 12; // fits a 1500-byte Ethernet MTU over IPv4
        if frames * format.frame_bytes() > max_payload {
            bail!(
                "{} byte packets exceed the MTU; lower --ptime",
                frames * format.frame_bytes()
            );
        }
        let pt = cfg
            .payload_type
            .unwrap_or_else(|| format.default_payload_type());
        let socket = open_sender(&cfg.net)
            .with_context(|| format!("opening multicast sender for {}", cfg.net.group))?;
        Ok(Self {
            socket,
            format,
            packetizer: Packetizer::new(format, frames, pt, random_u32()),
            input,
            duration: cfg.duration,
        })
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn frames_per_packet(&self) -> usize {
        self.packetizer.frames_per_packet()
    }

    /// Stream until the source ends, `duration` elapses, or `stop` is set.
    pub fn run(mut self, stop: &AtomicBool) -> Result<SenderStats> {
        let mut stats = SenderStats::default();
        let limit = self
            .duration
            .map(|d| (d.as_secs_f64() * self.format.sample_rate as f64) as u64);

        if let Input::Capture { device } = &self.input {
            let dev = device::input_device(device.as_deref())?;
            let (tx, rx) = mpsc::sync_channel(64);
            let _stream = device::capture(&dev, &self.format, tx)?;
            let channels = self.format.channels as u64;
            while !stop.load(Ordering::Relaxed) && limit.map_or(true, |l| stats.frames < l) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(chunk) => {
                        stats.frames += chunk.len() as u64 / channels;
                        send(&self.socket, &mut self.packetizer, &chunk, &mut stats)?;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            return Ok(stats);
        }

        // File and tone sources are paced against the wall clock.
        let fpp = self.frames_per_packet();
        let mut buf = vec![0.0f32; fpp * self.format.channels as usize];
        let start = Instant::now();
        while !stop.load(Ordering::Relaxed) && limit.map_or(true, |l| stats.frames < l) {
            let n = self.fill(&mut buf)?;
            if n == 0 {
                break;
            }
            buf[n..].fill(0.0);
            send(&self.socket, &mut self.packetizer, &buf, &mut stats)?;
            stats.frames += fpp as u64;

            let due = start
                + Duration::from_secs_f64(stats.frames as f64 / self.format.sample_rate as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        Ok(stats)
    }

    /// Fill `out` from a paced source; returns the number of samples written.
    fn fill(&mut self, out: &mut [f32]) -> Result<usize> {
        match &mut self.input {
            Input::Tone {
                step,
                phase,
                amplitude,
                channels,
            } => {
                for frame in out.chunks_exact_mut(*channels) {
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

fn send(
    socket: &UdpSocket,
    packetizer: &mut Packetizer,
    samples: &[f32],
    stats: &mut SenderStats,
) -> Result<()> {
    let mut fatal = None;
    packetizer.push(samples, |pkt| match socket.send(pkt) {
        Ok(_) => stats.packets += 1,
        // Transient conditions (full buffers, no route yet) shouldn't kill a stream.
        Err(e) if matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) || e.raw_os_error() == Some(105) /* ENOBUFS */ =>
        {
            stats.send_errors += 1
        }
        Err(e) => fatal = Some(e),
    });
    match fatal {
        Some(e) => Err(e).context("sending packet"),
        None => Ok(()),
    }
}
