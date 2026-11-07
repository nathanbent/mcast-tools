//! G.711 (ITU-T) mu-law and A-law expansion to 16-bit linear PCM.

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
