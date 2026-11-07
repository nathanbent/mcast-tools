//! Per-source statistics.

use mcast_audio::codec::Decoder;
use mcast_audio::StreamSpec;
use mcast_core::rtp::{self, SeqEvent, SequenceTracker};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct StreamKey {
    pub group: usize,
    pub source: SocketAddr,
}

/// Packets in a row with one SSRC and consecutive sequence numbers needed
/// before traffic counts as RTP (random data parses as RTP surprisingly often).
const RTP_CONFIRM: u32 = 3;

/// One row of monitor output.
#[derive(Debug, Clone)]
pub struct StreamReport {
    pub group: SocketAddr,
    pub name: Option<String>,
    pub source: SocketAddr,
    pub ssrc: Option<u32>,
    pub payload_type: Option<u8>,
    /// Codec, e.g. "PCMU/8000/1", when known.
    pub codec: Option<String>,
    pub packets: u64,
    pub bytes: u64,
    pub packets_per_sec: f64,
    pub kbit_per_sec: f64,
    pub lost: u64,
    pub late: u64,
    /// Lost as a share of expected packets.
    pub loss_percent: f64,
    /// RFC 3550 interarrival jitter (ms).
    pub jitter_ms: Option<f64>,
    /// Largest gap between packets (ms).
    pub max_gap_ms: f64,
    pub ptime_ms: Option<f64>,
    pub ttl: Option<u8>,
    pub tos: Option<u8>,
    /// Peak and RMS audio level (dBFS) for decodable audio.
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    /// Time since the last packet.
    pub idle: Duration,
    /// The sender's RTCP reports, if it sends them.
    pub rtcp: Option<crate::SenderRtcp>,
}

impl StreamReport {
    pub(crate) fn new(group: SocketAddr, name: Option<String>, source: SocketAddr) -> Self {
        Self {
            group,
            name,
            source,
            ssrc: None,
            payload_type: None,
            codec: None,
            packets: 0,
            bytes: 0,
            packets_per_sec: 0.0,
            kbit_per_sec: 0.0,
            lost: 0,
            late: 0,
            loss_percent: 0.0,
            jitter_ms: None,
            max_gap_ms: 0.0,
            ptime_ms: None,
            ttl: None,
            tos: None,
            peak_dbfs: None,
            rms_dbfs: None,
            idle: Duration::ZERO,
            rtcp: None,
        }
    }

    pub fn is_rtp(&self) -> bool {
        self.ssrc.is_some()
    }

    pub fn dscp(&self) -> Option<u8> {
        self.tos.map(|t| t >> 2)
    }
}

/// Common names for DSCP code points.
pub fn dscp_name(dscp: u8) -> String {
    match dscp {
        0 => "BE".into(),
        46 => "EF".into(),
        44 => "VA".into(),
        d if d % 8 == 0 => format!("CS{}", d / 8),
        d if (10..=38).contains(&d) && d % 2 == 0 => format!("AF{}{}", d / 8, (d % 8) / 2),
        d => d.to_string(),
    }
}

#[derive(Default, Clone, Copy)]
struct Counters {
    packets: u64,
    bytes: u64,
    lost: u64,
    late: u64,
    max_gap: Duration,
    peak: f32,
    sum_sq: f64,
    samples: u64,
}

