//! RTCP sessions: sender reports beside an RTP stream, receiver reports,
//! and a background listener for both.

use crate::net::{open_receiver, open_sender, MulticastOptions};
use crate::rtcp::{
    self, jittered, ntp_now, Packet, Playout, ReportBlock, SdesChunk, SenderInfo, SDES_CNAME,
    SDES_NAME, SDES_TOOL,
};
use crate::rtp::RTP_HEADER_LEN;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// RTCP settings for a sender or receiver.
#[derive(Debug, Clone)]
pub struct RtcpConfig {
    /// Our name in reports (SDES NAME), e.g. the stream or the room.
    pub name: Option<String>,
    /// Average time between reports.
    pub interval: Duration,
}

const TOOL: &str = concat!("mcast-tools ", env!("CARGO_PKG_VERSION"));

/// Where RTCP for `rtp` goes, with the same interface/TTL/TOS settings.
pub fn rtcp_options(rtp: &MulticastOptions) -> Option<MulticastOptions> {
    let mut o = rtp.clone();
    o.group = rtcp::rtcp_addr(rtp.group)?;
    Some(o)
}

fn sdes(ssrc: u32, socket: &UdpSocket, name: Option<&str>) -> Packet {
    // CNAME should be stable and unique per host: user@host form, by address.
    let host = socket
        .local_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "localhost".into());
    let mut items = vec![(SDES_CNAME, format!("mcast@{host}"))];
    if let Some(n) = name {
        items.push((SDES_NAME, n.to_string()));
    }
    items.push((SDES_TOOL, TOOL.to_string()));
    Packet::Sdes(vec![SdesChunk { ssrc, items }])
}

/// Sends Sender Reports beside an RTP stream, and BYE when it ends.
pub struct SenderRtcp {
    socket: UdpSocket,
    ssrc: u32,
    clock_rate: f64,
    sdes: Packet,
    interval: Duration,
    next: Option<Instant>,
    packets: u32,
    octets: u32,
    /// RTP timestamp of the last packet sent, and when.
    last: Option<(u32, Instant)>,
    /// When we last sent a report.
    reported: Option<Instant>,
    done: bool,
}

