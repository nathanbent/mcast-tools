//! G.711 (ITU-T) mu-law and A-law companding to and from 16-bit linear PCM.

pub fn ulaw_to_linear(u: u8) -> i16 {
    let u = !u;
    let exponent = (u >> 4) & 0x07;
    let mantissa = (u & 0x0f) as i16;
    let magnitude = (((mantissa << 3) + 0x84) << exponent) - 0x84;
    if u & 0x80 != 0 {
        -magnitude
    } else {
        magnitude
    }
}

pub fn alaw_to_linear(a: u8) -> i16 {
    let a = a ^ 0x55;
    let segment = (a & 0x70) >> 4;
    let mut t = ((a & 0x0f) as i16) << 4;
    match segment {
        0 => t += 8,
        1 => t += 0x108,
        s => t = (t + 0x108) << (s - 1),
    }
    if a & 0x80 != 0 {
        t
    } else {
        -t
    }
}

/// Encode tables indexed by the 14-bit sample, built from the decoders so each
/// input maps to the nearest G.711 level (midpoint decision thresholds).
/// This matches ffmpeg's `pcm_mulaw`/`pcm_alaw` encoders byte for byte.
fn build_table(decode: fn(u8) -> i16, mask: u8) -> Box<[u8; 16384]> {
    let mut t = Box::new([0u8; 16384]);
    t[8192] = mask;
    let mut j = 1usize;
    for i in 0..127u8 {
        let v1 = decode(i ^ mask) as i32;
        let v2 = decode((i + 1) ^ mask) as i32;
        let v = ((v1 + v2 + 4) >> 3) as usize;
        while j < v {
            t[8192 - j] = i ^ (mask ^ 0x80);
            t[8192 + j] = i ^ mask;
            j += 1;
        }
    }
    while j < 8192 {
        t[8192 - j] = 127 ^ (mask ^ 0x80);
        t[8192 + j] = 127 ^ mask;
        j += 1;
    }
    t[0] = t[1];
    t
}

fn index(pcm: i16) -> usize {
    ((pcm as i32 + 32768) >> 2) as usize
}

pub fn linear_to_ulaw(pcm: i16) -> u8 {
    static TABLE: std::sync::OnceLock<Box<[u8; 16384]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| build_table(ulaw_to_linear, 0xff))[index(pcm)]
}

pub fn linear_to_alaw(pcm: i16) -> u8 {
    static TABLE: std::sync::OnceLock<Box<[u8; 16384]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| build_table(alaw_to_linear, 0xd5))[index(pcm)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        // Silence and full scale per the G.711 tables.
        assert_eq!(ulaw_to_linear(0xff), 0);
        assert_eq!(ulaw_to_linear(0x7f), 0);
        assert_eq!(ulaw_to_linear(0x80), 32124);
        assert_eq!(ulaw_to_linear(0x00), -32124);
        assert_eq!(alaw_to_linear(0xd5), 8);
        assert_eq!(alaw_to_linear(0x55), -8);
        assert_eq!(alaw_to_linear(0xaa), 32256);
        assert_eq!(alaw_to_linear(0x2a), -32256);
    }

    #[test]
    fn encode_decode_roundtrip_every_code() {
        for code in 0..=255u8 {
            let u = ulaw_to_linear(code);
            // 0x7f and 0xff both mean zero; the encoder picks 0xff.
            let want = if code == 0x7f { 0xff } else { code };
            assert_eq!(linear_to_ulaw(u), want, "ulaw {code:#x}");
            assert_eq!(linear_to_alaw(alaw_to_linear(code)), code, "alaw {code:#x}");
        }
        assert_eq!(linear_to_ulaw(i16::MIN), 0x00);
        assert_eq!(linear_to_alaw(i16::MAX), 0xaa);
    }

    fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
        bytes.fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// ffmpeg's pcm_mulaw/pcm_alaw encoding of every i16 value hashes to these.
    #[test]
    fn encoders_bit_exact_with_ffmpeg() {
        assert_eq!(
            fnv((i16::MIN..=i16::MAX).map(linear_to_ulaw)),
            0xa733_78df_9212_a731
        );
        assert_eq!(
            fnv((i16::MIN..=i16::MAX).map(linear_to_alaw)),
            0xb692_762f_b973_cb11
        );
    }

    #[test]
    fn monotonic() {
        // Positive mu-law codes 0xff..=0x80 increase in magnitude.
        let mut prev = -1;
        for code in (0x80..=0xffu8).rev() {
            let v = ulaw_to_linear(code) as i32;
            assert!(v > prev || (v == 0 && prev == -1), "{code:#x}");
            prev = v;
        }
    }
}
