//! G.722 64 kbit/s encoder and decoder (sub-band ADPCM, ITU-T G.722).
//!
//! A port of the public-domain reference implementation (CMU 1993, as
//! adapted by Steve Underwood). 16 kHz 16-bit mono in and out.

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
const Q6: [i32; 32] = [
    0, 35, 72, 110, 150, 190, 233, 276, 323, 370, 422, 473, 530, 587, 650, 714, 786, 858, 940,
    1023, 1121, 1219, 1339, 1458, 1612, 1765, 1980, 2195, 2557, 2919, 0, 0,
];
const ILN: [i32; 32] = [
    0, 63, 62, 31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11,
    10, 9, 8, 7, 6, 5, 4, 0,
];
const ILP: [i32; 32] = [
    0, 61, 60, 59, 58, 57, 56, 55, 54, 53, 52, 51, 50, 49, 48, 47, 46, 45, 44, 43, 42, 41, 40, 39,
    38, 37, 36, 35, 34, 33, 32, 0,
];
const IHN: [usize; 3] = [0, 1, 0];
const IHP: [usize; 3] = [0, 3, 2];
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

pub struct G722Encoder {
    band: [Band; 2],
    x: [i32; 24],
}

impl Default for G722Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl G722Encoder {
    pub fn new() -> Self {
        let mut band: [Band; 2] = Default::default();
        band[0].det = 32;
        band[1].det = 8;
        Self { band, x: [0; 24] }
    }

    /// Encode 16 kHz samples (an even count); every two samples yield one byte.
    pub fn encode_into(&mut self, samples: &[i16], out: &mut Vec<u8>) {
        for pair in samples.chunks_exact(2) {
            // Transmit QMF: split into low and high bands at 8 kHz each.
            self.x.copy_within(2.., 0);
            self.x[22] = pair[0] as i32;
            self.x[23] = pair[1] as i32;
            let mut sumodd = 0;
            let mut sumeven = 0;
            for i in 0..12 {
                sumodd += self.x[2 * i] * QMF[i];
                sumeven += self.x[2 * i + 1] * QMF[11 - i];
            }
            let xlow = (sumeven + sumodd) >> 14;
            let xhigh = (sumeven - sumodd) >> 14;

            // Low band: SUBTRA, QUANTL (6 bits).
            let lo = &mut self.band[0];
            let el = sat(xlow - lo.s);
            let wd = if el >= 0 { el } else { -(el + 1) };
            let mut i = 1;
            while i < 30 && wd >= (Q6[i] * lo.det) >> 12 {
                i += 1;
            }
            let ilow = if el < 0 { ILN[i] } else { ILP[i] } as usize;
            let ril = ilow >> 2;
            let dlow = (lo.det * QM4[ril]) >> 15;
            lo.nb = (((lo.nb * 127) >> 7) + WL[RL42[ril]]).clamp(0, 18432);
            lo.det = scale(lo.nb, 8);
            block4(lo, dlow);

            // High band: SUBTRA, QUANTH (2 bits).
            let hi = &mut self.band[1];
            let eh = sat(xhigh - hi.s);
            let wd = if eh >= 0 { eh } else { -(eh + 1) };
            let mih = if wd >= (564 * hi.det) >> 12 { 2 } else { 1 };
            let ihigh = if eh < 0 { IHN[mih] } else { IHP[mih] };
            let dhigh = (hi.det * QM2[ihigh]) >> 15;
            hi.nb = (((hi.nb * 127) >> 7) + WH[RH2[ihigh]]).clamp(0, 22528);
            hi.det = scale(hi.nb, 10);
            block4(hi, dhigh);

            out.push(((ihigh << 6) | ilow) as u8);
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

    /// The same integer test signal ffmpeg encoded into `encoded.g722`.
    fn test_signal() -> Vec<i16> {
        let mut seed: i64 = 12345;
        (0..1600i64)
            .map(|i| {
                seed = (seed * 1_103_515_245 + 12345) & 0x7fff_ffff;
                let tri1 = ((i * 37) % 400 - 200).abs() * 60 - 6000;
                let tri2 = ((i * 211) % 160 - 80).abs() * 40 - 1600;
                (tri1 + tri2 + ((seed >> 16) % 2001 - 1000)).clamp(-32768, 32767) as i16
            })
            .collect()
    }

    #[test]
    fn encoder_bit_exact_with_ffmpeg() {
        let mut out = Vec::new();
        G722Encoder::new().encode_into(&test_signal(), &mut out);
        assert_eq!(
            out.as_slice(),
            include_bytes!("../../tests/data/encoded.g722")
        );
    }
}
