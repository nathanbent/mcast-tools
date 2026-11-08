//! Packet format. Each file ("object") travels as RTP packets whose payload is:
//!
//! ```text
//! 0      4    5          9                     21
//! +------+----+----------+---------------------+---------------------------+
//! | MCF1 |kind| objectid | RaptorQ OTI (12 B)  | body                      |
//! +------+----+----------+---------------------+---------------------------+
//! kind 0 (data): body = RaptorQ encoding packet (4-byte payload ID + symbol)
//! kind 1 (meta): body = CRC-32 (4) | name length (2) | name | type length (1) | type
//! ```
//!
//! Every packet carries the object's transmission information (OTI), so a
//! receiver can start decoding from whichever packet it hears first.

use raptorq::{EncodingPacket, ObjectTransmissionInformation};

pub const MAGIC: [u8; 4] = *b"MCF1";
pub const HEADER_LEN: usize = 21;
/// RTP payload type (dynamic) and clock for file streams.
pub const DEFAULT_PT: u8 = 110;
pub const CLOCK_RATE: u32 = 1000;
pub const ENCODING_NAME: &str = "x-mcast-file";
/// RaptorQ symbol size: header + payload ID + symbol fit a 1500-byte MTU
/// (12 RTP + 21 + 4 + 1400 = 1437 bytes of UDP payload).
pub const SYMBOL_SIZE: u16 = 1400;
/// The most symbols a source block may have (RFC 6330 K'max).
const MAX_BLOCK_SYMBOLS: u64 = 56_403;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub crc32: u32,
    pub name: String,
    /// MIME type, e.g. "image/jpeg" (may be empty).
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Data(EncodingPacket),
    Meta(Meta),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilePacket {
    pub object: u32,
    pub oti: ObjectTransmissionInformation,
    pub body: Body,
}

fn header(out: &mut Vec<u8>, kind: u8, object: u32, oti: &ObjectTransmissionInformation) {
    out.extend_from_slice(&MAGIC);
    out.push(kind);
    out.extend_from_slice(&object.to_be_bytes());
    out.extend_from_slice(&oti.serialize());
}

pub fn write_data(
    out: &mut Vec<u8>,
    object: u32,
    oti: &ObjectTransmissionInformation,
    packet: &EncodingPacket,
) {
    header(out, 0, object, oti);
    out.extend_from_slice(&packet.payload_id().serialize());
    out.extend_from_slice(packet.data());
}

pub fn write_meta(out: &mut Vec<u8>, object: u32, oti: &ObjectTransmissionInformation, m: &Meta) {
    header(out, 1, object, oti);
    out.extend_from_slice(&m.crc32.to_be_bytes());
    let name = &m.name.as_bytes()[..floor_char(&m.name, 1024)];
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name);
    let ty = &m.content_type.as_bytes()[..floor_char(&m.content_type, 255)];
    out.push(ty.len() as u8);
    out.extend_from_slice(ty);
}

fn floor_char(s: &str, max: usize) -> usize {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Why a packet was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    NotOurs,
    Truncated,
    /// Transmission parameters we won't (or can't safely) decode.
    BadParameters,
    TooLarge(u64),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::NotOurs => write!(f, "not an mcast file packet"),
            WireError::Truncated => write!(f, "truncated packet"),
            WireError::BadParameters => write!(f, "invalid transmission parameters"),
            WireError::TooLarge(n) => write!(f, "file of {n} bytes is over the size limit"),
        }
    }
}

/// Check transmission parameters from the network before trusting them.
pub fn validate(oti: &ObjectTransmissionInformation, max_size: u64) -> Result<(), WireError> {
    let len = oti.transfer_length();
    let t = oti.symbol_size() as u64;
    let (z, n, al) = (
        oti.source_blocks() as u64,
        oti.sub_blocks() as u64,
        oti.symbol_alignment() as u64,
    );
    if len > max_size {
        return Err(WireError::TooLarge(len));
    }
    let ok = len > 0
        && t > 0
        && t <= 65_000
        && z > 0
        && n > 0
        && al > 0
        && t.is_multiple_of(al)
        && n <= t / al
        && len.div_ceil(t).div_ceil(z) <= MAX_BLOCK_SYMBOLS
        && len.div_ceil(t) >= z;
    if ok {
        Ok(())
    } else {
        Err(WireError::BadParameters)
    }
}