impl SenderRtcp {
    /// Open the RTCP socket for an RTP stream on `net` (port + 1).
    pub fn open(
        net: &MulticastOptions,
        ssrc: u32,
        clock_rate: u32,
        name: Option<&str>,
        interval: Duration,
    ) -> io::Result<Self> {
        let opts = rtcp_options(net).ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "RTP port 65535 leaves no room for RTCP",
            )
        })?;
        let socket = open_sender(&opts)?;
        let sdes = sdes(ssrc, &socket, name);
        Ok(Self {
            socket,
            ssrc,
            clock_rate: clock_rate as f64,
            sdes,
            interval,
            next: None,
            packets: 0,
            octets: 0,
            last: None,
            reported: None,
            done: false,
        })
    }

    pub fn group(&self) -> Option<SocketAddr> {
        self.socket.peer_addr().ok()
    }

    /// Note an RTP packet just sent; sends a report when one is due.
    pub fn sent(&mut self, packet: &[u8], at: Instant) {
        if packet.len() < RTP_HEADER_LEN {
            return;
        }
        let ts = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        self.packets = self.packets.wrapping_add(1);
        self.octets = self
            .octets
            .wrapping_add((packet.len() - RTP_HEADER_LEN) as u32);
        self.last = Some((ts, at));
        self.tick(at);
    }

    /// Tie the media clock to the wall clock before (or between) packets:
    /// `ts` is the RTP timestamp at instant `at`. For streams such as text
    /// that go quiet, so reports stay meaningful while idle.
    pub fn anchor(&mut self, ts: u32, at: Instant) {
        self.last = Some((ts, at));
    }

    /// Send a report if one is due. Call this regularly when no packets are
    /// being sent, so the heartbeat continues.
    pub fn tick(&mut self, now: Instant) {
        // RFC 3550 6.2: the first report comes after half an interval.
        let next = *self
            .next
            .get_or_insert_with(|| now + jittered(self.interval) / 2);
        if now >= next {
            self.next = Some(now + jittered(self.interval));
            self.send(&[]);
        }
    }

    /// Send a report now, unless one went out within `min_gap`. For bursty
    /// streams: a report just before a burst tells receivers who's talking.
    pub fn report_now(&mut self, min_gap: Duration) {
        let now = Instant::now();
        if self
            .reported
            .is_some_and(|t| now.duration_since(t) < min_gap)
        {
            return;
        }
        self.next = Some(now + jittered(self.interval));
        self.send(&[]);
    }

    /// When the next report is due (for sleeping until then).
    pub fn next_due(&self) -> Option<Instant> {
        self.next
    }

    fn report(&self) -> Option<Packet> {
        let (ts, at) = self.last?;
        let ntp = ntp_now();
        // The timestamp of a sample sent right now, extrapolated from the
        // last packet (sent at `at`).
        let since = Instant::now().saturating_duration_since(at).as_secs_f64();
        let rtp_timestamp = ts.wrapping_add((since * self.clock_rate).round() as u32);
        Some(Packet::SenderReport {
            ssrc: self.ssrc,
            info: SenderInfo {
                ntp,
                rtp_timestamp,
                packets: self.packets,
                octets: self.octets,
            },
            reports: vec![],
        })
    }

    fn send(&mut self, extra: &[Packet]) {
        let mut packets = Vec::with_capacity(2 + extra.len());
        packets.push(self.report().unwrap_or(Packet::ReceiverReport {
            ssrc: self.ssrc,
            reports: vec![],
        }));
        packets.push(self.sdes.clone());
        packets.extend_from_slice(extra);
        self.reported = Some(Instant::now());
        // Best effort: RTCP loss is harmless.
        let _ = self.socket.send(&rtcp::encode(&packets));
    }

    /// Say goodbye (also done on drop).
    pub fn bye(&mut self, reason: Option<&str>) {
        if self.done {
            return;
        }
        self.done = true;
        self.send(&[Packet::Bye {
            ssrcs: vec![self.ssrc],
            reason: reason.map(str::to_string),
        }]);
    }
}

impl Drop for SenderRtcp {
    fn drop(&mut self) {
        self.bye(None);
    }
}

/// An RTCP compound packet as received.
#[derive(Debug, Clone)]
pub struct Received {
    pub packets: Vec<Packet>,
    pub from: SocketAddr,
    /// Local wall-clock time of arrival.
    pub ntp: u64,
    pub at: Instant,
}

/// Receives RTCP on a group in a background thread.
pub struct Listener {
    rx: mpsc::Receiver<Received>,
    stop: Arc<AtomicBool>,
}

