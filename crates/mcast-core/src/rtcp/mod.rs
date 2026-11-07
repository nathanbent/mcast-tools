//! RTCP (RFC 3550 section 6): the control channel that travels beside an RTP
//! stream, conventionally on the RTP port + 1.
//!
//! Senders send Sender Reports (SR), which map the stream's RTP timestamps to
//! wall-clock (NTP) time, plus SDES (who they are) and BYE (stream ended).
//! Receivers send Receiver Reports (RR) with loss and jitter. Together they
//! give a heartbeat in both directions and the timing data needed to measure
//! end-to-end delay and keep receivers in step.
//!
//! mcast-tools receivers also send an APP packet named `MCST` (see
//! [`Playout`]) carrying how far behind the sender they play, so a monitor
//! can show whether receivers are in sync with each other.

mod session;

pub use session::{rtcp_options, Listener, Received, ReceiverRtcp, RtcpConfig, SenderRtcp};

use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PT_SR: u8 = 200;
pub const PT_RR: u8 = 201;
pub const PT_SDES: u8 = 202;
pub const PT_BYE: u8 = 203;
pub const PT_APP: u8 = 204;

pub const SDES_CNAME: u8 = 1;
pub const SDES_NAME: u8 = 2;
pub const SDES_TOOL: u8 = 6;

/// Seconds from the NTP epoch (1900) to the Unix epoch (1970).
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// The RTCP address that goes with an RTP group: same address, port + 1.
pub fn rtcp_addr(rtp: SocketAddr) -> Option<SocketAddr> {
    let port = rtp.port().checked_add(1)?;
    Some(SocketAddr::new(rtp.ip(), port))
}

/// 64-bit NTP timestamp (32.32 fixed point seconds since 1900) for `t`.
pub fn ntp_from_system(t: SystemTime) -> u64 {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() + NTP_UNIX_OFFSET;
    let frac = ((d.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

/// The current wall-clock time as an NTP timestamp.
pub fn ntp_now() -> u64 {
    ntp_from_system(SystemTime::now())
}

/// `a - b` in seconds (wraps correctly; good for differences under 68 years).
pub fn ntp_diff_secs(a: u64, b: u64) -> f64 {
    a.wrapping_sub(b) as i64 as f64 / 4_294_967_296.0
}

/// `ntp` moved by `secs` (which may be negative).
pub fn ntp_add_secs(ntp: u64, secs: f64) -> u64 {
    ntp.wrapping_add((secs * 4_294_967_296.0).round() as i64 as u64)
}

/// The middle 32 bits of an NTP timestamp, as used in LSR/DLSR fields.
pub fn ntp_middle(ntp: u64) -> u32 {
    (ntp >> 16) as u32
}

/// A duration in the 1/65536-second units of the DLSR field.
pub fn dlsr_units(d: Duration) -> u32 {
    (d.as_secs_f64() * 65_536.0).min(u32::MAX as f64) as u32
}

/// One reception report block (inside an SR or RR).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportBlock {
    /// The source this block is about.
    pub ssrc: u32,
    /// Lost since the previous report, as a fraction of 256.
    pub fraction_lost: u8,
    /// Total lost (24-bit signed; duplicates can make it negative).
    pub cumulative_lost: i32,
    /// Extended highest sequence number received.
    pub highest_seq: u32,
    /// Interarrival jitter, in RTP timestamp units.
    pub jitter: u32,
    /// Middle 32 bits of the last SR's NTP time (0 if none yet).
    pub last_sr: u32,
    /// Time since that SR arrived, in 1/65536 s.
    pub delay_since_last_sr: u32,
}

impl ReportBlock {
    pub fn loss_fraction(&self) -> f64 {
        self.fraction_lost as f64 / 256.0
    }
}

/// The sender information in an SR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SenderInfo {
    /// Wall-clock time this report was made.
    pub ntp: u64,
    /// The RTP timestamp that corresponds to `ntp`.
    pub rtp_timestamp: u32,
    pub packets: u32,
    /// Payload bytes sent (excluding headers).
    pub octets: u32,
}

/// One SDES chunk: items describing a source.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SdesChunk {
    pub ssrc: u32,
    pub items: Vec<(u8, String)>,
}