pub fn parse(payload: &[u8], max_size: u64) -> Result<FilePacket, WireError> {
    if payload.len() < HEADER_LEN || payload[..4] != MAGIC {
        return Err(WireError::NotOurs);
    }
    let object = u32::from_be_bytes([payload[5], payload[6], payload[7], payload[8]]);
    let oti_bytes: [u8; 12] = payload[9..21].try_into().expect("12 bytes");
    let oti = ObjectTransmissionInformation::deserialize(&oti_bytes);
    validate(&oti, max_size)?;
    let body = &payload[HEADER_LEN..];
    let body = match payload[4] {
        0 => {
            if body.len() != 4 + oti.symbol_size() as usize {
                return Err(WireError::Truncated);
            }
            let packet = EncodingPacket::deserialize(body);
            // Source block numbers past the object's blocks would index out of range.
            if packet.payload_id().source_block_number() as u64 >= oti.source_blocks() as u64 {
                return Err(WireError::BadParameters);
            }
            Body::Data(packet)
        }
        1 => {
            let get = |r: std::ops::Range<usize>| body.get(r).ok_or(WireError::Truncated);
            let crc32 = u32::from_be_bytes(get(0..4)?.try_into().expect("4 bytes"));
            let name_len = u16::from_be_bytes(get(4..6)?.try_into().expect("2 bytes")) as usize;
            let name = String::from_utf8_lossy(get(6..6 + name_len)?).into_owned();
            let at = 6 + name_len;
            let ty_len = *get(at..at + 1)?.first().expect("1 byte") as usize;
            let content_type = String::from_utf8_lossy(get(at + 1..at + 1 + ty_len)?).into_owned();
            Body::Meta(Meta {
                crc32,
                name,
                content_type,
            })
        }
        _ => return Err(WireError::NotOurs),
    };
    Ok(FilePacket { object, oti, body })
}

/// Guess a MIME type from a file name (for display; receivers don't need it).
pub fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "txt" | "log" => "text/plain",
        "pdf" => "application/pdf",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raptorq::Encoder;

    #[test]
    fn data_and_meta_roundtrip() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        let enc = Encoder::with_defaults(&data, SYMBOL_SIZE);
        let oti = enc.get_config();
        let pkt = enc.get_encoded_packets(0).remove(0);
        let mut buf = Vec::new();
        write_data(&mut buf, 7, &oti, &pkt);
        assert!(12 + buf.len() <= 1472);
        let p = parse(&buf, 1 << 20).unwrap();
        assert_eq!((p.object, p.oti), (7, oti));
        assert_eq!(p.body, Body::Data(pkt));

        let meta = Meta {
            crc32: 0xdead_beef,
            name: "cat 🐈.jpg".into(),
            content_type: "image/jpeg".into(),
        };
        buf.clear();
        write_meta(&mut buf, 7, &oti, &meta);
        assert_eq!(parse(&buf, 1 << 20).unwrap().body, Body::Meta(meta));
    }

    #[test]
    fn rejects_bad_input() {
        let enc = Encoder::with_defaults(&[1u8; 3000], SYMBOL_SIZE);
        let oti = enc.get_config();
        let pkt = enc.get_encoded_packets(0).remove(0);
        let mut buf = Vec::new();
        write_data(&mut buf, 1, &oti, &pkt);
        assert_eq!(parse(&buf, 100), Err(WireError::TooLarge(3000)));
        assert_eq!(parse(&buf[..30], 1 << 20), Err(WireError::Truncated));
        assert_eq!(
            parse(b"hello there, not a file packet", 1 << 20),
            Err(WireError::NotOurs)
        );
        // Zero symbol size.
        let mut bad = buf.clone();
        bad[9 + 6] = 0;
        bad[9 + 7] = 0;
        assert_eq!(parse(&bad, 1 << 20), Err(WireError::BadParameters));
        // A source block that doesn't exist.
        let mut bad = buf.clone();
        bad[HEADER_LEN] = 9;
        assert_eq!(parse(&bad, 1 << 20), Err(WireError::BadParameters));
        // Truncated meta.
        buf.clear();
        write_meta(
            &mut buf,
            1,
            &oti,
            &Meta {
                crc32: 0,
                name: "x".into(),
                content_type: String::new(),
            },
        );
        assert_eq!(
            parse(&buf[..buf.len() - 2], 1 << 20),
            Err(WireError::Truncated)
        );
    }
}
