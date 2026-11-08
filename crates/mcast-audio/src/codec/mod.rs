//! Codecs the receiver can decode, and how to pick one for a stream.

mod g711;
mod g722;

use crate::format::{AudioFormat, Encoding};
use std::fmt;
use std::str::FromStr;

pub use g722::{G722Decoder, G722Encoder};

/// G.711 mu-law encoding of one sample (exposed for tests and tools).
pub fn g711_encode_ulaw(sample: i16) -> u8 {
    g711::linear_to_ulaw(sample)
}
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    L16,
    L24,
    /// G.711 mu-law.
    Pcmu,
    /// G.711 A-law.
    Pcma,
    G722,
    Opus,
}

impl Codec {
    /// Match an SDP rtpmap encoding name.
    pub fn from_sdp_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_uppercase().as_str() {
            "L16" => Codec::L16,
            "L24" => Codec::L24,
            "PCMU" => Codec::Pcmu,
            "PCMA" => Codec::Pcma,
            "G722" => Codec::G722,
            "OPUS" => Codec::Opus,
            _ => return None,
        })
    }

    /// The PCM encoding, for codecs we can also send.
    pub fn pcm_encoding(self) -> Option<Encoding> {
        match self {
            Codec::L16 => Some(Encoding::L16),
            Codec::L24 => Some(Encoding::L24),
            _ => None,
        }
    }
}

impl FromStr for Codec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "l16" => Ok(Codec::L16),
            "l24" => Ok(Codec::L24),
            "pcmu" | "g711u" | "ulaw" => Ok(Codec::Pcmu),
            "pcma" | "g711a" | "alaw" => Ok(Codec::Pcma),
            "g722" => Ok(Codec::G722),
            "opus" => Ok(Codec::Opus),
            _ => Err(format!(
                "unknown encoding '{s}' (expected l16, l24, pcmu/g711u, pcma/g711a, g722, opus)"
            )),
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Codec::L16 => "L16",
            Codec::L24 => "L24",
            Codec::Pcmu => "PCMU",
            Codec::Pcma => "PCMA",
            Codec::G722 => "G722",
            Codec::Opus => "opus",
        })
    }
}

/// What the receiver needs to decode a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSpec {
    pub codec: Codec,
    /// Decoded sample rate. Fixed by the codec except for L16/L24.
    pub sample_rate: u32,
    /// Decoded channels. G.711/G.722 are always mono; Opus may be 1 or 2.
    pub channels: u16,
}

impl StreamSpec {
    /// Build a spec, forcing the rate/channels the codec dictates.
    pub fn new(codec: Codec, sample_rate: u32, channels: u16) -> Self {
        let (sample_rate, channels) = match codec {
            Codec::L16 | Codec::L24 => (sample_rate, channels),
            Codec::Pcmu | Codec::Pcma => (8_000, 1),
            Codec::G722 => (16_000, 1),
            Codec::Opus => (48_000, channels.clamp(1, 2)),
        };
        Self {
            codec,
            sample_rate,
            channels,
        }
    }

    /// RFC 3551 static payload types.
    pub fn from_static_payload_type(pt: u8) -> Option<Self> {
        Some(match pt {
            0 => Self::new(Codec::Pcmu, 8_000, 1),
            8 => Self::new(Codec::Pcma, 8_000, 1),
            9 => Self::new(Codec::G722, 16_000, 1),
            10 => Self::new(Codec::L16, 44_100, 2),
            11 => Self::new(Codec::L16, 44_100, 1),
            _ => return None,
        })
    }

    /// Format of the decoded PCM (bit depth used when recording to WAV).
    pub fn output_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: self.channels,
            encoding: if self.codec == Codec::L24 {
                Encoding::L24
            } else {
                Encoding::L16
            },
        }
    }

    pub fn decoder(&self) -> Result<Box<dyn Decoder>, String> {
        Ok(match self.codec {
            Codec::L16 | Codec::L24 => Box::new(PcmDecoder(self.output_format())),
            Codec::Pcmu => Box::new(G711Decoder { alaw: false }),
            Codec::Pcma => Box::new(G711Decoder { alaw: true }),
            Codec::G722 => Box::new(G722Decoder::new()),
            Codec::Opus => Box::new(OpusDecoder::new(self.channels)?),
        })
    }
}

