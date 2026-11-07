//! Sends real-time text to a multicast group.

use crate::session;
use crate::t140::{self, encode_red, RedBlock, CLOCK_RATE, MAX_OFFSET};
use anyhow::{Context, Result};
use mcast_core::net::{open_sender, MulticastOptions};
use mcast_core::random_u32;
use mcast_core::rtcp::{RtcpConfig, SenderRtcp};
use mcast_core::rtp::RtpHeader;
use mcast_core::sap::{self, Announcer};
use mcast_core::sdp::SessionDescription;
use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// RFC 4103's recommended buffering time: text is gathered for this long
/// before a packet goes out.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(300);
/// Most new text per packet. With two redundant copies a packet stays well
/// under a 1500-byte MTU.
pub const MAX_CHUNK: usize = 400;
/// After announcing with SAP, wait this long before the first text so
/// receivers that discover the stream have time to join.
pub const SAP_LEAD: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct TextSenderConfig {
    pub net: MulticastOptions,
    pub t140_pt: u8,
    /// Send each packet's text again in the next `generations` packets
    /// (RFC 2198, using this payload type). `None` sends plain T.140.
    pub red: Option<(u8, usize)>,
    /// How long to gather text per packet.
    pub interval: Duration,
    /// Announce the stream with SAP under this name.
    pub announce: Option<Announce>,
    pub rtcp: Option<RtcpConfig>,
}

