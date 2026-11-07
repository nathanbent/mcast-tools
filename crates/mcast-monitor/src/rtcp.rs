//! RTCP participants seen on a group's control port: senders (heartbeat,
//! delay, clock drift) and listeners (loss, jitter, playout delay).

use mcast_core::rtcp::{ntp_diff_secs, Packet, Playout, ReportBlock, SDES_CNAME, SDES_NAME};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Forget participants we haven't heard from in this long (RFC 3550
/// suggests five reporting intervals).
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);
/// Keep participants that said BYE around this long so the goodbye shows.
const LINGER: Duration = Duration::from_secs(10);
/// Media clock drift needs a long baseline to be meaningful.
const DRIFT_BASELINE_SECS: f64 = 10.0;

/// What we know about a sender from its SRs.
#[derive(Debug, Clone)]
struct SenderClock {
    first_ntp: u64,
    last_ntp: u64,
    last_rtp: u32,
    /// Extended (unwrapped) RTP timestamp of the last SR, relative to the first.
    ext: i64,
    reports: u64,
    last_at: Instant,
    /// Smoothed time between SRs.
    interval: Option<f64>,
    /// Arrival time minus the SR's own timestamp, for the last SR.
    one_way: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct Participant {
    from: SocketAddr,
    cname: Option<String>,
    name: Option<String>,
    last_heard: Instant,
    bye: Option<(Option<String>, Instant)>,
    sender: Option<SenderClock>,
    blocks: Vec<ReportBlock>,
    playout: Option<Playout>,
}

impl Participant {
    pub fn new(from: SocketAddr, at: Instant) -> Self {
        Self {
            from,
            cname: None,
            name: None,
            last_heard: at,
            bye: None,
            sender: None,
            blocks: Vec::new(),
            playout: None,
        }
    }

    pub fn expired(&self, now: Instant) -> bool {
        let quiet = now.saturating_duration_since(self.last_heard);
        quiet > TIMEOUT
            || self
                .bye
                .as_ref()
                .is_some_and(|(_, at)| now.duration_since(*at) > LINGER)
    }

    pub fn is_sender(&self) -> bool {
        self.sender.is_some()
    }

    /// Fold in one packet of a compound that `ssrc` sent.
    pub fn update(&mut self, ssrc: u32, p: &Packet, from: SocketAddr, ntp: u64, at: Instant) {
        self.from = from;
        self.last_heard = at;
        match p {
            Packet::SenderReport { info, reports, .. } => {
                let s = self.sender.get_or_insert(SenderClock {
                    first_ntp: info.ntp,
                    last_ntp: info.ntp,
                    last_rtp: info.rtp_timestamp,
                    ext: 0,
                    reports: 0,
                    last_at: at,
                    interval: None,
                    one_way: 0.0,
                });
                if s.reports > 0 {
                    let gap = at.duration_since(s.last_at).as_secs_f64();
                    s.interval = Some(s.interval.map_or(gap, |i| i + (gap - i) / 4.0));
                    s.ext += info.rtp_timestamp.wrapping_sub(s.last_rtp) as i32 as i64;
                }
                s.last_ntp = info.ntp;
                s.last_rtp = info.rtp_timestamp;
                s.last_at = at;
                s.reports += 1;
                s.one_way = ntp_diff_secs(ntp, info.ntp);
                self.blocks.clone_from(reports);
            }
            Packet::ReceiverReport { reports, .. } => self.blocks.clone_from(reports),
            Packet::Sdes(chunks) => {
                if let Some(c) = chunks.iter().find(|c| c.ssrc == ssrc) {
                    self.cname = c.item(SDES_CNAME).map(str::to_string).or(self.cname.take());
                    self.name = c.item(SDES_NAME).map(str::to_string).or(self.name.take());
                }
            }
            Packet::Bye { reason, .. } => self.bye = Some((reason.clone(), at)),
            p => {
                if let Some((_, playout)) = Playout::from_app(p) {
                    self.playout = Some(playout);
                }
            }
        }
    }

