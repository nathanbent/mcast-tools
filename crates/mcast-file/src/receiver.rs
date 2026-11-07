//! Receives files: collects RaptorQ symbols per object until it can decode.

use crate::wire::{self, Body, Meta, CLOCK_RATE};
use anyhow::{Context, Result};
use mcast_core::net::{open_receiver, MulticastOptions};
use mcast_core::rtcp::{looks_like_rtcp, Packet, ReceiverRtcp, ReceptionStats, RtcpConfig};
use mcast_core::rtp;
use raptorq::{Decoder, ObjectTransmissionInformation};
use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Forget an unfinished file after this long without packets for it.
pub const ABANDON_AFTER: Duration = Duration::from_secs(30);
/// Decode at most this many files at once (each holds up to its size in memory).
const MAX_ACTIVE: usize = 8;
/// After decoding, wait this long for the file's name if we haven't seen it.
const META_WAIT: Duration = Duration::from_secs(2);

/// A finished file.
#[derive(Debug, Clone)]
pub struct Completed {
    pub object: u32,
    pub from: SocketAddr,
    pub data: Vec<u8>,
    /// Name, CRC and type, if the metadata arrived.
    pub meta: Option<Meta>,
    /// The CRC-32 matched (`None` without metadata).
    pub verified: Option<bool>,
    /// Data packets received for this file, and how many hold the file itself.
    pub received: u64,
    pub source: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub enum Event {
    Started {
        object: u32,
        from: SocketAddr,
        size: u64,
        name: Option<String>,
    },
    /// The name arrived after the file started.
    Named {
        object: u32,
        name: String,
    },
    Progress {
        object: u32,
        received: u64,
        source: u64,
    },
    Completed(Box<Completed>),
    /// Gave up on a file (sender stopped before we had enough).
    Abandoned {
        object: u32,
        name: Option<String>,
        received: u64,
        source: u64,
    },
    /// Packets refused (too large, malformed), reported once per reason.
    Rejected {
        from: SocketAddr,
        reason: String,
    },
    Warning {
        message: String,
    },
}

struct Transfer {
    from: SocketAddr,
    oti: ObjectTransmissionInformation,
    decoder: Option<Decoder>,
    meta: Option<Meta>,
    received: u64,
    source: u64,
    started: Instant,
    last: Instant,
    /// Decoded, waiting for metadata.
    decoded: Option<(Vec<u8>, Instant)>,
    reported_progress: Instant,
}

/// Packets in, files out. Independent of the network for testing.
pub struct Assembler {
    max_size: u64,
    transfers: HashMap<u32, Transfer>,
    done: HashSet<u32>,
    rejected: HashSet<String>,
    progress_every: Duration,
}

impl Assembler {
    pub fn new(max_size: u64) -> Self {
        Self {
            max_size,
            transfers: HashMap::new(),
            done: HashSet::new(),
            rejected: HashSet::new(),
            progress_every: Duration::from_millis(500),
        }
    }

    /// Feed an RTP payload.
    pub fn payload(
        &mut self,
        payload: &[u8],
        from: SocketAddr,
        now: Instant,
        events: &mut Vec<Event>,
    ) {
        let pkt = match wire::parse(payload, self.max_size) {
            Ok(p) => p,
            Err(e) => {
                let reason = e.to_string();
                if self.rejected.insert(reason.clone()) {
                    events.push(Event::Rejected { from, reason });
                }
                return;
            }
        };
        let object = pkt.object;
        if self.done.contains(&object) {
            return; // already have it (sender still looping)
        }
        if !self.transfers.contains_key(&object) {
            if self.transfers.len() >= MAX_ACTIVE {
                return;
            }
            let oti = pkt.oti;
            let source = oti.transfer_length().div_ceil(oti.symbol_size() as u64);
            // A malformed-but-valid-looking OTI must not take us down.
            let Ok(decoder) = catch_unwind(|| Decoder::new(oti)) else {
                events.push(Event::Rejected {
                    from,
                    reason: "undecodable transmission parameters".into(),
                });
                self.done.insert(object);
                return;
            };
            let meta = match &pkt.body {
                Body::Meta(m) => Some(m.clone()),
                Body::Data(_) => None,
            };
            events.push(Event::Started {
                object,
                from,
                size: oti.transfer_length(),
                name: meta.as_ref().map(|m| m.name.clone()),
            });
            self.transfers.insert(
                object,
                Transfer {
                    from,
                    oti,
                    decoder: Some(decoder),
                    meta,
                    received: 0,
                    source,
                    started: now,
                    last: now,
                    decoded: None,
                    reported_progress: now,
                },
            );
        }
        let t = self.transfers.get_mut(&object).expect("just inserted");
        if pkt.oti != t.oti {
            return; // conflicting parameters for the same object id
        }
        t.last = now;
        match pkt.body {
            Body::Meta(m) => {
                if t.meta.is_none() {
                    events.push(Event::Named {
                        object,
                        name: m.name.clone(),
                    });
                }
                t.meta = Some(m);
            }
            Body::Data(p) => {
                t.received += 1;
                if let Some(decoder) = t.decoder.as_mut() {
                    match catch_unwind(AssertUnwindSafe(|| decoder.decode(p))) {
                        Ok(Some(data)) => {
                            t.decoder = None;
                            t.decoded = Some((data, now));
                        }
                        Ok(None) => {}
                        Err(_) => {
                            events.push(Event::Rejected {
                                from,
                                reason: "decoder failed on a packet".into(),
                            });
                            self.transfers.remove(&object);
                            self.done.insert(object);
                            return;
                        }
                    }
                }
                if t.decoder.is_some()
                    && now.duration_since(t.reported_progress) >= self.progress_every
                {
                    t.reported_progress = now;
                    events.push(Event::Progress {
                        object,
                        received: t.received,
                        source: t.source,
                    });
                }
            }
        }
        self.finish_ready(now, events);
    }