impl StreamSpec {
    /// RTP timestamp clock (8000 for G.722 despite 16 kHz audio, per RFC 3551).
    pub fn rtp_clock_rate(&self) -> u32 {
        match self.codec {
            Codec::G722 => 8_000,
            _ => self.sample_rate,
        }
    }

    /// Static payload type where RFC 3551 assigns one, else a dynamic one.
    pub fn default_payload_type(&self) -> u8 {
        match self.codec {
            Codec::Pcmu => 0,
            Codec::Pcma => 8,
            Codec::G722 => 9,
            Codec::Opus => 107,
            Codec::L16 | Codec::L24 => self.output_format().default_payload_type(),
        }
    }

    /// 1 ms for uncompressed PCM (AES67), 20 ms for telephony codecs and Opus.
    pub fn default_ptime(&self) -> Duration {
        match self.codec {
            Codec::L16 | Codec::L24 => Duration::from_millis(1),
            _ => Duration::from_millis(20),
        }
    }

    /// Check a packet time is one this codec can produce.
    pub fn check_ptime(&self, ptime: Duration) -> Result<(), String> {
        let ms = ptime.as_secs_f64() * 1000.0;
        match self.codec {
            Codec::Opus
                if ![2.5, 5.0, 10.0, 20.0, 40.0, 60.0]
                    .iter()
                    .any(|v| (v - ms).abs() < 1e-6) =>
            {
                Err(format!(
                    "Opus needs a ptime of 2.5, 5, 10, 20, 40 or 60 ms, not {ms}"
                ))
            }
            _ if ms <= 0.0 => Err("ptime must be positive".into()),
            _ => Ok(()),
        }
    }

    /// `a=rtpmap` value (without the payload type).
    pub fn rtpmap(&self) -> String {
        match self.codec {
            Codec::L16 | Codec::L24 => {
                format!("{}/{}/{}", self.codec, self.sample_rate, self.channels)
            }
            // RFC 7587: always "/2"; mono vs stereo is signalled in fmtp.
            Codec::Opus => "opus/48000/2".into(),
            _ => format!("{}/{}", self.codec, self.rtp_clock_rate()),
        }
    }

    pub fn encoder(&self, opus: &OpusOptions) -> Result<Box<dyn Encoder>, String> {
        Ok(match self.codec {
            Codec::L16 | Codec::L24 => Box::new(PcmEncoder(self.output_format())),
            Codec::Pcmu => Box::new(G711Encoder { alaw: false }),
            Codec::Pcma => Box::new(G711Encoder { alaw: true }),
            Codec::G722 => Box::new(G722Encoder::new()),
            Codec::Opus => Box::new(OpusEncoder::new(self.channels, opus)?),
        })
    }
}

impl fmt::Display for StreamSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.codec, self.sample_rate, self.channels)
    }
}

/// Turns one packet's worth of interleaved f32 samples into an RTP payload.
pub trait Encoder: Send {
    fn encode(&mut self, samples: &[f32], out: &mut Vec<u8>) -> Result<(), String>;
}