pub(crate) struct StreamState {
    pub last_seen: Instant,
    /// The confirmed RTP stream's SSRC.
    ssrc: Option<u32>,
    /// A candidate SSRC: (ssrc, last sequence, packets in a row).
    candidate: Option<(u32, u16, u32)>,
    first_seen: Instant,
    interval: Counters,
    total: Counters,
    tracker: SequenceTracker,
    payload_type: Option<u8>,
    ttl: Option<u8>,
    tos: Option<u8>,
    /// Codec from the SAP announcement.
    announced: Option<StreamSpec>,
    /// Codec to assume for dynamic payload types (`--encoding`).
    fallback: Option<StreamSpec>,
    /// Codec in use: announced, else from a static payload type, else fallback.
    spec: Option<StreamSpec>,
    decoder: Option<Box<dyn Decoder>>,
    decode_failed: bool,
    clock: Option<f64>,
    /// First (arrival, extended timestamp) for estimating an unknown clock.
    clock_probe: Option<(Instant, i64, u64)>,
    ext_ts: Option<i64>,
    last_ts: Option<u32>,
    last_transit: Option<f64>,
    jitter: f64,
    max_jitter_ms: f64,
    ptime_ms: Option<f64>,
}

const COMMON_CLOCKS: [f64; 8] = [
    8000.0, 16000.0, 22050.0, 24000.0, 32000.0, 44100.0, 48000.0, 90000.0,
];

impl StreamState {
    pub fn new(
        at: Instant,
        announced: Option<StreamSpec>,
        fallback: Option<StreamSpec>,
        clock: Option<u32>,
    ) -> Self {
        Self {
            last_seen: at,
            ssrc: None,
            candidate: None,
            first_seen: at,
            interval: Counters::default(),
            total: Counters::default(),
            tracker: SequenceTracker::new(),
            payload_type: None,
            ttl: None,
            tos: None,
            announced,
            fallback,
            spec: announced,
            decoder: None,
            decode_failed: false,
            clock: clock.map(f64::from),
            clock_probe: None,
            ext_ts: None,
            last_ts: None,
            last_transit: None,
            jitter: 0.0,
            max_jitter_ms: 0.0,
            ptime_ms: None,
        }
    }

    pub fn ssrc(&self) -> Option<u32> {
        self.ssrc
    }

    pub fn clock(&self) -> Option<f64> {
        self.clock
    }

    pub fn packet(
        &mut self,
        data: &[u8],
        ttl: Option<u8>,
        tos: Option<u8>,
        at: Instant,
        epoch: Instant,
    ) {
        let gap = at.saturating_duration_since(self.last_seen);
        for c in [&mut self.interval, &mut self.total] {
            c.packets += 1;
            c.bytes += data.len() as u64;
            c.max_gap = c.max_gap.max(gap);
        }
        self.last_seen = at;
        self.ttl = ttl.or(self.ttl);
        self.tos = tos.or(self.tos);

        let Ok(pkt) = rtp::parse(data) else { return };
        let h = pkt.header;
        if self.ssrc != Some(h.ssrc) {
            // Not (yet) the stream we're tracking: is a new one establishing itself?
            let run = match self.candidate {
                Some((ssrc, seq, n))
                    if ssrc == h.ssrc && h.sequence.wrapping_sub(seq).wrapping_sub(1) < 16 =>
                {
                    n + 1
                }
                _ => 1,
            };
            self.candidate = Some((h.ssrc, h.sequence, run));
            if run < RTP_CONFIRM {
                return;
            }
            // Confirmed: (re)start RTP analysis on this SSRC.
            self.ssrc = Some(h.ssrc);
            self.candidate = None;
            self.tracker = SequenceTracker::new();
            self.tracker.update(h.sequence.wrapping_sub(1));
            self.last_transit = None;
            self.clock_probe = None;
            self.ext_ts = None;
            self.last_ts = None;
            self.decoder = None;
            self.payload_type = None;
        }
        if self.payload_type != Some(h.payload_type) {
            self.payload_type = Some(h.payload_type);
            // Static payload types tell us the codec and clock; --encoding
            // only fills in for dynamic ones (96-127).
            let stat = StreamSpec::from_static_payload_type(h.payload_type);
            let dynamic = (h.payload_type >= 96).then_some(self.fallback).flatten();
            self.spec = self.announced.or(stat).or(dynamic);
            if let Some(s) = stat.filter(|_| self.announced.is_none()) {
                self.clock = self.clock.or(Some(s.rtp_clock_rate() as f64));
            }
            self.decoder = None;
            self.decode_failed = false;
        }

        // Unwrap the 32-bit timestamp.
        let ext = match (self.ext_ts, self.last_ts) {
            (Some(e), Some(l)) => e + h.timestamp.wrapping_sub(l) as i32 as i64,
            _ => h.timestamp as i64,
        };

        let in_order = match self.tracker.update(h.sequence) {
            SeqEvent::Late => {
                self.interval.late += 1;
                self.total.late += 1;
                false
            }
            SeqEvent::Gap(n) => {
                self.interval.lost += n as u64;
                self.total.lost += n as u64;
                true
            }
            SeqEvent::Reset => {
                self.last_transit = None;
                self.clock_probe = None;
                true
            }
            SeqEvent::InOrder => true,
        };
        if !in_order {
            return;
        }

        if self.clock.is_none() {
            self.estimate_clock(at, ext);
        }
        if let (Some(clock), Some(last), Some(last_ext)) = (self.clock, self.last_ts, self.ext_ts) {
            if gap > Duration::ZERO || h.timestamp != last {
                let step = (ext - last_ext) as f64;
                if step > 0.0 {
                    self.ptime_ms = Some(step / clock * 1000.0);
                }
            }
            // RFC 3550 section 6.4.1: J += (|D| - J) / 16, in timestamp units.
            let transit = at.duration_since(epoch).as_secs_f64() * clock - ext as f64;
            if let Some(prev) = self.last_transit {
                self.jitter += ((transit - prev).abs() - self.jitter) / 16.0;
                self.max_jitter_ms = self.max_jitter_ms.max(self.jitter / clock * 1000.0);
            }
            self.last_transit = Some(transit);
        }
        self.ext_ts = Some(ext);
        self.last_ts = Some(h.timestamp);

        self.meter(pkt.payload);
    }