#[derive(Debug, Clone)]
pub struct Announce {
    pub name: String,
    /// SAP group; `None` picks one from the stream's scope.
    pub sap_group: Option<SocketAddr>,
    pub interval: Duration,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TextSenderStats {
    pub packets: u64,
    /// Characters of new text sent.
    pub chars: u64,
}

pub struct TextSender {
    socket: UdpSocket,
    rtcp: Option<SenderRtcp>,
    _announcer: Option<Announcer>,
    sap_group: Option<SocketAddr>,
    sdp: SessionDescription,
    ssrc: u32,
    sequence: u16,
    ts0: u32,
    start: Instant,
    t140_pt: u8,
    red: Option<(u8, usize)>,
    interval: Duration,
    /// Text not yet sent (T.140, i.e. line breaks already converted).
    pending: String,
    /// Recent primaries (timestamp, text), newest last, for redundancy.
    history: VecDeque<(u32, String)>,
    /// Packets still to send so the last text has all its redundant copies.
    redundancy_left: usize,
    next_send: Instant,
    /// The next packet starts a talkspurt (marker bit).
    marker: bool,
    stats: TextSenderStats,
}

impl TextSender {
    pub fn new(cfg: TextSenderConfig) -> Result<Self> {
        let socket = open_sender(&cfg.net)
            .with_context(|| format!("opening multicast sender for {}", cfg.net.group))?;
        let origin = socket.local_addr()?.ip();
        let ssrc = random_u32();
        let ts0 = random_u32();
        let start = Instant::now();
        let name = cfg
            .announce
            .as_ref()
            .map_or("mcast text", |a| a.name.as_str());
        let sdp = session::describe(
            name,
            origin,
            cfg.net.group,
            cfg.net.ttl,
            cfg.t140_pt,
            cfg.red,
        );
        let rtcp = match &cfg.rtcp {
            Some(r) => {
                let mut s =
                    SenderRtcp::open(&cfg.net, ssrc, CLOCK_RATE, r.name.as_deref(), r.interval)
                        .context("opening RTCP sender (use --no-rtcp to skip)")?;
                // The text clock runs whether or not anyone types.
                s.anchor(ts0, start);
                s.report_now(Duration::ZERO);
                Some(s)
            }
            None => None,
        };
        let (announcer, sap_group) = match &cfg.announce {
            Some(a) => {
                let mut net = cfg.net.clone();
                net.group = a
                    .sap_group
                    .unwrap_or_else(|| sap::announce_group_for(cfg.net.group.ip()));
                net.tos = None;
                let announcer = Announcer::start(&net, &sdp, a.interval)
                    .with_context(|| format!("starting SAP announcements on {}", net.group))?;
                (Some(announcer), Some(net.group))
            }
            None => (None, None),
        };
        Ok(Self {
            socket,
            rtcp,
            _announcer: announcer,
            sap_group,
            sdp,
            ssrc,
            sequence: random_u32() as u16,
            ts0,
            start,
            t140_pt: cfg.t140_pt,
            red: cfg.red.filter(|(_, g)| *g > 0),
            interval: cfg.interval,
            pending: String::new(),
            history: VecDeque::new(),
            redundancy_left: 0,
            next_send: if sap_group.is_some() {
                start + SAP_LEAD
            } else {
                start
            },
            marker: true,
            stats: TextSenderStats::default(),
        })
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    pub fn sdp(&self) -> &SessionDescription {
        &self.sdp
    }

    pub fn sap_group(&self) -> Option<SocketAddr> {
        self.sap_group
    }

    pub fn rtcp_group(&self) -> Option<SocketAddr> {
        self.rtcp.as_ref().and_then(|r| r.group())
    }

    pub fn stats(&self) -> TextSenderStats {
        self.stats
    }

    /// Queue text to send (plain text; `\n` ends a line).
    pub fn push(&mut self, text: &str) {
        self.pending.push_str(&t140::to_t140(text));
    }

    /// True when everything queued, and its redundant copies, has been sent.
    pub fn idle(&self) -> bool {
        self.pending.is_empty() && self.redundancy_left == 0
    }

    /// When `poll` next has something to do.
    pub fn next_wake(&self) -> Option<Instant> {
        let send = (!self.idle()).then_some(self.next_send);
        let rtcp = self.rtcp.as_ref().and_then(|r| r.next_due());
        match (send, rtcp) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Send a packet if one is due, and RTCP if that's due.
    pub fn poll(&mut self) -> io::Result<()> {
        let now = Instant::now();
        if !self.idle() && now >= self.next_send {
            self.send_packet(now)?;
        }
        if let Some(r) = &mut self.rtcp {
            r.tick(now);
        }
        Ok(())
    }

    fn send_packet(&mut self, now: Instant) -> io::Result<()> {
        let (chunk, rest) = t140::split_at_most(&self.pending, MAX_CHUNK);
        let (chunk, rest) = (chunk.to_string(), rest.to_string());
        self.pending = rest;
        let ts = self
            .ts0
            .wrapping_add(now.duration_since(self.start).as_millis() as u32);

        let mut pkt = Vec::with_capacity(64 + 3 * chunk.len());
        RtpHeader {
            marker: self.marker,
            payload_type: self.red.map_or(self.t140_pt, |(pt, _)| pt),
            sequence: self.sequence,
            timestamp: ts,
            ssrc: self.ssrc,
        }
        .write_to(&mut pkt);
        match self.red {
            Some((_, generations)) => {
                // Pad the history so every packet carries `generations` blocks.
                while self.history.len() < generations {
                    self.history.push_front((ts, String::new()));
                }
                let blocks: Vec<RedBlock> = self
                    .history
                    .iter()
                    .map(|(t, text)| {
                        let offset = ts.wrapping_sub(*t);
                        // Too old to express: send it empty.
                        let (ts_offset, data) = if offset > MAX_OFFSET {
                            (0, &b""[..])
                        } else {
                            (offset, text.as_bytes())
                        };
                        RedBlock {
                            payload_type: self.t140_pt,
                            ts_offset,
                            data,
                        }
                    })
                    .collect();
                encode_red(&mut pkt, self.t140_pt, &blocks, chunk.as_bytes());
                self.history.push_back((ts, chunk.clone()));
                while self.history.len() > generations {
                    self.history.pop_front();
                }
                self.redundancy_left = if chunk.is_empty() {
                    self.redundancy_left.saturating_sub(1)
                } else {
                    generations
                };
            }
            None => pkt.extend_from_slice(chunk.as_bytes()),
        }

        if self.marker {
            if let Some(r) = &mut self.rtcp {
                // Bursts are at least ~0.6 s apart (two redundant packets),
                // so this adds at most a couple of small packets a second.
                r.report_now(Duration::from_millis(250));
            }
        }
        match self.socket.send(&pkt) {
            Ok(_) => {}
            // A full buffer just loses this packet; redundancy covers it.
            Err(e) if e.raw_os_error() == Some(105) || e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        if let Some(r) = &mut self.rtcp {
            r.sent(&pkt, now);
        }
        self.stats.packets += 1;
        self.stats.chars += chunk.chars().count() as u64;
        self.sequence = self.sequence.wrapping_add(1);
        self.next_send = now + self.interval;
        // After going quiet, the next text starts a new burst.
        self.marker = self.idle();
        Ok(())
    }

    /// Send everything queued (with its redundancy), sleeping as needed.
    pub fn flush(&mut self) -> io::Result<()> {
        while !self.idle() {
            if let Some(wait) = self.next_send.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            self.poll()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t140::decode_red;
    use mcast_core::net::open_receiver;
    use mcast_core::rtp;

    #[test]
    fn sends_text_with_two_generations_of_redundancy() {
        let net = MulticastOptions::new("239.255.77.21:6300".parse().unwrap()).unwrap();
        let rx = open_receiver(&net).unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut s = TextSender::new(TextSenderConfig {
            net,
            t140_pt: 98,
            red: Some((100, 2)),
            interval: Duration::from_millis(20),
            announce: None,
            rtcp: None,
        })
        .unwrap();
        s.push("hi\n");
        s.flush().unwrap();
        s.push("yo");
        s.flush().unwrap();

        let mut buf = [0u8; 1500];
        let mut packets = Vec::new();
        for _ in 0..6 {
            let n = rx.recv(&mut buf).unwrap();
            let p = rtp::parse(&buf[..n]).unwrap();
            let red = decode_red(p.payload).unwrap();
            let gens: Vec<String> = red
                .redundant
                .iter()
                .map(|b| String::from_utf8(b.data.to_vec()).unwrap())
                .collect();
            packets.push((
                p.header.marker,
                gens,
                String::from_utf8(red.primary.to_vec()).unwrap(),
            ));
        }
        let s = |x: &str| x.to_string();
        assert_eq!(
            packets,
            vec![
                (true, vec![s(""), s("")], s("hi\u{2028}")),
                (false, vec![s(""), s("hi\u{2028}")], s("")),
                (false, vec![s("hi\u{2028}"), s("")], s("")),
                (true, vec![s(""), s("")], s("yo")),
                (false, vec![s(""), s("yo")], s("")),
                (false, vec![s("yo"), s("")], s("")),
            ]
        );
        // Nothing more: the sender goes quiet once redundancy is done.
        rx.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        assert!(rx.recv(&mut buf).is_err());
    }
}