/// Full-scale is ±32768, the inverse of the G.711/G.722 decoders and of
/// reading 16-bit WAV, so 16-bit input passes through unchanged.
fn to_i16(s: f32) -> i16 {
    (s * 32768.0)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

struct PcmEncoder(AudioFormat);

impl Encoder for PcmEncoder {
    fn encode(&mut self, samples: &[f32], out: &mut Vec<u8>) -> Result<(), String> {
        self.0.encode(samples, out);
        Ok(())
    }
}

struct G711Encoder {
    alaw: bool,
}

impl Encoder for G711Encoder {
    fn encode(&mut self, samples: &[f32], out: &mut Vec<u8>) -> Result<(), String> {
        let f = if self.alaw {
            g711::linear_to_alaw
        } else {
            g711::linear_to_ulaw
        };
        out.extend(samples.iter().map(|&s| f(to_i16(s))));
        Ok(())
    }
}

impl Encoder for G722Encoder {
    fn encode(&mut self, samples: &[f32], out: &mut Vec<u8>) -> Result<(), String> {
        let pcm: Vec<i16> = samples.iter().map(|&s| to_i16(s)).collect();
        self.encode_into(&pcm, out);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpusMode {
    /// Favours speech intelligibility (libopus OPUS_APPLICATION_VOIP).
    Voice,
    /// Favours fidelity for music and mixed content.
    #[default]
    Music,
}

impl FromStr for OpusMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "voice" | "voip" => Ok(OpusMode::Voice),
            "music" | "audio" => Ok(OpusMode::Music),
            _ => Err(format!("unknown Opus mode '{s}' (expected voice or music)")),
        }
    }
}

impl fmt::Display for OpusMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OpusMode::Voice => "voice",
            OpusMode::Music => "music",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusBandwidth {
    Narrow,
    Wide,
    SuperWide,
    Full,
}

impl FromStr for OpusBandwidth {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "nb" | "narrow" => Ok(OpusBandwidth::Narrow),
            "wb" | "wide" => Ok(OpusBandwidth::Wide),
            "swb" | "superwide" => Ok(OpusBandwidth::SuperWide),
            "fb" | "full" => Ok(OpusBandwidth::Full),
            _ => Err(format!(
                "unknown Opus bandwidth '{s}' (expected nb, wb, swb, fb)"
            )),
        }
    }
}

impl fmt::Display for OpusBandwidth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OpusBandwidth::Narrow => "nb",
            OpusBandwidth::Wide => "wb",
            OpusBandwidth::SuperWide => "swb",
            OpusBandwidth::Full => "fb",
        })
    }
}

/// Opus encoder settings. Defaults match a typical paging setup:
/// 64 kbit/s VBR with in-band FEC tuned for 5% loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusOptions {
    pub mode: OpusMode,
    pub bitrate: u32,
    pub max_bandwidth: Option<OpusBandwidth>,
    pub fec: bool,
    pub expected_loss_percent: u8,
}

impl Default for OpusOptions {
    fn default() -> Self {
        Self {
            mode: OpusMode::Music,
            bitrate: 64_000,
            max_bandwidth: None,
            fec: true,
            expected_loss_percent: 5,
        }
    }
}

struct OpusEncoder {
    encoder: opus::Encoder,
    buf: Vec<u8>,
}

impl OpusEncoder {
    fn new(channels: u16, o: &OpusOptions) -> Result<Self, String> {
        let e = |e: opus::Error| e.to_string();
        let ch = if channels == 2 {
            opus::Channels::Stereo
        } else {
            opus::Channels::Mono
        };
        let app = match o.mode {
            OpusMode::Voice => opus::Application::Voip,
            OpusMode::Music => opus::Application::Audio,
        };
        let mut encoder = opus::Encoder::new(48_000, ch, app).map_err(e)?;
        encoder
            .set_bitrate(opus::Bitrate::Bits(o.bitrate as i32))
            .map_err(e)?;
        encoder.set_vbr(true).map_err(e)?;
        encoder.set_inband_fec(o.fec).map_err(e)?;
        encoder
            .set_packet_loss_perc(if o.fec {
                o.expected_loss_percent as i32
            } else {
                0
            })
            .map_err(e)?;
        if let Some(bw) = o.max_bandwidth {
            encoder
                .set_max_bandwidth(match bw {
                    OpusBandwidth::Narrow => opus::Bandwidth::Narrowband,
                    OpusBandwidth::Wide => opus::Bandwidth::Wideband,
                    OpusBandwidth::SuperWide => opus::Bandwidth::Superwideband,
                    OpusBandwidth::Full => opus::Bandwidth::Fullband,
                })
                .map_err(e)?;
        }
        Ok(Self {
            encoder,
            buf: vec![0; 4000],
        })
    }
}