    fn estimate_clock(&mut self, at: Instant, ext: i64) {
        match self.clock_probe {
            None => self.clock_probe = Some((at, ext, 0)),
            Some((t0, ts0, n)) => {
                self.clock_probe = Some((t0, ts0, n + 1));
                let dt = at.duration_since(t0).as_secs_f64();
                if dt >= 2.0 && n >= 10 && ext > ts0 {
                    let raw = (ext - ts0) as f64 / dt;
                    let snapped = COMMON_CLOCKS
                        .iter()
                        .copied()
                        .find(|c| (raw / c - 1.0).abs() < 0.05);
                    self.clock = Some(snapped.unwrap_or(raw));
                }
            }
        }
    }

    fn meter(&mut self, payload: &[u8]) {
        if self.decode_failed {
            return;
        }
        let Some(spec) = self.spec else { return };
        if self.decoder.is_none() {
            match spec.decoder() {
                Ok(d) => self.decoder = Some(d),
                Err(_) => {
                    self.decode_failed = true;
                    return;
                }
            }
        }
        let mut samples = Vec::new();
        if self
            .decoder
            .as_mut()
            .unwrap()
            .decode(payload, &mut samples)
            .is_err()
        {
            // Wrong codec guess (e.g. a dynamic payload type): stop trying.
            self.decode_failed = true;
            return;
        }
        for c in [&mut self.interval, &mut self.total] {
            for &s in &samples {
                c.peak = c.peak.max(s.abs());
                c.sum_sq += (s as f64) * (s as f64);
            }
            c.samples += samples.len() as u64;
        }
    }

