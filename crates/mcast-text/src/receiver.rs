//! Receives real-time text from a multicast group.

use crate::t140::{self, decode_red, CLOCK_RATE, MISSING_TEXT};
use anyhow::{Context, Result};
use mcast_core::net::{open_receiver, MulticastOptions};
use mcast_core::rtcp::{
    looks_like_rtcp, rtcp_options, Listener, Packet, ReceiverRtcp, ReceptionStats, RtcpConfig,
    SDES_CNAME, SDES_NAME,
};
use mcast_core::rtp::{self, RtpPacket};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Turns RTP packets from any number of senders back into text, using the
/// redundant copies to fill in lost packets.
#[derive(Debug, Default)]
pub struct Depacketizer {
    t140_pt: u8,
    red_pt: Option<u8>,
    /// Next expected sequence number per SSRC.
    expected: HashMap<u32, u16>,
}

/// What a packet yielded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// New text (possibly with U+FFFD where text was lost for good).
    Text(String),
    /// Nothing new: a duplicate, a late packet, or an idle keep-going packet.
    Nothing,
    /// Not text we understand (another payload type).
    Unknown(u8),
}

impl Depacketizer {
    pub fn new(t140_pt: u8, red_pt: Option<u8>) -> Self {
        Self {
            t140_pt,
            red_pt,
            expected: HashMap::new(),
        }
    }

    pub fn forget(&mut self, ssrc: u32) {
        self.expected.remove(&ssrc);
    }

    pub fn packet(&mut self, pkt: &RtpPacket) -> Decoded {
        let h = pkt.header;
        let is_red = Some(h.payload_type) == self.red_pt;
        if !is_red && h.payload_type != self.t140_pt {
            return Decoded::Unknown(h.payload_type);
        }
        // Packets missed since the last one (0 for the first, or in order).
        let missed = match self.expected.get(&h.ssrc) {
            None => 0,
            Some(&e) => match h.sequence.wrapping_sub(e) as i16 {
                d if d < 0 => return Decoded::Nothing, // late or duplicate
                d if d > 3000 => 0,                    // restarted numbering
                d => d as usize,
            },
        };
        self.expected.insert(h.ssrc, h.sequence.wrapping_add(1));

        let mut text = Vec::new();
        if is_red {
            let Ok(red) = decode_red(pkt.payload) else {
                return Decoded::Unknown(h.payload_type);
            };
            let blocks: Vec<&[u8]> = red
                .redundant
                .iter()
                .filter(|b| b.payload_type == self.t140_pt)
                .map(|b| b.data)
                .collect();
            // The last `n` redundant blocks are the primaries of the `n`
            // packets just before this one, oldest first.
            let recoverable = missed.min(blocks.len());
            if missed > recoverable {
                text.extend_from_slice(MISSING_TEXT.to_string().as_bytes());
            }
            for b in &blocks[blocks.len() - recoverable..] {
                text.extend_from_slice(b);
            }
            if red.primary_type == self.t140_pt {
                text.extend_from_slice(red.primary);
            }
        } else {
            if missed > 0 {
                text.extend_from_slice(MISSING_TEXT.to_string().as_bytes());
            }
            text.extend_from_slice(pkt.payload);
        }
        let text = t140::from_t140(&String::from_utf8_lossy(&text));
        if text.is_empty() {
            Decoded::Nothing
        } else {
            Decoded::Text(text)
        }
    }
}

#[derive(Debug, Clone)]
pub struct TextReceiverConfig {
    pub net: MulticastOptions,
    pub t140_pt: u8,
    /// RFC 2198 payload type; `None` accepts only plain T.140.
    pub red_pt: Option<u8>,
    /// Ignore this SSRC (our own text, when chatting).
    pub ignore_ssrc: Option<u32>,
    /// Listen for RTCP (for senders' names and BYE).
    pub rtcp: Option<RtcpConfig>,
    /// Also send receiver reports.
    pub send_reports: bool,
    pub duration: Option<Duration>,
}

pub enum Event<'a> {
    NewSource {
        ssrc: u32,
        from: SocketAddr,
    },
    Text {
        ssrc: u32,
        text: &'a str,
    },
    /// A sender's RTCP name (SDES NAME, else CNAME), when first learned or changed.
    Named {
        ssrc: u32,
        name: String,
    },
    /// A sender said goodbye.
    Bye {
        ssrc: u32,
        reason: Option<String>,
    },
    /// Packets of a payload type we don't decode (reported once per type).
    UnknownPayloadType {
        payload_type: u8,
    },
    Warning {
        message: String,
    },
    /// Called about every 200 ms, for flushing idle partial lines.
    Tick,
}