impl SdesChunk {
    pub fn item(&self, kind: u8) -> Option<&str> {
        self.items
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    SenderReport {
        ssrc: u32,
        info: SenderInfo,
        reports: Vec<ReportBlock>,
    },
    ReceiverReport {
        ssrc: u32,
        reports: Vec<ReportBlock>,
    },
    Sdes(Vec<SdesChunk>),
    Bye {
        ssrcs: Vec<u32>,
        reason: Option<String>,
    },
    App {
        subtype: u8,
        ssrc: u32,
        name: [u8; 4],
        data: Vec<u8>,
    },
    /// A packet type we don't interpret (e.g. RTPFB/PSFB feedback).
    Other {
        packet_type: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtcpError {
    TooShort,
    BadVersion(u8),
    BadLength,
    BadPadding,
}

impl fmt::Display for RtcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtcpError::TooShort => write!(f, "packet too short for RTCP"),
            RtcpError::BadVersion(v) => write!(f, "unsupported RTCP version {v}"),
            RtcpError::BadLength => write!(f, "RTCP length field doesn't match the packet"),
            RtcpError::BadPadding => write!(f, "invalid RTCP padding"),
        }
    }
}

impl std::error::Error for RtcpError {}

/// Write the common header, returning its offset so the length can be patched.
fn begin(out: &mut Vec<u8>, count: u8, packet_type: u8) -> usize {
    let at = out.len();
    out.push((2 << 6) | (count & 0x1f));
    out.push(packet_type);
    out.extend_from_slice(&[0, 0]);
    at
}

/// Pad to a 32-bit boundary and fill in the length field.
fn end(out: &mut Vec<u8>, at: usize) {
    while (out.len() - at) % 4 != 0 {
        out.push(0);
    }
    let words = ((out.len() - at) / 4 - 1) as u16;
    out[at + 2..at + 4].copy_from_slice(&words.to_be_bytes());
}

fn write_blocks(out: &mut Vec<u8>, reports: &[ReportBlock]) {
    for r in reports.iter().take(31) {
        out.extend_from_slice(&r.ssrc.to_be_bytes());
        let lost = (r.cumulative_lost.clamp(-0x80_0000, 0x7f_ffff) as u32) & 0xff_ffff;
        out.extend_from_slice(&(((r.fraction_lost as u32) << 24) | lost).to_be_bytes());
        out.extend_from_slice(&r.highest_seq.to_be_bytes());
        out.extend_from_slice(&r.jitter.to_be_bytes());
        out.extend_from_slice(&r.last_sr.to_be_bytes());
        out.extend_from_slice(&r.delay_since_last_sr.to_be_bytes());
    }
}

impl Packet {
    /// Append this packet to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            Packet::SenderReport {
                ssrc,
                info,
                reports,
            } => {
                let at = begin(out, reports.len().min(31) as u8, PT_SR);
                out.extend_from_slice(&ssrc.to_be_bytes());
                out.extend_from_slice(&info.ntp.to_be_bytes());
                out.extend_from_slice(&info.rtp_timestamp.to_be_bytes());
                out.extend_from_slice(&info.packets.to_be_bytes());
                out.extend_from_slice(&info.octets.to_be_bytes());
                write_blocks(out, reports);
                end(out, at);
            }
            Packet::ReceiverReport { ssrc, reports } => {
                let at = begin(out, reports.len().min(31) as u8, PT_RR);
                out.extend_from_slice(&ssrc.to_be_bytes());
                write_blocks(out, reports);
                end(out, at);
            }
            Packet::Sdes(chunks) => {
                let at = begin(out, chunks.len().min(31) as u8, PT_SDES);
                for c in chunks.iter().take(31) {
                    let start = out.len();
                    out.extend_from_slice(&c.ssrc.to_be_bytes());
                    for (kind, text) in &c.items {
                        let bytes = truncate_utf8(text, 255);
                        out.push(*kind);
                        out.push(bytes.len() as u8);
                        out.extend_from_slice(bytes);
                    }
                    // End of items: at least one null octet, then pad the chunk.
                    out.push(0);
                    while (out.len() - start) % 4 != 0 {
                        out.push(0);
                    }
                }
                end(out, at);
            }
            Packet::Bye { ssrcs, reason } => {
                let at = begin(out, ssrcs.len().min(31) as u8, PT_BYE);
                for s in ssrcs.iter().take(31) {
                    out.extend_from_slice(&s.to_be_bytes());
                }
                if let Some(r) = reason {
                    let bytes = truncate_utf8(r, 255);
                    out.push(bytes.len() as u8);
                    out.extend_from_slice(bytes);
                }
                end(out, at);
            }
            Packet::App {
                subtype,
                ssrc,
                name,
                data,
            } => {
                let at = begin(out, *subtype, PT_APP);
                out.extend_from_slice(&ssrc.to_be_bytes());
                out.extend_from_slice(name);
                out.extend_from_slice(data);
                end(out, at);
            }
            Packet::Other { .. } => {}
        }
    }
}