    fn fill(
        &self,
        mut r: StreamReport,
        c: &Counters,
        elapsed: Duration,
        now: Instant,
        jitter_ms: Option<f64>,
    ) -> StreamReport {
        // A single packet has no meaningful rate.
        let secs = if c.packets < 2 {
            f64::INFINITY
        } else {
            elapsed.as_secs_f64().max(1e-3)
        };
        r.ssrc = self.ssrc;
        r.payload_type = self.payload_type;
        r.codec = self
            .spec
            .filter(|_| r.ssrc.is_some())
            .map(|s| s.to_string());
        r.packets = c.packets;
        r.bytes = c.bytes;
        r.packets_per_sec = c.packets as f64 / secs;
        r.kbit_per_sec = c.bytes as f64 * 8.0 / secs / 1000.0;
        r.lost = c.lost;
        r.late = c.late;
        let expected = c.packets + c.lost;
        r.loss_percent = if expected > 0 {
            c.lost as f64 * 100.0 / expected as f64
        } else {
            0.0
        };
        r.jitter_ms = jitter_ms;
        r.max_gap_ms = c.max_gap.as_secs_f64() * 1000.0;
        r.ptime_ms = self.ptime_ms;
        r.ttl = self.ttl;
        r.tos = self.tos;
        if c.samples > 0 {
            r.peak_dbfs = Some(20.0 * (c.peak as f64).max(1e-6).log10());
            r.rms_dbfs = Some(10.0 * (c.sum_sq / c.samples as f64).max(1e-12).log10());
        }
        r.idle = now.saturating_duration_since(self.last_seen);
        r
    }

    fn jitter_ms(&self) -> Option<f64> {
        self.clock
            .filter(|_| self.last_transit.is_some())
            .map(|c| self.jitter / c * 1000.0)
    }

    /// Report the interval and reset its counters.
    pub fn take_interval(
        &mut self,
        base: StreamReport,
        elapsed: Duration,
        now: Instant,
    ) -> StreamReport {
        let c = std::mem::take(&mut self.interval);
        self.fill(base, &c, elapsed, now, self.jitter_ms())
    }