    pub fn sender_report(&self, clock: Option<f64>, now: Instant) -> Option<SenderRtcp> {
        let s = self.sender.as_ref()?;
        let span = ntp_diff_secs(s.last_ntp, s.first_ntp);
        let media_clock_ppm = clock
            .filter(|_| span >= DRIFT_BASELINE_SECS)
            .map(|c| (s.ext as f64 / c / span - 1.0) * 1e6);
        Some(SenderRtcp {
            cname: self.cname.clone(),
            name: self.name.clone(),
            reports: s.reports,
            last_report: now.saturating_duration_since(s.last_at),
            interval: s.interval.map(Duration::from_secs_f64),
            one_way_secs: s.one_way,
            media_clock_ppm,
            bye: self.bye.as_ref().map(|(r, _)| r.clone()),
        })
    }

    pub fn listener_report(
        &self,
        group: SocketAddr,
        ssrc: u32,
        clock: impl Fn(u32) -> Option<f64>,
        now: Instant,
    ) -> ListenerReport {
        let about = self
            .playout
            .map(|p| p.media_ssrc)
            .or(self.blocks.first().map(|b| b.ssrc));
        let block = self.blocks.iter().find(|b| Some(b.ssrc) == about).copied();
        ListenerReport {
            group,
            ssrc,
            from: self.from,
            cname: self.cname.clone(),
            name: self.name.clone(),
            last_heard: now.saturating_duration_since(self.last_heard),
            left: self.bye.is_some(),
            about,
            loss_percent: block.map(|b| b.loss_fraction() * 100.0),
            cumulative_lost: block.map(|b| b.cumulative_lost),
            jitter_ms: block.and_then(|b| clock(b.ssrc).map(|c| b.jitter as f64 / c * 1000.0)),
            transit_ms: self
                .playout
                .and_then(|p| p.transit_secs)
                .map(|t| t * 1000.0),
            buffered_ms: self.playout.map(|p| p.buffered.as_secs_f64() * 1000.0),
        }
    }
}

/// A sender's RTCP, shown alongside its stream.
#[derive(Debug, Clone)]
pub struct SenderRtcp {
    pub cname: Option<String>,
    pub name: Option<String>,
    pub reports: u64,
    /// Time since the last sender report: the heartbeat.
    pub last_report: Duration,
    /// Typical time between reports.
    pub interval: Option<Duration>,
    /// Arrival time minus the sender's timestamp in its last report. The
    /// network delay if both clocks are synced (NTP/PTP); otherwise mostly
    /// the difference between the clocks.
    pub one_way_secs: f64,
    /// How fast the stream's media clock runs against the sender's wall
    /// clock, in parts per million (needs 10 s of reports).
    pub media_clock_ppm: Option<f64>,
    /// The sender said BYE (with this reason, if any).
    pub bye: Option<Option<String>>,
}

/// A receiver heard on a group's RTCP port.
#[derive(Debug, Clone)]
pub struct ListenerReport {
    pub group: SocketAddr,
    pub ssrc: u32,
    pub from: SocketAddr,
    pub cname: Option<String>,
    pub name: Option<String>,
    pub last_heard: Duration,
    /// Said BYE.
    pub left: bool,
    /// SSRC of the stream it's reporting on.
    pub about: Option<u32>,
    /// Loss since its previous report.
    pub loss_percent: Option<f64>,
    pub cumulative_lost: Option<i32>,
    pub jitter_ms: Option<f64>,
    /// mcast-tools receivers only: sender clock to arrival.
    pub transit_ms: Option<f64>,
    /// mcast-tools receivers only: audio in the playout buffer.
    pub buffered_ms: Option<f64>,
}

impl ListenerReport {
    /// How far behind the sender this listener plays: transit plus buffer
    /// (excluding the sound card's own latency).
    pub fn latency_ms(&self) -> Option<f64> {
        Some(self.transit_ms? + self.buffered_ms?)
    }