fn truncate_utf8(s: &str, max: usize) -> &[u8] {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s.as_bytes()[..end]
}

/// Encode a compound RTCP packet.
pub fn encode(packets: &[Packet]) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    for p in packets {
        p.write_to(&mut out);
    }
    out
}

/// True if `buf` starts like an RTCP packet (version 2, type SR..APP).
pub fn looks_like_rtcp(buf: &[u8]) -> bool {
    buf.len() >= 8 && buf[0] >> 6 == 2 && (PT_SR..=PT_APP).contains(&buf[1])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn parse_blocks(body: &[u8], count: usize) -> Result<Vec<ReportBlock>, RtcpError> {
    if body.len() < count * 24 {
        return Err(RtcpError::TooShort);
    }
    Ok((0..count)
        .map(|i| {
            let b = &body[i * 24..];
            let word = u32_at(b, 4);
            // Sign-extend the 24-bit cumulative loss.
            let lost = ((word << 8) as i32) >> 8;
            ReportBlock {
                ssrc: u32_at(b, 0),
                fraction_lost: (word >> 24) as u8,
                cumulative_lost: lost,
                highest_seq: u32_at(b, 8),
                jitter: u32_at(b, 12),
                last_sr: u32_at(b, 16),
                delay_since_last_sr: u32_at(b, 20),
            }
        })
        .collect())
}

fn parse_sdes(body: &[u8], count: usize) -> Result<Vec<SdesChunk>, RtcpError> {
    let mut chunks = Vec::with_capacity(count);
    let mut i = 0;
    for _ in 0..count {
        if i + 4 > body.len() {
            return Err(RtcpError::TooShort);
        }
        let start = i;
        let mut chunk = SdesChunk {
            ssrc: u32_at(body, i),
            items: Vec::new(),
        };
        i += 4;
        loop {
            let Some(&kind) = body.get(i) else {
                return Err(RtcpError::TooShort);
            };
            if kind == 0 {
                i += 1;
                break;
            }
            let len = *body.get(i + 1).ok_or(RtcpError::TooShort)? as usize;
            let text = body.get(i + 2..i + 2 + len).ok_or(RtcpError::TooShort)?;
            chunk
                .items
                .push((kind, String::from_utf8_lossy(text).into_owned()));
            i += 2 + len;
        }
        // Chunks end on a 32-bit boundary.
        i = start + (i - start).div_ceil(4) * 4;
        chunks.push(chunk);
    }
    Ok(chunks)
}

/// Parse a (compound) RTCP packet.
pub fn parse(buf: &[u8]) -> Result<Vec<Packet>, RtcpError> {
    let mut packets = Vec::new();
    let mut rest = buf;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err(RtcpError::TooShort);
        }
        let version = rest[0] >> 6;
        if version != 2 {
            return Err(RtcpError::BadVersion(version));
        }
        let padding = rest[0] & 0x20 != 0;
        let count = (rest[0] & 0x1f) as usize;
        let packet_type = rest[1];
        let len = (u16::from_be_bytes([rest[2], rest[3]]) as usize + 1) * 4;
        if len > rest.len() {
            return Err(RtcpError::BadLength);
        }
        let mut body = &rest[4..len];
        if padding {
            let pad = *body.last().ok_or(RtcpError::BadPadding)? as usize;
            if pad == 0 || pad > body.len() {
                return Err(RtcpError::BadPadding);
            }
            body = &body[..body.len() - pad];
        }
        let need = |n: usize| {
            if body.len() < n {
                Err(RtcpError::TooShort)
            } else {
                Ok(())
            }
        };
        let packet = match packet_type {
            PT_SR => {
                need(24)?;
                Packet::SenderReport {
                    ssrc: u32_at(body, 0),
                    info: SenderInfo {
                        ntp: ((u32_at(body, 4) as u64) << 32) | u32_at(body, 8) as u64,
                        rtp_timestamp: u32_at(body, 12),
                        packets: u32_at(body, 16),
                        octets: u32_at(body, 20),
                    },
                    reports: parse_blocks(&body[24..], count)?,
                }
            }
            PT_RR => {
                need(4)?;
                Packet::ReceiverReport {
                    ssrc: u32_at(body, 0),
                    reports: parse_blocks(&body[4..], count)?,
                }
            }
            PT_SDES => Packet::Sdes(parse_sdes(body, count)?),
            PT_BYE => {
                need(count * 4)?;
                let ssrcs = (0..count).map(|i| u32_at(body, i * 4)).collect();
                let tail = &body[count * 4..];
                let reason = tail.first().and_then(|&n| {
                    tail.get(1..1 + n as usize)
                        .map(|t| String::from_utf8_lossy(t).into_owned())
                });
                Packet::Bye { ssrcs, reason }
            }
            PT_APP => {
                need(8)?;
                Packet::App {
                    subtype: count as u8,
                    ssrc: u32_at(body, 0),
                    name: [body[4], body[5], body[6], body[7]],
                    data: body[8..].to_vec(),
                }
            }
            other => Packet::Other { packet_type: other },
        };
        packets.push(packet);
        rest = &rest[len..];
    }
    if packets.is_empty() {
        return Err(RtcpError::TooShort);
    }
    Ok(packets)
}