impl Encoder for OpusEncoder {
    fn encode(&mut self, samples: &[f32], out: &mut Vec<u8>) -> Result<(), String> {
        let n = self
            .encoder
            .encode_float(samples, &mut self.buf)
            .map_err(|e| e.to_string())?;
        out.extend_from_slice(&self.buf[..n]);
        Ok(())
    }
}

/// Turns RTP payloads into interleaved f32 samples.
pub trait Decoder: Send {
    /// Decode one payload, appending samples to `out`.
    fn decode(&mut self, payload: &[u8], out: &mut Vec<f32>) -> Result<(), String>;

    /// Fill in for `lost` packets that went missing before `next`.
    /// Returns false if the codec can't, and the caller should insert silence.
    fn conceal(&mut self, _lost: usize, _next: &[u8], _out: &mut Vec<f32>) -> bool {
        false
    }
}

struct PcmDecoder(AudioFormat);

impl Decoder for PcmDecoder {
    fn decode(&mut self, payload: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        if !payload.len().is_multiple_of(self.0.frame_bytes()) {
            return Err(format!(
                "{}-byte payload doesn't fit {}",
                payload.len(),
                self.0
            ));
        }
        self.0.decode(payload, out);
        Ok(())
    }
}

struct G711Decoder {
    alaw: bool,
}

impl Decoder for G711Decoder {
    fn decode(&mut self, payload: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let f = if self.alaw {
            g711::alaw_to_linear
        } else {
            g711::ulaw_to_linear
        };
        out.extend(payload.iter().map(|&b| f(b) as f32 / 32768.0));
        Ok(())
    }
}

impl Decoder for G722Decoder {
    fn decode(&mut self, payload: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        self.decode_into(payload, |s| out.push(s as f32 / 32768.0));
        Ok(())
    }
}

/// Opus via libopus, with in-band FEC and packet-loss concealment.
struct OpusDecoder {
    decoder: opus::Decoder,
    channels: usize,
    buf: Vec<f32>,
    last_frames: usize,
}

// Largest Opus packet: 120 ms at 48 kHz.
const OPUS_MAX_FRAMES: usize = 5_760;

impl OpusDecoder {
    fn new(channels: u16) -> Result<Self, String> {
        let ch = if channels == 2 {
            opus::Channels::Stereo
        } else {
            opus::Channels::Mono
        };
        Ok(Self {
            decoder: opus::Decoder::new(48_000, ch).map_err(|e| e.to_string())?,
            channels: channels as usize,
            buf: vec![0.0; OPUS_MAX_FRAMES * 2],
            last_frames: 960,
        })
    }

    fn run(
        &mut self,
        data: &[u8],
        frames: usize,
        fec: bool,
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        let len = frames.min(OPUS_MAX_FRAMES) * self.channels;
        let n = self
            .decoder
            .decode_float(data, &mut self.buf[..len], fec)
            .map_err(|e| e.to_string())?;
        out.extend_from_slice(&self.buf[..n * self.channels]);
        Ok(())
    }
}

impl Decoder for OpusDecoder {
    fn decode(&mut self, payload: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        if let Ok(n) = self.decoder.get_nb_samples(payload) {
            self.last_frames = n;
        }
        self.run(payload, OPUS_MAX_FRAMES, false, out)
    }