    /// Hand over decoded files (once their name is known, or after waiting
    /// for it) and give up on stalled ones. Call regularly.
    pub fn finish_ready(&mut self, now: Instant, events: &mut Vec<Event>) {
        let ready: Vec<u32> = self
            .transfers
            .iter()
            .filter(|(_, t)| {
                t.decoded
                    .as_ref()
                    .is_some_and(|(_, at)| t.meta.is_some() || now.duration_since(*at) >= META_WAIT)
            })
            .map(|(&id, _)| id)
            .collect();
        for object in ready {
            let t = self.transfers.remove(&object).expect("listed above");
            self.done.insert(object);
            let (mut data, _) = t.decoded.expect("filtered on decoded");
            data.truncate(t.oti.transfer_length() as usize);
            let verified = t.meta.as_ref().map(|m| crc32fast::hash(&data) == m.crc32);
            events.push(Event::Completed(Box::new(Completed {
                object,
                from: t.from,
                data,
                meta: t.meta,
                verified,
                received: t.received,
                source: t.source,
                elapsed: now.duration_since(t.started),
            })));
        }
        let stalled: Vec<u32> = self
            .transfers
            .iter()
            .filter(|(_, t)| t.decoded.is_none() && now.duration_since(t.last) >= ABANDON_AFTER)
            .map(|(&id, _)| id)
            .collect();
        for object in stalled {
            let t = self.transfers.remove(&object).expect("listed above");
            events.push(Event::Abandoned {
                object,
                name: t.meta.map(|m| m.name),
                received: t.received,
                source: t.source,
            });
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileReceiverConfig {
    pub net: MulticastOptions,
    pub payload_type: Option<u8>,
    /// Refuse files larger than this.
    pub max_size: u64,
    pub rtcp: Option<RtcpConfig>,
    pub duration: Option<Duration>,
}

/// Receive until `stop`, `duration`, or `on_event` returns false.
pub fn run(
    cfg: FileReceiverConfig,
    stop: &AtomicBool,
    mut on_event: impl FnMut(Event) -> bool,
) -> Result<()> {
    let socket = open_receiver(&cfg.net)
        .with_context(|| format!("joining multicast group {}", cfg.net.group))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut rtcp = match &cfg.rtcp {
        Some(r) => match ReceiverRtcp::open(&cfg.net, r.name.as_deref(), r.interval) {
            Ok(r) => Some(r),
            Err(e) => {
                on_event(Event::Warning {
                    message: format!("no RTCP: {e}"),
                });
                None
            }
        },
        None => None,
    };
    let start = Instant::now();
    let mut assembler = Assembler::new(cfg.max_size);
    let mut reception: HashMap<u32, (ReceptionStats, Option<(u64, Instant)>)> = HashMap::new();
    let mut events = Vec::new();
    let mut buf = vec![0u8; 65_536];

    while !stop.load(Ordering::Relaxed) && cfg.duration.map_or(true, |d| start.elapsed() < d) {
        if let Some(r) = &mut rtcp {
            for m in r.listener.drain() {
                for p in &m.packets {
                    if let Packet::SenderReport { ssrc, info, .. } = p {
                        if let Some(s) = reception.get_mut(ssrc) {
                            s.1 = Some((info.ntp, m.at));
                        }
                    }
                }
            }
            if r.due() {
                let blocks = reception
                    .iter_mut()
                    .filter_map(|(&ssrc, (s, sr))| {
                        s.report(ssrc, sr.map(|(ntp, at)| (ntp, at.elapsed())))
                    })
                    .collect();
                r.report(blocks, None);
            }
        }

        let now = Instant::now();
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                let data = &buf[..len];
                if !looks_like_rtcp(data) {
                    if let Ok(pkt) = rtp::parse(data) {
                        let h = pkt.header;
                        if cfg.payload_type.map_or(true, |pt| pt == h.payload_type) {
                            if reception.len() < 64 || reception.contains_key(&h.ssrc) {
                                let arrival = start.elapsed().as_secs_f64() * CLOCK_RATE as f64;
                                reception.entry(h.ssrc).or_default().0.packet(
                                    h.sequence,
                                    h.timestamp,
                                    arrival,
                                );
                            }
                            assembler.payload(pkt.payload, from, now, &mut events);
                        }
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e).context("receiving"),
        }
        assembler.finish_ready(now, &mut events);
        for e in events.drain(..) {
            if !on_event(e) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Make a received file name safe to create in the output directory: no
/// directories, no hidden files, no control characters.
pub fn safe_file_name(name: &str, object: u32) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_control() || c == ':' { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let cleaned: String = cleaned.chars().take(200).collect();
    if cleaned.is_empty() {
        format!("mcast-file-{object:08x}")
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{write_data, write_meta, SYMBOL_SIZE};
    use raptorq::Encoder;

    fn from() -> SocketAddr {
        "10.0.0.5:4000".parse().unwrap()
    }

    /// Payloads as the sender makes them: meta, source, then `repair` per block.
    fn payloads(data: &[u8], repair: u32) -> Vec<Vec<u8>> {
        let enc = Encoder::with_defaults(data, SYMBOL_SIZE);
        let oti = enc.get_config();
        let meta = Meta {
            crc32: crc32fast::hash(data),
            name: "../../etc/pic.png".into(),
            content_type: "image/png".into(),
        };
        let mut out = Vec::new();
        let mut b = Vec::new();
        write_meta(&mut b, 42, &oti, &meta);
        out.push(b);
        for p in enc.get_encoded_packets(repair) {
            let mut b = Vec::new();
            write_data(&mut b, 42, &oti, &p);
            out.push(b);
        }
        out
    }

    fn feed(a: &mut Assembler, packets: &[Vec<u8>]) -> Vec<Event> {
        let mut events = Vec::new();
        let now = Instant::now();
        for p in packets {
            a.payload(p, from(), now, &mut events);
        }
        events
    }

    fn completed(events: &[Event]) -> Vec<&Completed> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Completed(c) => Some(&**c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rebuilds_a_file_despite_30_percent_loss() {
        let data: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let all = payloads(&data, 100);
        // Drop every packet whose index is 0 or 1 mod 7 (about 29%), but keep the meta.
        let kept: Vec<Vec<u8>> = all
            .iter()
            .enumerate()
            .filter(|(i, _)| *i == 0 || !matches!(i % 7, 0 | 1))
            .map(|(_, p)| p.clone())
            .collect();
        let mut a = Assembler::new(1 << 30);
        let events = feed(&mut a, &kept);
        let done = completed(&events);
        assert_eq!(done.len(), 1, "{events:?}");
        assert_eq!(done[0].data, data);
        assert_eq!(done[0].verified, Some(true));
        assert_eq!(done[0].meta.as_ref().unwrap().content_type, "image/png");
        // Same object again (looping sender): ignored.
        assert!(completed(&feed(&mut a, &all)).is_empty());
    }

    #[test]
    fn waits_for_the_name_then_gives_up_waiting() {
        let data = vec![9u8; 10_000];
        let all = payloads(&data, 5);
        let mut a = Assembler::new(1 << 30);
        let mut events = Vec::new();
        let t0 = Instant::now();
        for p in &all[1..] {
            a.payload(p, from(), t0, &mut events);
        }
        assert!(completed(&events).is_empty(), "held for the name");
        a.finish_ready(t0 + META_WAIT, &mut events);
        let done = completed(&events);
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].verified, done[0].meta.is_none()), (None, true));
    }

    #[test]
    fn abandons_stalled_and_refuses_oversized() {
        let data = vec![1u8; 50_000];
        let all = payloads(&data, 0);
        let mut a = Assembler::new(1 << 30);
        let t0 = Instant::now();
        let mut events = Vec::new();
        for p in &all[..10] {
            a.payload(p, from(), t0, &mut events);
        }
        a.finish_ready(t0 + ABANDON_AFTER, &mut events);
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Abandoned { received: 9, .. })));

        let mut small = Assembler::new(1000);
        let events = feed(&mut small, &all[..3]);
        assert_eq!(events.len(), 1, "one rejection report: {events:?}");
        assert!(
            matches!(&events[0], Event::Rejected { reason, .. } if reason.contains("size limit"))
        );
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd", 1), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\x\\cat.jpg", 1), "cat.jpg");
        assert_eq!(safe_file_name(".bashrc", 1), "bashrc");
        assert_eq!(safe_file_name("a\nb\u{7}.png", 1), "a_b_.png");
        assert_eq!(safe_file_name("..", 0xab), "mcast-file-000000ab");
        assert_eq!(safe_file_name("", 1), "mcast-file-00000001");
    }
}