/// The SDES chunk for `ssrc` in a compound packet, if any.
pub fn sdes_for(packets: &[Packet], ssrc: u32) -> Option<&SdesChunk> {
    packets.iter().find_map(|p| match p {
        Packet::Sdes(chunks) => chunks.iter().find(|c| c.ssrc == ssrc),
        _ => None,
    })
}

/// The APP name mcast-tools receivers use for [`Playout`].
pub const PLAYOUT_APP: [u8; 4] = *b"MCST";

/// How far behind the sender a receiver plays (mcast-tools APP packet).
///
/// `transit` is how long packets take from the sender's clock to arrival
/// (needs the two hosts' clocks in sync, e.g. NTP or PTP); `buffered` is the
/// audio queued in the receiver's jitter buffer. Their sum is roughly when a
/// sample sent "now" comes out of the speaker (plus the sound card's own
/// latency, which we can't see).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playout {
    /// The media source this is about.
    pub media_ssrc: u32,
    /// Seconds; negative when the clocks disagree, `None` before the first SR.
    pub transit_secs: Option<f64>,
    pub buffered: Duration,
}

impl Playout {
    pub fn new(media_ssrc: u32, transit_secs: Option<f64>, buffered: Duration) -> Self {
        Self {
            media_ssrc,
            transit_secs,
            buffered,
        }
    }

    /// Transit plus buffered audio, in seconds.
    pub fn latency_secs(&self) -> Option<f64> {
        self.transit_secs.map(|t| t + self.buffered.as_secs_f64())
    }

    pub fn to_app(&self, ssrc: u32) -> Packet {
        let mut data = Vec::with_capacity(12);
        data.extend_from_slice(&self.media_ssrc.to_be_bytes());
        let transit_us = self
            .transit_secs
            .map_or(i32::MIN, |t| (t * 1e6).round().clamp(-2e9, 2e9) as i32);
        data.extend_from_slice(&transit_us.to_be_bytes());
        let buffered_us = self.buffered.as_micros().min(u32::MAX as u128) as u32;
        data.extend_from_slice(&buffered_us.to_be_bytes());
        Packet::App {
            subtype: 0,
            ssrc,
            name: PLAYOUT_APP,
            data,
        }
    }

    pub fn from_app(packet: &Packet) -> Option<(u32, Self)> {
        let Packet::App {
            subtype: 0,
            ssrc,
            name,
            data,
        } = packet
        else {
            return None;
        };
        if *name != PLAYOUT_APP || data.len() < 12 {
            return None;
        }
        let transit_us = u32_at(data, 4) as i32;
        let transit = (transit_us != i32::MIN).then_some(transit_us as f64 / 1e6);
        let buffered = Duration::from_micros(u32_at(data, 8) as u64);
        Some((*ssrc, Self::new(u32_at(data, 0), transit, buffered)))
    }
}

/// Reception statistics for one source, for building report blocks
/// (RFC 3550 appendix A.1, A.3 and A.8).
#[derive(Debug, Clone, Default)]
pub struct ReceptionStats {
    /// Extended highest sequence number seen.
    max_ext: Option<u32>,
    base_ext: u32,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
    /// In RTP timestamp units.
    jitter: f64,
    last_transit: Option<f64>,
}