    pub fn label(&self) -> String {
        match (&self.name, &self.cname) {
            (Some(n), _) => n.clone(),
            (None, Some(c)) => c.clone(),
            (None, None) => format!("ssrc {:08x}", self.ssrc),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcast_core::rtcp::{ntp_add_secs, ReceptionStats, SdesChunk, SenderInfo};

    fn addr() -> SocketAddr {
        "10.0.0.2:40000".parse().unwrap()
    }

    #[test]
    fn sender_heartbeat_delay_and_drift() {
        let t0 = Instant::now();
        let ntp0 = 0xE000_0000_0000_0000u64;
        let mut p = Participant::new(addr(), t0);
        // SRs every 5 s; the media clock runs 100 ppm fast (8000.8 Hz), and
        // each report arrives 3 ms after it was stamped.
        for i in 0..5u32 {
            let secs = 5.0 * i as f64;
            let sr = Packet::SenderReport {
                ssrc: 1,
                info: SenderInfo {
                    ntp: ntp_add_secs(ntp0, secs),
                    rtp_timestamp: (u32::MAX - 10_000).wrapping_add((secs * 8000.8) as u32),
                    packets: 0,
                    octets: 0,
                },
                reports: vec![],
            };
            let at = t0 + Duration::from_secs(5 * i as u64);
            p.update(1, &sr, addr(), ntp_add_secs(ntp0, secs + 0.003), at);
        }
        p.update(
            1,
            &Packet::Sdes(vec![SdesChunk {
                ssrc: 1,
                items: vec![(SDES_NAME, "Lobby".into())],
            }]),
            addr(),
            0,
            t0 + Duration::from_secs(20),
        );
        let r = p
            .sender_report(Some(8000.0), t0 + Duration::from_secs(22))
            .unwrap();
        assert_eq!(r.reports, 5);
        assert_eq!(r.name.as_deref(), Some("Lobby"));
        assert_eq!(r.last_report, Duration::from_secs(2));
        assert!((r.interval.unwrap().as_secs_f64() - 5.0).abs() < 1e-6);
        assert!((r.one_way_secs - 0.003).abs() < 1e-6);
        assert!(
            (r.media_clock_ppm.unwrap() - 100.0).abs() < 1.0,
            "{:?}",
            r.media_clock_ppm
        );
        assert!(r.bye.is_none());
    }

    #[test]
    fn listener_loss_jitter_and_playout() {
        let t0 = Instant::now();
        let mut stats = ReceptionStats::new();
        for i in 0..50u16 {
            if i != 10 {
                stats.packet(
                    i,
                    i as u32 * 160,
                    i as f64 * 160.0 + if i % 2 == 0 { 0.0 } else { 16.0 },
                );
            }
        }
        let block = stats.report(7, None).unwrap();
        let mut p = Participant::new(addr(), t0);
        p.update(
            9,
            &Packet::ReceiverReport {
                ssrc: 9,
                reports: vec![block],
            },
            addr(),
            0,
            t0,
        );
        p.update(
            9,
            &Playout::new(7, Some(0.004), Duration::from_millis(40)).to_app(9),
            addr(),
            0,
            t0,
        );
        let r = p.listener_report(addr(), 9, |_| Some(8000.0), t0 + Duration::from_secs(1));
        assert_eq!(r.about, Some(7));
        assert!((r.loss_percent.unwrap() - 1.0 / 50.0 * 100.0).abs() < 0.5);
        assert!(
            (r.jitter_ms.unwrap() - 2.0).abs() < 0.3,
            "{:?}",
            r.jitter_ms
        );
        assert!((r.latency_ms().unwrap() - 44.0).abs() < 1e-6);
        assert_eq!(r.label(), "ssrc 00000009");
        assert!(!p.expired(t0 + Duration::from_secs(29)));
        assert!(p.expired(t0 + Duration::from_secs(31)));
        p.update(
            9,
            &Packet::Bye {
                ssrcs: vec![9],
                reason: None,
            },
            addr(),
            0,
            t0,
        );
        assert!(p.expired(t0 + Duration::from_secs(11)));
    }
}
