//! PCM stream formats and sample encode/decode.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// 16-bit signed big-endian PCM.
    #[default]
    L16,
    /// 24-bit signed big-endian PCM.
    L24,
}

impl Encoding {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Encoding::L16 => 2,
            Encoding::L24 => 3,
        }
    }

    pub fn bits(self) -> u16 {
        self.bytes_per_sample() as u16 * 8
    }
}

impl FromStr for Encoding {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "l16" => Ok(Encoding::L16),
            "l24" => Ok(Encoding::L24),
            _ => Err(format!("unknown encoding '{s}' (expected l16 or l24)")),
        }
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Encoding::L16 => "L16",
            Encoding::L24 => "L24",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub encoding: Encoding,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            encoding: Encoding::L16,
        }
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.encoding, self.sample_rate, self.channels
        )
    }
}

impl AudioFormat {
    /// Bytes for one frame (one sample per channel).
    pub fn frame_bytes(&self) -> usize {
        self.encoding.bytes_per_sample() * self.channels as usize
    }

    /// Frames in one packet of the given duration (at least 1).
    pub fn frames_for(&self, ptime: Duration) -> usize {
        ((self.sample_rate as f64 * ptime.as_secs_f64()).round() as usize).max(1)
    }

    pub fn duration_of(&self, frames: usize) -> Duration {
        Duration::from_secs_f64(frames as f64 / self.sample_rate as f64)
    }

    /// Static RTP payload type from RFC 3551 if one exists, else dynamic 96.
    pub fn default_payload_type(&self) -> u8 {
        match (self.encoding, self.sample_rate, self.channels) {
            (Encoding::L16, 44_100, 2) => 10,
            (Encoding::L16, 44_100, 1) => 11,
            _ => 96,
        }
    }

    /// Append interleaved `samples` (in -1.0..=1.0) to `out` as big-endian PCM.
    pub fn encode(&self, samples: &[f32], out: &mut Vec<u8>) {
        out.reserve(samples.len() * self.encoding.bytes_per_sample());
        match self.encoding {
            Encoding::L16 => {
                for &s in samples {
                    let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            Encoding::L24 => {
                const MAX: f32 = 8_388_607.0;
                for &s in samples {
                    let v = (s.clamp(-1.0, 1.0) * MAX).round() as i32;
                    out.extend_from_slice(&v.to_be_bytes()[1..]);
                }
            }
        }
    }

    /// Append decoded samples to `out`. Trailing partial samples are ignored.
    pub fn decode(&self, bytes: &[u8], out: &mut Vec<f32>) {
        match self.encoding {
            Encoding::L16 => out.extend(
                bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_be_bytes([b[0], b[1]]) as f32 / i16::MAX as f32),
            ),
            Encoding::L24 => out.extend(bytes.as_chunks::<3>().0.iter().map(|b| {
                // Place in the top 24 bits, then arithmetic-shift to sign-extend.
                let v = i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8;
                v as f32 / 8_388_607.0
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(encoding: Encoding, tolerance: f32) {
        let fmt = AudioFormat {
            encoding,
            ..Default::default()
        };
        let input = [0.0, 0.5, -0.5, 1.0, -1.0, 0.123_456, -0.999];
        let mut bytes = Vec::new();
        fmt.encode(&input, &mut bytes);
        assert_eq!(bytes.len(), input.len() * encoding.bytes_per_sample());
        let mut out = Vec::new();
        fmt.decode(&bytes, &mut out);
        for (a, b) in input.iter().zip(&out) {
            assert!((a - b).abs() <= tolerance, "{a} vs {b}");
        }
    }

    #[test]
    fn l16_roundtrip() {
        roundtrip(Encoding::L16, 1.0 / 32_000.0);
    }

    #[test]
    fn l24_roundtrip() {
        roundtrip(Encoding::L24, 1.0 / 8_000_000.0);
    }

    #[test]
    fn packet_sizes() {
        let fmt = AudioFormat::default();
        assert_eq!(fmt.frames_for(Duration::from_millis(1)), 48);
        assert_eq!(fmt.frame_bytes(), 4);
        let cd = AudioFormat {
            sample_rate: 44_100,
            ..fmt
        };
        assert_eq!(cd.frames_for(Duration::from_millis(1)), 44);
        assert_eq!(cd.default_payload_type(), 10);
    }
}