impl Listener {
    /// Join the RTCP group for `net` (an RTCP group, already port + 1).
    pub fn start(net: &MulticastOptions) -> io::Result<Self> {
        let socket = open_receiver(net)?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        thread::spawn(move || {
            let mut buf = vec![0u8; 2048];
            while !thread_stop.load(Ordering::Relaxed) {
                match socket.recv_from(&mut buf) {
                    Ok((len, from)) => {
                        let (ntp, at) = (ntp_now(), Instant::now());
                        let Ok(packets) = rtcp::parse(&buf[..len]) else {
                            continue;
                        };
                        let r = Received {
                            packets,
                            from,
                            ntp,
                            at,
                        };
                        if tx.send(r).is_err() {
                            return;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return,
                }
            }
        });
        Ok(Self { rx, stop })
    }

    /// Packets received since the last call.
    pub fn drain(&self) -> impl Iterator<Item = Received> + '_ {
        self.rx.try_iter()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A receiver's side of RTCP: hears the sender's reports and sends its own.
pub struct ReceiverRtcp {
    pub listener: Listener,
    socket: UdpSocket,
    ssrc: u32,
    sdes: Packet,
    interval: Duration,
    next: Instant,
    done: bool,
}

impl ReceiverRtcp {
    /// Join the RTCP group for an RTP stream on `net` and prepare to report.
    pub fn open(
        net: &MulticastOptions,
        name: Option<&str>,
        interval: Duration,
    ) -> io::Result<Self> {
        let opts = rtcp_options(net).ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidInput,
                "RTP port 65535 leaves no room for RTCP",
            )
        })?;
        let listener = Listener::start(&opts)?;
        let socket = open_sender(&opts)?;
        let ssrc = crate::random_u32();
        let sdes = sdes(ssrc, &socket, name);
        Ok(Self {
            listener,
            socket,
            ssrc,
            sdes,
            interval,
            next: Instant::now() + jittered(interval) / 2,
            done: false,
        })
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn due(&self) -> bool {
        Instant::now() >= self.next
    }

    /// Send a Receiver Report (with blocks about the sources we hear) and,
    /// for audio, our playout delay.
    pub fn report(&mut self, blocks: Vec<ReportBlock>, playout: Option<Playout>) {
        self.next = Instant::now() + jittered(self.interval);
        self.send(blocks, playout, false);
    }

    fn send(&mut self, blocks: Vec<ReportBlock>, playout: Option<Playout>, bye: bool) {
        let mut packets = vec![
            Packet::ReceiverReport {
                ssrc: self.ssrc,
                reports: blocks,
            },
            self.sdes.clone(),
        ];
        if let Some(p) = playout {
            packets.push(p.to_app(self.ssrc));
        }
        if bye {
            packets.push(Packet::Bye {
                ssrcs: vec![self.ssrc],
                reason: None,
            });
        }
        let _ = self.socket.send(&rtcp::encode(&packets));
    }

    pub fn bye(&mut self) {
        if !self.done {
            self.done = true;
            self.send(Vec::new(), None, true);
        }
    }
}

impl Drop for ReceiverRtcp {
    fn drop(&mut self) {
        self.bye();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtcp::{ntp_diff_secs, sdes_for};
    use crate::rtp::RtpHeader;

    fn rtp(ts: u32, payload: usize) -> Vec<u8> {
        let mut v = Vec::new();
        RtpHeader {
            marker: false,
            payload_type: 0,
            sequence: 1,
            timestamp: ts,
            ssrc: 42,
        }
        .write_to(&mut v);
        v.resize(v.len() + payload, 0);
        v
    }

    #[test]
    fn sender_reports_and_bye_reach_a_listener() {
        let net = MulticastOptions::new("239.255.77.9:6200".parse().unwrap()).unwrap();
        let listener = Listener::start(&rtcp_options(&net).unwrap()).unwrap();
        let mut s =
            SenderRtcp::open(&net, 42, 8000, Some("Lobby"), Duration::from_millis(20)).unwrap();
        let t0 = Instant::now();
        for i in 0..20u32 {
            s.sent(&rtp(1000 + i * 160, 160), Instant::now());
            std::thread::sleep(Duration::from_millis(20));
        }
        s.bye(Some("done"));
        std::thread::sleep(Duration::from_millis(300));
        let got: Vec<Received> = listener.drain().collect();
        assert!(got.len() >= 2, "{} packets", got.len());
        let mut saw_bye = false;
        for r in &got {
            assert_eq!(
                sdes_for(&r.packets, 42).unwrap().item(SDES_NAME),
                Some("Lobby")
            );
            for p in &r.packets {
                match p {
                    Packet::SenderReport { ssrc, info, .. } => {
                        assert_eq!(*ssrc, 42);
                        // Reported "now" is within a few ms of arrival.
                        assert!(ntp_diff_secs(r.ntp, info.ntp).abs() < 0.05);
                        // The extrapolated timestamp tracks elapsed time at 8 kHz.
                        let elapsed = r.at.duration_since(t0).as_secs_f64();
                        let ts = info.rtp_timestamp.wrapping_sub(1000) as f64 / 8000.0;
                        assert!((ts - elapsed).abs() < 0.03, "{ts} vs {elapsed}");
                    }
                    Packet::Bye { ssrcs, reason } => {
                        assert_eq!(ssrcs, &[42]);
                        assert_eq!(reason.as_deref(), Some("done"));
                        saw_bye = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(saw_bye);
    }
}
