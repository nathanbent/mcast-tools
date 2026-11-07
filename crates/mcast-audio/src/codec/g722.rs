//! G.722 64 kbit/s decoder (sub-band ADPCM, ITU-T G.722).
//!
//! A port of the public-domain reference implementation (CMU 1993, as
//! adapted by Steve Underwood). Produces 16 kHz 16-bit mono.

#[derive(Default, Clone)]
struct Band {
    s: i32,
    sp: i32,
    sz: i32,
    r: [i32; 3],
    a: [i32; 3],
    ap: [i32; 3],
    p: [i32; 3],
    d: [i32; 7],
    b: [i32; 7],
    bp: [i32; 7],
    sg: [i32; 7],
    nb: i32,
    det: i32,
}

pub struct G722Decoder {
    band: [Band; 2],
    x: [i32; 24],
}

fn sat(v: i32) -> i32 {
    v.clamp(i16::MIN as i32, i16::MAX as i32)
}

const WL: [i32; 8] = [-60, -30, 58, 172, 334, 538, 1198, 3042];
const RL42: [usize; 16] = [0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0];
const ILB: [i32; 32] = [
    2048, 2093, 2139, 2186, 2233, 2282, 2332, 2383, 2435, 2489, 2543, 2599, 2656, 2714, 2774, 2834,
    2896, 2960, 3025, 3091, 3158, 3228, 3298, 3371, 3444, 3520, 3597, 3676, 3756, 3838, 3922, 4008,
];
const WH: [i32; 3] = [0, -214, 798];
const RH2: [usize; 4] = [2, 1, 2, 1];
const QM2: [i32; 4] = [-7408, -1616, 7408, 1616];
const QM4: [i32; 16] = [
    0, -20456, -12896, -8968, -6288, -4240, -2584, -1200, 20456, 12896, 8968, 6288, 4240, 2584,
    1200, 0,
];
const QM6: [i32; 64] = [
    -136, -136, -136, -136, -24808, -21904, -19008, -16704, -14984, -13512, -12280, -11192, -10232,
    -9360, -8576, -7856, -7192, -6576, -6000, -5456, -4944, -4464, -4008, -3576, -3168, -2776,
    -2400, -2032, -1688, -1360, -1040, -728, 24808, 21904, 19008, 16704, 14984, 13512, 12280,
    11192, 10232, 9360, 8576, 7856, 7192, 6576, 6000, 5456, 4944, 4464, 4008, 3576, 3168, 2776,
    2400, 2032, 1688, 1360, 1040, 728, 432, 136, -432, -136,
];
const QMF: [i32; 12] = [3, -11, 12, 32, -210, 951, 3876, -805, 362, -156, 53, -11];

impl Default for G722Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl G722Decoder {
    pub fn new() -> Self {
        let mut band: [Band; 2] = Default::default();
        band[0].det = 32;
        band[1].det = 8;
        Self { band, x: [0; 24] }
    }

    /// Decode G.722 bytes; each byte yields two 16 kHz samples.
    pub fn decode_into(&mut self, data: &[u8], mut emit: impl FnMut(i16)) {
        for &code in data {
            let code = code as i32;
            // Low band: 6 bits.
            let ilow = (code & 0x3f) as usize;
            let ihigh = ((code >> 6) & 0x03) as usize;
            let lo = &mut self.band[0];
            let rlow = (lo.s + ((lo.det * QM6[ilow]) >> 15)).clamp(-16384, 16383);
            let ilow4 = ilow >> 2;
            let dlow = (lo.det * QM4[ilow4]) >> 15;
            lo.nb = (((lo.nb * 127) >> 7) + WL[RL42[ilow4]]).clamp(0, 18432);
            lo.det = scale(lo.nb, 8);
            block4(lo, dlow);

            // High band: 2 bits.
            let hi = &mut self.band[1];
            let dhigh = (hi.det * QM2[ihigh]) >> 15;
            let rhigh = (dhigh + hi.s).clamp(-16384, 16383);
            hi.nb = (((hi.nb * 127) >> 7) + WH[RH2[ihigh]]).clamp(0, 22528);
            hi.det = scale(hi.nb, 10);
            block4(hi, dhigh);

            // Receive QMF: recombine the two bands into two output samples.
            self.x.copy_within(2.., 0);
            self.x[22] = rlow + rhigh;
            self.x[23] = rlow - rhigh;
            let mut xout1 = 0;
            let mut xout2 = 0;
            for i in 0..12 {
                xout2 += self.x[2 * i] * QMF[i];
                xout1 += self.x[2 * i + 1] * QMF[11 - i];
            }
            emit(sat(xout1 >> 11) as i16);
            emit(sat(xout2 >> 11) as i16);
        }
    }
}