impl ReceptionStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a packet. `arrival` is the arrival time in RTP timestamp units
    /// on any fixed local timeline (only differences matter).
    pub fn packet(&mut self, seq: u16, timestamp: u32, arrival: f64) {
        self.received = self.received.wrapping_add(1);
        match self.max_ext {
            None => {
                self.max_ext = Some(seq as u32);
                self.base_ext = seq as u32;
            }
            Some(max) => {
                let delta = seq.wrapping_sub(max as u16) as i16;
                if delta > 0 {
                    self.max_ext = Some(max.wrapping_add(delta as u32));
                } else if delta < -3000 {
                    // Restarted with new numbering: start counting afresh.
                    *self = Self::default();
                    self.packet(seq, timestamp, arrival);
                    return;
                }
            }
        }
        let transit = arrival - timestamp as f64;
        if let Some(prev) = self.last_transit {
            // Timestamps wrap at 2^32; differences beyond half that are wraps.
            let mut d = (transit - prev).abs();
            if d > 2_147_483_648.0 {
                d = (d - 4_294_967_296.0).abs();
            }
            self.jitter += (d - self.jitter) / 16.0;
        }
        self.last_transit = Some(transit);
    }

    /// Interarrival jitter in RTP timestamp units.
    pub fn jitter(&self) -> f64 {
        self.jitter
    }

    /// Build a report block for `ssrc` and start a new reporting interval.
    pub fn report(&mut self, ssrc: u32, last_sr: Option<(u64, Duration)>) -> Option<ReportBlock> {
        let max = self.max_ext?;
        let expected = max.wrapping_sub(self.base_ext).wrapping_add(1);
        let lost = expected as i64 - self.received as i64;
        let expected_interval = expected.wrapping_sub(self.expected_prior);
        let received_interval = self.received.wrapping_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_interval = expected_interval as i64 - received_interval as i64;
        let fraction_lost = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / expected_interval as i64).min(255) as u8
        };
        let (last_sr, delay_since_last_sr) =
            last_sr.map_or((0, 0), |(ntp, since)| (ntp_middle(ntp), dlsr_units(since)));
        Some(ReportBlock {
            ssrc,
            fraction_lost,
            cumulative_lost: lost.clamp(-0x80_0000, 0x7f_ffff) as i32,
            highest_seq: max,
            jitter: self.jitter as u32,
            last_sr,
            delay_since_last_sr,
        })
    }
}

/// Maps a sender's RTP timestamps to its wall clock, from its last SR.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockMapping {
    pub ntp: u64,
    pub rtp_timestamp: u32,
    pub clock_rate: f64,
}

impl ClockMapping {
    /// The sender's wall-clock time for an RTP timestamp near the SR's.
    pub fn ntp_for(&self, timestamp: u32) -> u64 {
        let delta = timestamp.wrapping_sub(self.rtp_timestamp) as i32 as f64;
        ntp_add_secs(self.ntp, delta / self.clock_rate)
    }
}

