//! Codecs the receiver can decode, and how to pick one for a stream.

mod g711;
mod g722;

use crate::format::{AudioFormat, Encoding};
use std::fmt;
use std::str::FromStr;

pub use g722::G722Decoder;

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

impl fmt::Display for StreamSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.codec, self.sample_rate, self.channels)
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
        if payload.len() % self.0.frame_bytes() != 0 {
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