    fn conceal(&mut self, lost: usize, next: &[u8], out: &mut Vec<f32>) -> bool {
        let frames = self.last_frames;
        // Earlier losses: libopus packet-loss concealment (empty input).
        for _ in 1..lost {
            if self.run(&[], frames, false, out).is_err() {
                return false;
            }
        }
        // The packet just before `next`: recover it from next's in-band FEC.
        self.run(next, frames, true, out).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_forces_codec_rates() {
        assert_eq!("g711u".parse::<Codec>().unwrap(), Codec::Pcmu);
        assert_eq!(
            StreamSpec::new(Codec::G722, 48_000, 2).to_string(),
            "G722/16000/1"
        );
        assert_eq!(
            StreamSpec::new(Codec::Opus, 8_000, 2).to_string(),
            "opus/48000/2"
        );
        assert_eq!(
            StreamSpec::from_static_payload_type(8).unwrap().codec,
            Codec::Pcma
        );
        assert_eq!(StreamSpec::from_static_payload_type(96), None);
    }

    /// Encode then decode a 1 kHz tone; check level and frequency survive.
    fn roundtrip(spec: StreamSpec) -> (f64, f64) {
        let mut enc = spec.encoder(&OpusOptions::default()).unwrap();
        let mut dec = spec.decoder().unwrap();
        let frames = spec.sample_rate as usize / 50; // 20 ms
        let ch = spec.channels as usize;
        let mut out = Vec::new();
        let mut payload = Vec::new();
        for p in 0..50 {
            let pkt: Vec<f32> = (0..frames * ch)
                .map(|i| {
                    let n = p * frames + i / ch;
                    (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / spec.sample_rate as f32).sin()
                        * 0.5
                })
                .collect();
            payload.clear();
            enc.encode(&pkt, &mut payload).unwrap();
            dec.decode(&payload, &mut out).unwrap();
        }
        let mono: Vec<f32> = out.iter().step_by(ch).copied().collect();
        let mid = &mono[mono.len() / 4..mono.len() * 3 / 4];
        let rms = (mid.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / mid.len() as f64).sqrt();
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        (
            rms / (0.5 / 2f64.sqrt()),
            crossings as f64 * spec.sample_rate as f64 / mid.len() as f64,
        )
    }

    #[test]
    fn every_codec_roundtrips() {
        for spec in [
            StreamSpec::new(Codec::L16, 48_000, 2),
            StreamSpec::new(Codec::L24, 48_000, 1),
            StreamSpec::new(Codec::Pcmu, 0, 0),
            StreamSpec::new(Codec::Pcma, 0, 0),
            StreamSpec::new(Codec::G722, 0, 0),
            StreamSpec::new(Codec::Opus, 0, 1),
            StreamSpec::new(Codec::Opus, 0, 2),
        ] {
            let (gain, freq) = roundtrip(spec);
            assert!((gain - 1.0).abs() < 0.05, "{spec}: gain {gain}");
            assert!((freq - 1000.0).abs() < 5.0, "{spec}: {freq} Hz");
        }
    }

    #[test]
    fn rtp_details() {
        let g722 = StreamSpec::new(Codec::G722, 0, 0);
        assert_eq!(
            (
                g722.rtp_clock_rate(),
                g722.default_payload_type(),
                g722.rtpmap()
            ),
            (8_000, 9, "G722/8000".into())
        );
        let opus = StreamSpec::new(Codec::Opus, 0, 1);
        assert_eq!(opus.rtpmap(), "opus/48000/2");
        assert!(opus.check_ptime(Duration::from_millis(20)).is_ok());
        assert!(opus.check_ptime(Duration::from_millis(1)).is_err());
    }

    #[test]
    fn opus_roundtrip_and_fec() {
        let mut enc =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Audio).unwrap();
        enc.set_inband_fec(true).unwrap();
        enc.set_packet_loss_perc(10).unwrap();
        let tone: Vec<f32> = (0..960).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let pkt = enc.encode_vec_float(&tone, 4000).unwrap();

        let mut dec = StreamSpec::new(Codec::Opus, 48_000, 1).decoder().unwrap();
        let mut out = Vec::new();
        dec.decode(&pkt, &mut out).unwrap();
        assert_eq!(out.len(), 960);
        out.clear();
        assert!(dec.conceal(2, &pkt, &mut out));
        assert_eq!(out.len(), 2 * 960);
    }
}