    /// Report everything since the stream was first seen (jitter is the maximum).
    pub fn totals(&self, base: StreamReport, _since_start: Duration, now: Instant) -> StreamReport {
        let elapsed = self
            .last_seen
            .duration_since(self.first_seen)
            .max(Duration::from_millis(1));
        let max_jitter = self.jitter_ms().map(|_| self.max_jitter_ms);
        self.fill(base, &self.total, elapsed, now, max_jitter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcast_core::rtp::RtpHeader;

    fn rtp_packet(seq: u16, ts: u32, pt: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        RtpHeader {
            marker: false,
            payload_type: pt,
            sequence: seq,
            timestamp: ts,
            ssrc: 9,
        }
        .write_to(&mut v);
        v.extend_from_slice(payload);
        v
    }

    fn base() -> StreamReport {
        let a: SocketAddr = "239.1.1.1:5004".parse().unwrap();
        StreamReport::new(a, None, a)
    }

    #[test]
    fn steady_stream_has_no_jitter_or_loss_and_correct_rates() {
        let t0 = Instant::now();
        let mut s = StreamState::new(t0, None, None, None);
        // PCMU, 20 ms packets of a -6 dBFS-ish square wave, perfectly paced.
        let payload = [mcast_audio::codec::g711_encode_ulaw(16_384); 160];
        for i in 0..100u32 {
            let at = t0 + Duration::from_millis(20 * i as u64);
            s.packet(
                &rtp_packet(i as u16, i * 160, 0, &payload),
                Some(60),
                Some(184),
                at,
                t0,
            );
        }
        let r = s.take_interval(base(), Duration::from_secs(2), t0 + Duration::from_secs(2));
        assert_eq!((r.packets, r.lost, r.late), (100, 0, 0));
        assert_eq!(r.ssrc, Some(9));
        assert!((r.packets_per_sec - 50.0).abs() < 0.01);
        assert!((r.kbit_per_sec - 100.0 * 172.0 * 8.0 / 2.0 / 1000.0).abs() < 0.01);
        assert!(r.jitter_ms.unwrap() < 0.001);
        assert_eq!(r.ptime_ms, Some(20.0));
        assert_eq!(r.codec.as_deref(), Some("PCMU/8000/1"));
        assert_eq!((r.ttl, r.dscp()), (Some(60), Some(46)));
        assert!(
            (r.peak_dbfs.unwrap() + 6.0).abs() < 0.2,
            "{:?}",
            r.peak_dbfs
        );
    }

    #[test]
    fn jitter_follows_rfc3550_and_loss_is_counted() {
        let t0 = Instant::now();
        let mut s = StreamState::new(t0, None, None, Some(8000));
        // Alternate arrivals 4 ms early/late: |D| = 8 ms each packet, so J -> 8 ms.
        let mut seq = 0u16;
        for i in 0..400u32 {
            if i % 50 == 25 {
                seq = seq.wrapping_add(1); // drop one
                continue;
            }
            let wobble = if i % 2 == 0 { 0 } else { 8 };
            let at = t0 + Duration::from_millis(20 * i as u64 + wobble);
            s.packet(&rtp_packet(seq, i * 160, 96, &[0; 160]), None, None, at, t0);
            seq = seq.wrapping_add(1);
        }
        let r = s.take_interval(base(), Duration::from_secs(8), t0 + Duration::from_secs(8));
        assert_eq!(r.lost, 8);
        assert!((r.loss_percent - 2.0).abs() < 0.01);
        assert!(
            (r.jitter_ms.unwrap() - 8.0).abs() < 0.5,
            "{:?}",
            r.jitter_ms
        );
        assert!(
            r.peak_dbfs.is_none(),
            "dynamic PT without a codec isn't metered"
        );
    }

    #[test]
    fn estimates_unknown_clock() {
        let t0 = Instant::now();
        let mut s = StreamState::new(t0, None, None, None);
        for i in 0..200u32 {
            let at = t0 + Duration::from_millis(20 * i as u64);
            s.packet(
                &rtp_packet(i as u16, i * 960, 107, &[0; 50]),
                None,
                None,
                at,
                t0,
            );
        }
        assert_eq!(s.clock, Some(48_000.0));
        let r = s.take_interval(base(), Duration::from_secs(4), t0 + Duration::from_secs(4));
        assert_eq!(r.ptime_ms, Some(20.0));
    }

    #[test]
    fn encoding_flag_only_applies_to_dynamic_payload_types() {
        let t0 = Instant::now();
        let opus = Some(mcast_audio::StreamSpec::new(
            mcast_audio::Codec::Opus,
            48_000,
            1,
        ));
        let mut s = StreamState::new(t0, None, opus, None);
        for i in 0..10u32 {
            s.packet(
                &rtp_packet(i as u16, i * 160, 0, &[0xff; 160]),
                None,
                None,
                t0,
                t0,
            );
        }
        let r = s.take_interval(base(), Duration::from_secs(1), t0);
        assert_eq!(r.codec.as_deref(), Some("PCMU/8000/1"));
    }

    #[test]
    fn random_data_is_not_rtp() {
        let t0 = Instant::now();
        let mut s = StreamState::new(t0, None, None, None);
        let mut seed = 1u32;
        for i in 0..200u64 {
            let pkt: Vec<u8> = (0..300)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (seed >> 24) as u8
                })
                .collect();
            s.packet(&pkt, None, None, t0 + Duration::from_millis(10 * i), t0);
        }
        let r = s.take_interval(base(), Duration::from_secs(2), t0 + Duration::from_secs(2));
        assert_eq!(r.packets, 200);
        assert!(!r.is_rtp());
        assert_eq!(r.lost, 0);
    }

    #[test]
    fn dscp_names() {
        assert_eq!(dscp_name(46), "EF");
        assert_eq!(dscp_name(0), "BE");
        assert_eq!(dscp_name(34), "AF41");
        assert_eq!(dscp_name(40), "CS5");
        assert_eq!(dscp_name(26), "AF31");
    }
}
