//! Minimal RTP (RFC 3550) header encoding/decoding and sequence tracking.

use std::fmt;

pub const RTP_VERSION: u8 = 2;
pub const RTP_HEADER_LEN: usize = 12;

/// Fixed RTP header fields we send. CSRCs/extensions are skipped on parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

impl RtpHeader {
    /// Append the 12-byte fixed header to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.push(RTP_VERSION << 6);
        out.push(((self.marker as u8) << 7) | (self.payload_type & 0x7f));
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    pub header: RtpHeader,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpError {
    TooShort,
    BadVersion(u8),
    BadPadding,
}

impl fmt::Display for RtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtpError::TooShort => write!(f, "packet too short for RTP"),
            RtpError::BadVersion(v) => write!(f, "unsupported RTP version {v}"),
            RtpError::BadPadding => write!(f, "invalid RTP padding"),
        }
    }
}

impl std::error::Error for RtpError {}

/// Parse an RTP packet, skipping CSRCs and header extensions and removing padding.
pub fn parse(buf: &[u8]) -> Result<RtpPacket<'_>, RtpError> {
    if buf.len() < RTP_HEADER_LEN {
        return Err(RtpError::TooShort);
    }
    let version = buf[0] >> 6;
    if version != RTP_VERSION {
        return Err(RtpError::BadVersion(version));
    }
    let padding = buf[0] & 0x20 != 0;
    let extension = buf[0] & 0x10 != 0;
    let csrc_count = (buf[0] & 0x0f) as usize;

    let mut start = RTP_HEADER_LEN + 4 * csrc_count;
    if buf.len() < start {
        return Err(RtpError::TooShort);
    }
    if extension {
        if buf.len() < start + 4 {
            return Err(RtpError::TooShort);
        }
        let words = u16::from_be_bytes([buf[start + 2], buf[start + 3]]) as usize;
        start += 4 + 4 * words;
        if buf.len() < start {
            return Err(RtpError::TooShort);
        }
    }
    let mut end = buf.len();
    if padding {
        let pad = buf[end - 1] as usize;
        if pad == 0 || start + pad > end {
            return Err(RtpError::BadPadding);
        }
        end -= pad;
    }

    Ok(RtpPacket {
        header: RtpHeader {
            marker: buf[1] & 0x80 != 0,
            payload_type: buf[1] & 0x7f,
            sequence: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        },
        payload: &buf[start..end],
    })
}

/// What a newly arrived sequence number means relative to the stream so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqEvent {
    /// First packet, or a jump so large we treat it as a restarted stream.
    Reset,
    /// The packet we expected next.
    InOrder,
    /// `n` packets were skipped before this one.
    Gap(u16),
    /// Older than expected: reordered or duplicated.
    Late,
}

/// Jumps larger than this are treated as a stream restart rather than loss.
const MAX_GAP: i16 = 3000;

/// Tracks RTP sequence numbers (with wraparound) and loss statistics.
#[derive(Debug, Default, Clone)]
pub struct SequenceTracker {
    expected: Option<u16>,
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub resets: u64,
}

impl SequenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, seq: u16) -> SeqEvent {
        self.received += 1;
        let Some(expected) = self.expected else {
            self.expected = Some(seq.wrapping_add(1));
            return SeqEvent::Reset;
        };
        let diff = seq.wrapping_sub(expected) as i16;
        match diff {
            0 => {
                self.expected = Some(seq.wrapping_add(1));
                SeqEvent::InOrder
            }
            1..=MAX_GAP => {
                self.lost += diff as u64;
                self.expected = Some(seq.wrapping_add(1));
                SeqEvent::Gap(diff as u16)
            }
            d if d < 0 && d > -MAX_GAP => {
                self.late += 1;
                SeqEvent::Late
            }
            _ => {
                self.resets += 1;
                self.expected = Some(seq.wrapping_add(1));
                SeqEvent::Reset
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> RtpHeader {
        RtpHeader {
            marker: true,
            payload_type: 96,
            sequence: 0xfffe,
            timestamp: 0x1234_5678,
            ssrc: 0xdead_beef,
        }
    }

    #[test]
    fn roundtrip() {
        let mut buf = Vec::new();
        header().write_to(&mut buf);
        buf.extend_from_slice(&[1, 2, 3, 4]);
        let pkt = parse(&buf).unwrap();
        assert_eq!(pkt.header, header());
        assert_eq!(pkt.payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn skips_csrc_extension_and_padding() {
        let mut buf = Vec::new();
        header().write_to(&mut buf);
        buf[0] |= 0x20 | 0x10 | 0x01; // padding, extension, 1 CSRC
        buf.extend_from_slice(&[0; 4]); // CSRC
        buf.extend_from_slice(&[0xbe, 0xde, 0, 1, 9, 9, 9, 9]); // ext, 1 word
        buf.extend_from_slice(&[7, 7]); // payload
        buf.extend_from_slice(&[0, 0, 3]); // 3 bytes padding
        assert_eq!(parse(&buf).unwrap().payload, &[7, 7]);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse(&[0x80; 4]), Err(RtpError::TooShort));
        assert_eq!(parse(&[0x40; 12]), Err(RtpError::BadVersion(1)));
    }

    #[test]
    fn tracks_sequence_with_wrap() {
        let mut t = SequenceTracker::new();
        assert_eq!(t.update(65534), SeqEvent::Reset);
        assert_eq!(t.update(65535), SeqEvent::InOrder);
        assert_eq!(t.update(0), SeqEvent::InOrder);
        assert_eq!(t.update(3), SeqEvent::Gap(2));
        assert_eq!(t.update(1), SeqEvent::Late);
        assert_eq!(t.update(30000), SeqEvent::Reset);
        assert_eq!((t.received, t.lost, t.late, t.resets), (6, 2, 1, 1));
    }
}