enum Rtcp {
    /// Hear senders' names and goodbyes only.
    Listen(Listener),
    /// Also send receiver reports.
    Report(ReceiverRtcp),
}

struct Source {
    reception: ReceptionStats,
    last_sr: Option<(u64, Instant)>,
    name: Option<String>,
}

/// Receive until `stop` is set or `duration` elapses.
pub fn run(
    cfg: TextReceiverConfig,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Event<'_>),
) -> Result<()> {
    let socket = open_receiver(&cfg.net)
        .with_context(|| format!("joining multicast group {}", cfg.net.group))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut rtcp = match &cfg.rtcp {
        Some(r) => {
            let opened = if cfg.send_reports {
                ReceiverRtcp::open(&cfg.net, r.name.as_deref(), r.interval).map(Rtcp::Report)
            } else {
                rtcp_options(&cfg.net)
                    .ok_or_else(|| std::io::Error::other("no port for RTCP"))
                    .and_then(|o| Listener::start(&o))
                    .map(Rtcp::Listen)
            };
            match opened {
                Ok(r) => Some(r),
                Err(e) => {
                    on_event(Event::Warning {
                        message: format!("no RTCP: {e}"),
                    });
                    None
                }
            }
        }
        None => None,
    };

    let start = Instant::now();
    let mut depacketizer = Depacketizer::new(cfg.t140_pt, cfg.red_pt);
    let mut sources: HashMap<u32, Source> = HashMap::new();
    // Names from RTCP, including senders whose text hasn't arrived yet.
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut warned = Vec::new();
    let mut buf = vec![0u8; 65_536];

    while !stop.load(Ordering::Relaxed) && cfg.duration.map_or(true, |d| start.elapsed() < d) {
        on_event(Event::Tick);
        drain_rtcp(
            &rtcp,
            &mut sources,
            &mut names,
            cfg.ignore_ssrc,
            &mut on_event,
        );
        if let Some(Rtcp::Report(r)) = &mut rtcp {
            if r.due() {
                let blocks = sources
                    .iter_mut()
                    .filter_map(|(&ssrc, s)| {
                        s.reception
                            .report(ssrc, s.last_sr.map(|(ntp, at)| (ntp, at.elapsed())))
                    })
                    .collect();
                r.report(blocks, None);
            }
        }

        let (len, from) = match socket.recv_from(&mut buf) {
            Ok(r) => r,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(e) => return Err(e).context("receiving"),
        };
        let data = &buf[..len];
        if looks_like_rtcp(data) {
            continue;
        }
        let Ok(pkt) = rtp::parse(data) else { continue };
        let ssrc = pkt.header.ssrc;
        if Some(ssrc) == cfg.ignore_ssrc {
            continue;
        }
        // Senders report their name just before a burst; pick it up first.
        drain_rtcp(
            &rtcp,
            &mut sources,
            &mut names,
            cfg.ignore_ssrc,
            &mut on_event,
        );
        let decoded = depacketizer.packet(&pkt);
        if let Decoded::Unknown(payload_type) = decoded {
            if !warned.contains(&payload_type) {
                warned.push(payload_type);
                on_event(Event::UnknownPayloadType { payload_type });
            }
            continue;
        }
        let source = sources.entry(ssrc).or_insert_with(|| {
            on_event(Event::NewSource { ssrc, from });
            let name = names.get(&ssrc).cloned();
            if let Some(name) = &name {
                on_event(Event::Named {
                    ssrc,
                    name: name.clone(),
                });
            }
            Source {
                reception: ReceptionStats::new(),
                last_sr: None,
                name,
            }
        });
        let arrival = start.elapsed().as_secs_f64() * CLOCK_RATE as f64;
        source
            .reception
            .packet(pkt.header.sequence, pkt.header.timestamp, arrival);
        if let Decoded::Text(text) = decoded {
            on_event(Event::Text { ssrc, text: &text });
        }
    }
    Ok(())
}

