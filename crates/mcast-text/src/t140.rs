//! T.140 text and the RFC 2198 redundancy wrapper RFC 4103 uses.

/// Dynamic payload types we use by default (the values in RFC 4103's examples).
pub const DEFAULT_T140_PT: u8 = 98;
pub const DEFAULT_RED_PT: u8 = 100;
/// RFC 4103: T.140 timestamps count milliseconds.
pub const CLOCK_RATE: u32 = 1000;
/// T.140's new line.
pub const LINE_SEPARATOR: char = '\u{2028}';
/// What T.140 receivers show where text was lost.
pub const MISSING_TEXT: char = '\u{FFFD}';
/// T.140 erase-last-character.
pub const BACKSPACE: char = '\u{8}';
const BOM: char = '\u{FEFF}';

/// Largest redundancy block (10-bit length field).
pub const MAX_BLOCK: usize = 1023;
/// Largest timestamp offset a redundant block can have (14 bits).
pub const MAX_OFFSET: u32 = 0x3fff;

/// Plain text to T.140: line breaks become LINE SEPARATOR.
pub fn to_t140(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace(['\n', '\r'], &LINE_SEPARATOR.to_string())
}

/// T.140 to plain text: LINE SEPARATOR, PARAGRAPH SEPARATOR and CR LF become
/// `\n`; byte order marks (sent at the start of some sessions) are dropped.
pub fn from_t140(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace([LINE_SEPARATOR, '\u{2029}', '\r'], "\n")
        .replace(BOM, "")
}

/// One redundant generation in a RED payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedBlock<'a> {
    pub payload_type: u8,
    /// How much older than the primary block it is (RTP timestamp units).
    pub ts_offset: u32,
    pub data: &'a [u8],
}

/// Append a RED payload: `redundant` blocks (oldest first), then `primary`.
pub fn encode_red(out: &mut Vec<u8>, payload_type: u8, redundant: &[RedBlock], primary: &[u8]) {
    for b in redundant {
        debug_assert!(b.data.len() <= MAX_BLOCK && b.ts_offset <= MAX_OFFSET);
        out.push(0x80 | (b.payload_type & 0x7f));
        let word = (b.ts_offset.min(MAX_OFFSET) << 10) | b.data.len().min(MAX_BLOCK) as u32;
        out.extend_from_slice(&word.to_be_bytes()[1..]);
    }
    out.push(payload_type & 0x7f);
    for b in redundant {
        out.extend_from_slice(&b.data[..b.data.len().min(MAX_BLOCK)]);
    }
    out.extend_from_slice(primary);
}

/// A decoded RED payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Red<'a> {
    /// Oldest first.
    pub redundant: Vec<RedBlock<'a>>,
    pub primary_type: u8,
    pub primary: &'a [u8],
}

/// Split a RED payload into its blocks.
pub fn decode_red(payload: &[u8]) -> Result<Red<'_>, &'static str> {
    let mut i = 0;
    let mut headers = Vec::new();
    loop {
        let &b = payload.get(i).ok_or("RED payload ends in its headers")?;
        if b & 0x80 == 0 {
            i += 1;
            let mut redundant = Vec::with_capacity(headers.len());
            for (payload_type, ts_offset, len) in headers {
                let data = payload
                    .get(i..i + len)
                    .ok_or("RED block runs past the payload")?;
                redundant.push(RedBlock {
                    payload_type,
                    ts_offset,
                    data,
                });
                i += len;
            }
            return Ok(Red {
                redundant,
                primary_type: b & 0x7f,
                primary: &payload[i..],
            });
        }
        let h = payload
            .get(i..i + 4)
            .ok_or("RED payload ends in its headers")?;
        let word = u32::from_be_bytes([0, h[1], h[2], h[3]]);
        headers.push((b & 0x7f, word >> 10, (word & 0x3ff) as usize));
        i += 4;
    }
}

/// The longest prefix of `s` that fits in `max` bytes, at a char boundary.
pub fn split_at_most(s: &str, max: usize) -> (&str, &str) {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_breaks() {
        assert_eq!(to_t140("a\nb\r\nc"), "a\u{2028}b\u{2028}c");
        assert_eq!(from_t140("\u{FEFF}a\u{2028}b\r\nc\u{2029}"), "a\nb\nc\n");
    }

    #[test]
    fn red_roundtrip() {
        let redundant = [
            RedBlock {
                payload_type: 98,
                ts_offset: 600,
                data: b"he",
            },
            RedBlock {
                payload_type: 98,
                ts_offset: 300,
                data: b"",
            },
        ];
        let mut buf = Vec::new();
        encode_red(&mut buf, 98, &redundant, "llo ✓".as_bytes());
        assert_eq!(buf.len(), 4 + 4 + 1 + 2 + "llo ✓".len());
        let red = decode_red(&buf).unwrap();
        assert_eq!(red.redundant, redundant);
        assert_eq!(red.primary_type, 98);
        assert_eq!(red.primary, "llo ✓".as_bytes());
    }

    #[test]
    fn red_without_redundancy_and_garbage() {
        let red = decode_red(&[98, b'x']).unwrap();
        assert!(red.redundant.is_empty());
        assert_eq!(red.primary, b"x");
        assert!(decode_red(&[]).is_err());
        assert!(decode_red(&[0x80 | 98, 0, 0]).is_err());
        assert!(decode_red(&[0x80 | 98, 0, 0, 9, 98, 1]).is_err()); // block longer than payload
    }

    #[test]
    fn splits_on_char_boundaries() {
        assert_eq!(split_at_most("aé", 2), ("a", "é"));
        assert_eq!(split_at_most("ab", 5), ("ab", ""));
    }
}