/// A randomised RTCP reporting interval (RFC 3550 section 6.3.1: 0.5-1.5x
/// the nominal interval, so participants don't synchronise).
pub fn jittered(interval: Duration) -> Duration {
    let r = crate::random_u32() as f64 / u32::MAX as f64;
    interval.mul_f64(0.5 + r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntp_conversions() {
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_250);
        let ntp = ntp_from_system(t);
        assert_eq!(ntp >> 32, 1_700_000_000 + NTP_UNIX_OFFSET);
        assert_eq!(ntp as u32, 1 << 30); // 0.25 s
        assert!(
            (ntp_diff_secs(ntp, ntp_from_system(t - Duration::from_millis(1500))) - 1.5).abs()
                < 1e-9
        );
        assert!((ntp_diff_secs(ntp_add_secs(ntp, -0.125), ntp) + 0.125).abs() < 1e-9);
        assert_eq!(ntp_middle(0x0001_2345_6789_0000), 0x2345_6789);
    }

    #[test]
    fn rtcp_port() {
        assert_eq!(
            rtcp_addr("239.1.2.3:5004".parse().unwrap()),
            Some("239.1.2.3:5005".parse().unwrap())
        );
        assert_eq!(rtcp_addr("239.1.2.3:65535".parse().unwrap()), None);
    }

    fn block() -> ReportBlock {
        ReportBlock {
            ssrc: 0x1122_3344,
            fraction_lost: 25,
            cumulative_lost: -3,
            highest_seq: 0x0001_0005,
            jitter: 77,
            last_sr: 0xabcd_ef01,
            delay_since_last_sr: 65_536,
        }
    }

    #[test]
    fn compound_roundtrip() {
        let packets = vec![
            Packet::SenderReport {
                ssrc: 0xdead_beef,
                info: SenderInfo {
                    ntp: 0x0102_0304_0506_0708,
                    rtp_timestamp: 160,
                    packets: 50,
                    octets: 8000,
                },
                reports: vec![block()],
            },
            Packet::Sdes(vec![SdesChunk {
                ssrc: 0xdead_beef,
                items: vec![
                    (SDES_CNAME, "mcast@10.0.0.1".into()),
                    (SDES_NAME, "Lobby".into()),
                ],
            }]),
            Packet::ReceiverReport {
                ssrc: 7,
                reports: vec![],
            },
            Packet::Bye {
                ssrcs: vec![0xdead_beef],
                reason: Some("done".into()),
            },
            Playout::new(0xdead_beef, Some(0.0123), Duration::from_millis(40)).to_app(7),
        ];
        let buf = encode(&packets);
        assert_eq!(buf.len() % 4, 0);
        assert!(looks_like_rtcp(&buf));
        let parsed = parse(&buf).unwrap();
        assert_eq!(parsed, packets);
        assert_eq!(
            sdes_for(&parsed, 0xdead_beef).unwrap().item(SDES_NAME),
            Some("Lobby")
        );
        let (from, p) = Playout::from_app(&parsed[4]).unwrap();
        assert_eq!((from, p.media_ssrc), (7, 0xdead_beef));
        assert!((p.latency_secs().unwrap() - 0.0523).abs() < 1e-6);
    }

    #[test]
    fn sdes_chunk_padding() {
        // Item lengths that end exactly on, and just short of, a word boundary.
        for name in ["", "a", "ab", "abc", "abcd", "abcde"] {
            let p = Packet::Sdes(vec![
                SdesChunk {
                    ssrc: 1,
                    items: vec![(SDES_CNAME, name.into())],
                },
                SdesChunk {
                    ssrc: 2,
                    items: vec![(SDES_CNAME, "x".into())],
                },
            ]);
            let buf = encode(std::slice::from_ref(&p));
            assert_eq!(parse(&buf).unwrap(), vec![p], "{name:?}");
        }
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse(&[0x80, 200]), Err(RtcpError::TooShort));
        assert_eq!(parse(&[0x40, 200, 0, 0]), Err(RtcpError::BadVersion(1)));
        assert_eq!(
            parse(&[0x80, 200, 0, 9, 0, 0, 0, 0]),
            Err(RtcpError::BadLength)
        );
        assert_eq!(parse(&[0x80, 200, 0, 0]), Err(RtcpError::TooShort)); // SR with no body
        assert!(!looks_like_rtcp(&[0x80, 0, 0, 1, 0, 0, 0, 0]));
    }

    #[test]
    fn reception_stats_loss_and_jitter() {
        let mut s = ReceptionStats::new();
        // 8 kHz, 160-sample packets; drop seq 65535+3 (wraps), arrivals jitter by 40 units.
        let mut n = 0u32;
        for i in 0..100u32 {
            let seq = 65_530u16.wrapping_add(i as u16);
            if i == 8 {
                continue;
            }
            let wobble = if i % 2 == 0 { 0.0 } else { 40.0 };
            s.packet(seq, i * 160, (i * 160) as f64 + wobble);
            n += 1;
        }
        assert_eq!(n, 99);
        let r = s.report(5, None).unwrap();
        assert_eq!(r.cumulative_lost, 1);
        assert_eq!(r.highest_seq, 65_530 + 99);
        assert_eq!(r.fraction_lost, (256 / 100) as u8);
        assert!((r.jitter as f64 - 40.0).abs() < 3.0, "{}", r.jitter);
        // Nothing lost in the next interval.
        s.packet(65_530u16.wrapping_add(100), 100 * 160, 16_000.0);
        assert_eq!(s.report(5, None).unwrap().fraction_lost, 0);
    }

    #[test]
    fn clock_mapping() {
        let m = ClockMapping {
            ntp: 100 << 32,
            rtp_timestamp: u32::MAX - 3999,
            clock_rate: 8000.0,
        };
        // 8000 ticks later (across the wrap) is one second later.
        assert!((ntp_diff_secs(m.ntp_for(4000), m.ntp) - 1.0).abs() < 1e-9);
        assert!((ntp_diff_secs(m.ntp_for(u32::MAX - 7999), m.ntp) + 0.5).abs() < 1e-9);
    }
}
