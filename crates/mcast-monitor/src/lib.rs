//! Watch multicast streams.
//!
//! Joins the given groups (and, optionally, every stream announced with SAP)
//! and reports, per source: packet and bit rate, RTP loss and reordering,
//! interarrival jitter (RFC 3550), packet time, received TTL and DSCP, and
//! for audio codecs we can decode, the audio level. Non-RTP traffic is
//! counted too.
//!
//! It also listens on each group's RTCP port (RTP port + 1): sender reports
//! give a heartbeat, the one-way delay and media clock drift; receiver
//! reports show who is listening, with their loss, jitter and (for
//! mcast-tools receivers) how far behind the sender they play.

mod rtcp;
mod stats;

pub use rtcp::{ListenerReport, SenderRtcp};
pub use stats::{dscp_name, StreamReport};

use mcast_audio::{StreamInfo, StreamSpec};
use mcast_core::net::{
    enable_packet_info, open_receiver, recv_with_info, Interface, MulticastOptions,
};
use mcast_core::rtcp::{ntp_now, rtcp_addr, Packet};
use mcast_core::sap::{Directory, DirectoryEvent};
use rtcp::Participant;
use stats::{StreamKey, StreamState};
use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// A group to watch.
#[derive(Debug, Clone)]
pub struct Target {
    pub group: SocketAddr,
    /// Session name (from SAP).
    pub name: Option<String>,
    /// How to decode the audio, for level metering.
    pub spec: Option<StreamSpec>,
    /// RTP clock rate, if known (otherwise estimated from the stream).
    pub clock_rate: Option<u32>,
    /// What a non-audio stream carries ("T.140 text", "files"), from SAP.
    /// These are bursty: silence between messages or files is normal.
    pub kind: Option<&'static str>,
}

impl Target {
    pub fn new(group: SocketAddr) -> Self {
        Self {
            group,
            name: None,
            spec: None,
            clock_rate: None,
            kind: None,
        }
    }