fn drain_rtcp(
    rtcp: &Option<Rtcp>,
    sources: &mut HashMap<u32, Source>,
    names: &mut HashMap<u32, String>,
    ignore: Option<u32>,
    on_event: &mut impl FnMut(Event<'_>),
) {
    let listener = match rtcp {
        Some(Rtcp::Listen(l)) => l,
        Some(Rtcp::Report(r)) => &r.listener,
        None => return,
    };
    for m in listener.drain() {
        handle_rtcp(&m.packets, m.at, sources, names, ignore, on_event);
    }
}

fn handle_rtcp(
    packets: &[Packet],
    at: Instant,
    sources: &mut HashMap<u32, Source>,
    names: &mut HashMap<u32, String>,
    ignore: Option<u32>,
    on_event: &mut impl FnMut(Event<'_>),
) {
    for p in packets {
        match p {
            Packet::SenderReport { ssrc, info, .. } => {
                if let Some(s) = sources.get_mut(ssrc) {
                    s.last_sr = Some((info.ntp, at));
                }
            }
            Packet::Sdes(chunks) => {
                for c in chunks {
                    if Some(c.ssrc) == ignore {
                        continue;
                    }
                    let Some(name) = c.item(SDES_NAME).or(c.item(SDES_CNAME)) else {
                        continue;
                    };
                    if names.len() < 1000 {
                        names.insert(c.ssrc, name.to_string());
                    }
                    let Some(s) = sources.get_mut(&c.ssrc) else {
                        continue;
                    };
                    if s.name.as_deref() != Some(name) {
                        s.name = Some(name.to_string());
                        on_event(Event::Named {
                            ssrc: c.ssrc,
                            name: name.to_string(),
                        });
                    }
                }
            }
            Packet::Bye { ssrcs, reason } => {
                for ssrc in ssrcs {
                    names.remove(ssrc);
                    if sources.remove(ssrc).is_some() {
                        on_event(Event::Bye {
                            ssrc: *ssrc,
                            reason: reason.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t140::{encode_red, RedBlock};
    use mcast_core::rtp::RtpHeader;

    /// Packets as a sender with 2 generations would make them.
    fn stream(primaries: &[&str]) -> Vec<Vec<u8>> {
        let mut history = vec!["", ""];
        primaries
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut v = Vec::new();
                RtpHeader {
                    marker: false,
                    payload_type: 100,
                    sequence: 65_534u16.wrapping_add(i as u16),
                    timestamp: i as u32 * 300,
                    ssrc: 5,
                }
                .write_to(&mut v);
                let blocks: Vec<RedBlock> = history
                    .iter()
                    .map(|h| RedBlock {
                        payload_type: 98,
                        ts_offset: 300,
                        data: h.as_bytes(),
                    })
                    .collect();
                encode_red(&mut v, 98, &blocks, p.as_bytes());
                history.remove(0);
                history.push(p);
                v
            })
            .collect()
    }

    fn receive(packets: &[Vec<u8>], keep: impl Fn(usize) -> bool) -> String {
        let mut d = Depacketizer::new(98, Some(100));
        let mut out = String::new();
        for (i, p) in packets.iter().enumerate() {
            if keep(i) {
                if let Decoded::Text(t) = d.packet(&rtp::parse(p).unwrap()) {
                    out += &t;
                }
            }
        }
        out
    }

    #[test]
    fn recovers_up_to_two_lost_packets() {
        let packets = stream(&["he", "ll", "o\u{2028}", "", "wo", "rl", "d", ""]);
        assert_eq!(receive(&packets, |_| true), "hello\nworld");
        // Lose one, then two in a row (across the sequence wrap).
        assert_eq!(
            receive(&packets, |i| i != 1 && i != 4 && i != 5),
            "hello\nworld"
        );
        // Three in a row: the oldest ("ll") is gone for good and marked.
        assert_eq!(
            receive(&packets, |i| !(1..=3).contains(&i)),
            "he\u{FFFD}o\nworld"
        );
        // Duplicates add nothing.
        let mut dup = packets.clone();
        dup.insert(2, packets[1].clone());
        assert_eq!(receive(&dup, |_| true), "hello\nworld");
    }

    #[test]
    fn plain_t140_and_other_payload_types() {
        let mut d = Depacketizer::new(98, None);
        let mut v = Vec::new();
        RtpHeader {
            marker: true,
            payload_type: 98,
            sequence: 1,
            timestamp: 0,
            ssrc: 1,
        }
        .write_to(&mut v);
        v.extend_from_slice("hi\u{2028}".as_bytes());
        assert_eq!(
            d.packet(&rtp::parse(&v).unwrap()),
            Decoded::Text("hi\n".into())
        );
        v[1] = 0;
        assert_eq!(d.packet(&rtp::parse(&v).unwrap()), Decoded::Unknown(0));
    }
}