/// SCALEL / SCALEH: log-domain scale factor to linear step size.
fn scale(nb: i32, shift: i32) -> i32 {
    let wd1 = ILB[((nb >> 6) & 31) as usize];
    let wd2 = shift - (nb >> 11);
    let wd3 = if wd2 < 0 { wd1 << -wd2 } else { wd1 >> wd2 };
    wd3 << 2
}

/// Block 4: reconstruct, adapt the pole/zero predictor, and predict the next sample.
fn block4(s: &mut Band, d: i32) {
    // RECONS, PARREC
    s.d[0] = d;
    s.r[0] = sat(s.s + d);
    s.p[0] = sat(s.sz + d);

    // UPPOL2
    for i in 0..3 {
        s.sg[i] = s.p[i] >> 15;
    }
    let wd1 = sat(s.a[1] << 2);
    let wd2 = if s.sg[0] == s.sg[1] { -wd1 } else { wd1 }.min(32767);
    let mut wd3 = if s.sg[0] == s.sg[2] { 128 } else { -128 };
    wd3 += wd2 >> 7;
    wd3 += (s.a[2] * 32512) >> 15;
    s.ap[2] = wd3.clamp(-12288, 12288);

    // UPPOL1
    s.sg[0] = s.p[0] >> 15;
    s.sg[1] = s.p[1] >> 15;
    let wd1 = if s.sg[0] == s.sg[1] { 192 } else { -192 };
    let wd2 = (s.a[1] * 32640) >> 15;
    let limit = sat(15360 - s.ap[2]);
    s.ap[1] = sat(wd1 + wd2).clamp(-limit, limit);

    // UPZERO
    let wd1 = if d == 0 { 0 } else { 128 };
    s.sg[0] = d >> 15;
    for i in 1..7 {
        s.sg[i] = s.d[i] >> 15;
        let wd2 = if s.sg[i] == s.sg[0] { wd1 } else { -wd1 };
        let wd3 = (s.b[i] * 32640) >> 15;
        s.bp[i] = sat(wd2 + wd3);
    }

    // DELAYA
    for i in (1..7).rev() {
        s.d[i] = s.d[i - 1];
        s.b[i] = s.bp[i];
    }
    for i in (1..3).rev() {
        s.r[i] = s.r[i - 1];
        s.p[i] = s.p[i - 1];
        s.a[i] = s.ap[i];
    }

    // FILTEP
    let wd1 = (s.a[1] * sat(s.r[1] + s.r[1])) >> 15;
    let wd2 = (s.a[2] * sat(s.r[2] + s.r[2])) >> 15;
    s.sp = sat(wd1 + wd2);

    // FILTEZ
    let mut sz = 0;
    for i in (1..7).rev() {
        sz += (s.b[i] * sat(s.d[i] + s.d[i])) >> 15;
    }
    s.sz = sat(sz);

    // PREDIC
    s.s = sat(s.sp + s.sz);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2000 bytes of G.722 from ffmpeg; ffmpeg's own decode of it hashes to this.
    #[test]
    fn bit_exact_with_ffmpeg() {
        let data = include_bytes!("../../tests/data/tone.g722");
        let mut dec = G722Decoder::new();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut n = 0;
        dec.decode_into(data, |s| {
            n += 1;
            for b in s.to_le_bytes() {
                hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
        });
        assert_eq!(n, 4000);
        assert_eq!(hash, 0x7fcb_b226_3f7e_ed61);
    }
}