    /// The stream an SAP announcement describes: audio, text or files.
    fn from_sdp(sdp: &mcast_core::sdp::SessionDescription) -> Option<Self> {
        let named = |n: &str| (!n.is_empty()).then(|| n.to_string());
        if let Some(info) = StreamInfo::from_sdp(sdp) {
            return Some(Self {
                group: info.group,
                name: named(&info.name),
                spec: info.spec(),
                clock_rate: Some(info.sample_rate),
                kind: None,
            });
        }
        if let Some(info) = mcast_text::TextInfo::from_sdp(sdp) {
            return Some(Self {
                group: info.group,
                name: named(&info.name),
                spec: None,
                clock_rate: Some(mcast_text::t140::CLOCK_RATE),
                kind: Some("T.140 text"),
            });
        }
        let info = mcast_file::FileInfo::from_sdp(sdp)?;
        Some(Self {
            group: info.group,
            name: named(&info.name),
            spec: None,
            clock_rate: Some(mcast_file::wire::CLOCK_RATE),
            kind: Some("files"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub targets: Vec<Target>,
    /// Also watch every stream announced on these SAP groups.
    pub sap: Option<Vec<SocketAddr>>,
    pub interface: Interface,
    /// Codec to assume for dynamic payload types with no SDP.
    pub default_spec: Option<StreamSpec>,
}

/// State of a watched group.
#[derive(Debug, Clone)]
pub struct GroupStatus {
    pub group: SocketAddr,
    pub name: Option<String>,
    pub from_sap: bool,
    /// Text or files (see [`Target::kind`]): idle periods are normal.
    pub kind: Option<&'static str>,
    /// Announcement withdrawn (SAP deletion or timeout).
    pub ended: bool,
    pub error: Option<String>,
}

enum Event {
    Packet {
        group: usize,
        data: Vec<u8>,
        from: SocketAddr,
        ttl: Option<u8>,
        tos: Option<u8>,
        at: Instant,
    },
    Rtcp {
        group: usize,
        packets: Vec<Packet>,
        from: SocketAddr,
        /// Local wall-clock arrival time (NTP format).
        ntp: u64,
        at: Instant,
    },
    Sap(Box<DirectoryEvent>),
    JoinError {
        group: usize,
        error: String,
    },
}

struct Group {
    target: Target,
    from_sap: bool,
    ended: bool,
    error: Option<String>,
    stop: Arc<AtomicBool>,
}

pub struct Monitor {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    interface: Interface,
    default_spec: Option<StreamSpec>,
    groups: Vec<Group>,
    streams: BTreeMap<StreamKey, StreamState>,
    /// RTCP participants by (group, SSRC).
    participants: BTreeMap<(usize, u32), Participant>,
    started: Instant,
    last_report: Instant,
    sap_stop: Arc<AtomicBool>,
}

impl Monitor {
    pub fn start(cfg: MonitorConfig) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        let mut m = Self {
            tx,
            rx,
            interface: cfg.interface,
            default_spec: cfg.default_spec,
            groups: Vec::new(),
            streams: BTreeMap::new(),
            participants: BTreeMap::new(),
            started: now,
            last_report: now,
            sap_stop: Arc::new(AtomicBool::new(false)),
        };
        for t in cfg.targets {
            m.add_group(t, false)?;
        }
        if let Some(sap_groups) = cfg.sap {
            let mut dir = Directory::open(&sap_groups, cfg.interface, Duration::from_secs(300))?;
            let (tx, stop) = (m.tx.clone(), m.sap_stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match dir.poll(Duration::from_millis(500)) {
                        Ok(events) => {
                            for e in events {
                                if tx.send(Event::Sap(Box::new(e))).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(_) => thread::sleep(Duration::from_millis(500)),
                    }
                }
            });
        }
        Ok(m)
    }

    fn add_group(&mut self, target: Target, from_sap: bool) -> io::Result<()> {
        let index = self.groups.len();
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = MulticastOptions::new(target.group)?;
        opts.interface = self.interface;
        if let Some(group) = rtcp_addr(target.group) {
            let mut rtcp = opts.clone();
            rtcp.group = group;
            listen(rtcp, index, true, self.tx.clone(), stop.clone());
        }
        listen(opts, index, false, self.tx.clone(), stop.clone());
        self.groups.push(Group {
            target,
            from_sap,
            ended: false,
            error: None,
            stop,
        });
        Ok(())
    }

    /// Process incoming packets and announcements until `until`.
    pub fn poll(&mut self, until: Instant) {
        loop {
            let wait = until.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(wait) {
                Ok(e) => self.handle(e),
                Err(_) => return, // timeout (or all senders gone)
            }
            if Instant::now() >= until {
                // Drain what's already queued without waiting.
                while let Ok(e) = self.rx.try_recv() {
                    self.handle(e);
                }
                return;
            }
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Packet {
                group,
                data,
                from,
                ttl,
                tos,
                at,
            } => {
                let g = &self.groups[group];
                let key = StreamKey {
                    group,
                    source: from,
                };
                let (announced, fallback) = (g.target.spec, self.default_spec);
                let clock = g.target.clock_rate;
                let started = self.started;
                self.streams
                    .entry(key)
                    .or_insert_with(|| StreamState::new(at, announced, fallback, clock))
                    .packet(&data, ttl, tos, at, started);
            }
            Event::Rtcp {
                group,
                packets,
                from,
                ntp,
                at,
            } => {
                // Each packet in a compound may come from a different SSRC
                // (SDES chunks), but in practice it's the sender's own.
                let Some(ssrc) = packets.iter().find_map(packet_ssrc) else {
                    return;
                };
                let p = self
                    .participants
                    .entry((group, ssrc))
                    .or_insert_with(|| Participant::new(from, at));
                for packet in &packets {
                    p.update(ssrc, packet, from, ntp, at);
                }
            }
            Event::JoinError { group, error } => self.groups[group].error = Some(error),
            Event::Sap(e) => self.handle_sap(*e),
        }
    }

    fn handle_sap(&mut self, event: DirectoryEvent) {
        match event {
            DirectoryEvent::Added(s) | DirectoryEvent::Changed(s) => {
                let Some(target) = Target::from_sdp(&s.sdp) else {
                    return;
                };
                if let Some(g) = self
                    .groups
                    .iter_mut()
                    .find(|g| g.target.group == target.group)
                {
                    // Known group: fill in what the announcement tells us.
                    g.target.name = g.target.name.take().or(target.name);
                    g.target.spec = g.target.spec.or(target.spec);
                    g.target.clock_rate = g.target.clock_rate.or(target.clock_rate);
                    g.target.kind = g.target.kind.or(target.kind);
                    g.ended = false;
                    return;
                }
                if let Err(e) = self.add_group(target, true) {
                    if let Some(g) = self.groups.last_mut() {
                        g.error = Some(e.to_string());
                    }
                }
            }
            DirectoryEvent::Deleted(s) | DirectoryEvent::Expired(s) => {
                let Some(target) = Target::from_sdp(&s.sdp) else {
                    return;
                };
                if let Some(g) = self
                    .groups
                    .iter_mut()
                    .find(|g| g.target.group == target.group && g.from_sap)
                {
                    g.ended = true;
                    g.stop.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn groups(&self) -> Vec<GroupStatus> {
        self.groups
            .iter()
            .map(|g| GroupStatus {
                group: g.target.group,
                name: g.target.name.clone(),
                from_sap: g.from_sap,
                kind: g.target.kind,
                ended: g.ended,
                error: g.error.clone(),
            })
            .collect()
    }

    /// Statistics since the previous report, one row per source.
    pub fn report(&mut self) -> Vec<StreamReport> {
        let now = Instant::now();
        let interval = now.duration_since(self.last_report);
        self.last_report = now;
        let rows = self.rows(|s, base| s.take_interval(base, interval, now));
        // Forget sources of withdrawn groups once reported.
        let ended: Vec<usize> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.ended)
            .map(|(i, _)| i)
            .collect();
        self.streams.retain(|k, s| {
            !ended.contains(&k.group) || now.duration_since(s.last_seen) < Duration::from_secs(5)
        });
        self.participants.retain(|_, p| !p.expired(now));
        rows
    }

    /// Statistics since the monitor started.
    pub fn totals(&self) -> Vec<StreamReport> {
        let elapsed = self.started.elapsed();
        let now = Instant::now();
        self.streams
            .iter()
            .map(|(k, s)| {
                let mut r = s.totals(self.base(k), elapsed, now);
                r.rtcp = self.sender_rtcp(k.group, r.ssrc, now);
                r
            })
            .collect()
    }

    /// Receivers heard on each group's RTCP port.
    pub fn listeners(&self) -> Vec<ListenerReport> {
        let now = Instant::now();
        self.participants
            .iter()
            .filter(|(_, p)| !p.is_sender())
            .map(|(&(group, ssrc), p)| {
                p.listener_report(
                    self.groups[group].target.group,
                    ssrc,
                    |about| self.clock(group, about),
                    now,
                )
            })
            .collect()
    }

    /// RTP clock rate of the stream `ssrc` on `group`.
    fn clock(&self, group: usize, ssrc: u32) -> Option<f64> {
        self.streams
            .iter()
            .filter(|(k, _)| k.group == group)
            .find(|(_, s)| s.ssrc() == Some(ssrc))
            .and_then(|(_, s)| s.clock())
            .or(self.groups[group].target.clock_rate.map(f64::from))
    }

    fn sender_rtcp(&self, group: usize, ssrc: Option<u32>, now: Instant) -> Option<SenderRtcp> {
        let ssrc = ssrc?;
        self.participants
            .get(&(group, ssrc))?
            .sender_report(self.clock(group, ssrc), now)
    }

    fn base(&self, k: &StreamKey) -> StreamReport {
        let g = &self.groups[k.group];
        StreamReport::new(g.target.group, g.target.name.clone(), k.source)
    }

    fn rows(
        &mut self,
        mut f: impl FnMut(&mut StreamState, StreamReport) -> StreamReport,
    ) -> Vec<StreamReport> {
        let keys: Vec<StreamKey> = self.streams.keys().cloned().collect();
        let now = Instant::now();
        keys.into_iter()
            .map(|k| {
                let base = self.base(&k);
                let mut r = f(self.streams.get_mut(&k).unwrap(), base);
                r.rtcp = self.sender_rtcp(k.group, r.ssrc, now);
                r
            })
            .collect()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.sap_stop.store(true, Ordering::Relaxed);
        for g in &self.groups {
            g.stop.store(true, Ordering::Relaxed);
        }
    }
}

fn packet_ssrc(p: &Packet) -> Option<u32> {
    match p {
        Packet::SenderReport { ssrc, .. }
        | Packet::ReceiverReport { ssrc, .. }
        | Packet::App { ssrc, .. } => Some(*ssrc),
        Packet::Bye { ssrcs, .. } => ssrcs.first().copied(),
        Packet::Sdes(chunks) => chunks.first().map(|c| c.ssrc),
        Packet::Other { .. } => None,
    }
}

/// Receive on a group in a background thread, until `stop`. RTP-port
/// errors are reported; the RTCP port is best effort.
fn listen(
    opts: MulticastOptions,
    group: usize,
    rtcp: bool,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let socket = match open_receiver(&opts).and_then(|s| {
            enable_packet_info(&s)?;
            s.set_read_timeout(Some(Duration::from_millis(200)))?;
            Ok(s)
        }) {
            Ok(s) => s,
            Err(e) => {
                if !rtcp {
                    let _ = tx.send(Event::JoinError {
                        group,
                        error: e.to_string(),
                    });
                }
                return;
            }
        };
        let mut buf = vec![0u8; 65_536];
        while !stop.load(Ordering::Relaxed) {
            match recv_with_info(&socket, &mut buf) {
                Ok(r) => {
                    let at = Instant::now();
                    // RTCP sharing the RTP port (RFC 5761 mux) is recognisable
                    // by its packet type.
                    let event = if rtcp || mcast_core::rtcp::looks_like_rtcp(&buf[..r.len]) {
                        let Ok(packets) = mcast_core::rtcp::parse(&buf[..r.len]) else {
                            continue;
                        };
                        Event::Rtcp {
                            group,
                            packets,
                            from: r.from,
                            ntp: ntp_now(),
                            at,
                        }
                    } else {
                        Event::Packet {
                            group,
                            data: buf[..r.len].to_vec(),
                            from: r.from,
                            ttl: r.ttl,
                            tos: r.tos,
                            at,
                        }
                    };
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) => {}
                Err(e) => {
                    if !rtcp {
                        let _ = tx.send(Event::JoinError {
                            group,
                            error: e.to_string(),
                        });
                    }
                    return;
                }
            }
        }
    });
}
