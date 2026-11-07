//! Sends files to a multicast group as a stream of RaptorQ symbols.

use crate::session;
use crate::wire::{self, Meta, CLOCK_RATE, SYMBOL_SIZE};
use anyhow::{bail, Context, Result};
use mcast_core::net::{open_sender, MulticastOptions};
use mcast_core::random_u32;
use mcast_core::rtcp::{RtcpConfig, SenderRtcp};
use mcast_core::rtp::RtpHeader;
use mcast_core::sap::{self, Announcer};
use mcast_core::sdp::SessionDescription;
use raptorq::{Encoder, EncodingPacket, ObjectTransmissionInformation};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Send the file's metadata this often (in packets), so late joiners learn
/// the name.
const META_EVERY: u64 = 64;
/// Give receivers that discover us via SAP time to join first.
pub const SAP_LEAD: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct FileSenderConfig {
    pub net: MulticastOptions,
    pub payload_type: u8,
    /// Sending rate in kbit/s (UDP payload).
    pub rate_kbps: u32,
    /// Extra repair data to send on top of the file, as a fraction of its
    /// size: 0.5 sends 1.5x the file. Receivers can finish if they catch
    /// slightly more than the file's worth of any packets.
    pub redundancy: f64,
    /// Keep sending fresh repair packets until stopped (for late joiners).
    pub looped: bool,
    pub announce: Option<Announce>,
    pub rtcp: Option<RtcpConfig>,
}

#[derive(Debug, Clone)]
pub struct Announce {
    pub name: String,
    pub sap_group: Option<SocketAddr>,
    pub interval: Duration,
}

/// Progress through one file.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    /// Data packets sent so far.
    pub sent: u64,
    /// Packets that hold the file itself (receivers need about this many).
    pub source: u64,
    /// Packets this file will take (`None` when looping).
    pub total: Option<u64>,
}

pub struct FileSender {
    socket: UdpSocket,
    rtcp: Option<SenderRtcp>,
    _announcer: Option<Announcer>,
    sap_group: Option<SocketAddr>,
    sdp: SessionDescription,
    ssrc: u32,
    sequence: u16,
    ts0: u32,
    start: Instant,
    payload_type: u8,
    /// Bytes per second.
    rate: f64,
    redundancy: f64,
    looped: bool,
    /// For pacing: bytes sent since `pace_start`.
    paced_bytes: f64,
    pace_start: Instant,
    first: bool,
    ready_at: Instant,
    buf: Vec<u8>,
}

impl FileSender {
    pub fn new(cfg: FileSenderConfig) -> Result<Self> {
        let socket = open_sender(&cfg.net)
            .with_context(|| format!("opening multicast sender for {}", cfg.net.group))?;
        let origin = socket.local_addr()?.ip();
        let ssrc = random_u32();
        let ts0 = random_u32();
        let start = Instant::now();
        let name = cfg
            .announce
            .as_ref()
            .map_or("mcast files", |a| a.name.as_str());
        let sdp = session::describe(name, origin, cfg.net.group, cfg.net.ttl, cfg.payload_type);
        let rtcp = match &cfg.rtcp {
            Some(r) => {
                let mut s =
                    SenderRtcp::open(&cfg.net, ssrc, CLOCK_RATE, r.name.as_deref(), r.interval)
                        .context("opening RTCP sender (use --no-rtcp to skip)")?;
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
            payload_type: cfg.payload_type,
            rate: cfg.rate_kbps.max(8) as f64 * 1000.0 / 8.0,
            redundancy: cfg.redundancy.max(0.0),
            looped: cfg.looped,
            paced_bytes: 0.0,
            pace_start: start,
            first: true,
            ready_at: if sap_group.is_some() {
                start + SAP_LEAD
            } else {
                start
            },
            buf: Vec::with_capacity(1500),
        })
    }

    /// Keep sending repair packets for the next file until stopped.
    pub fn set_looped(&mut self, looped: bool) {
        self.looped = looped;
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

    fn send(&mut self, build: impl FnOnce(&mut Vec<u8>)) -> io::Result<()> {
        let now = Instant::now();
        if now < self.ready_at {
            std::thread::sleep(self.ready_at - now);
            self.pace_start = Instant::now();
            self.paced_bytes = 0.0;
        }
        let mut buf = std::mem::take(&mut self.buf);
        buf.clear();
        let now = Instant::now();
        RtpHeader {
            marker: self.first,
            payload_type: self.payload_type,
            sequence: self.sequence,
            timestamp: self
                .ts0
                .wrapping_add(now.duration_since(self.start).as_millis() as u32),
            ssrc: self.ssrc,
        }
        .write_to(&mut buf);
        build(&mut buf);

        // Pace to the configured rate.
        let due = self.pace_start + Duration::from_secs_f64(self.paced_bytes / self.rate);
        if let Some(wait) = due.checked_duration_since(now) {
            std::thread::sleep(wait);
        } else if now.duration_since(due) > Duration::from_millis(200) {
            // We fell behind (e.g. encoding); don't burst to catch up.
            self.pace_start = now;
            self.paced_bytes = 0.0;
        }
        self.paced_bytes += buf.len() as f64;

        let result = match self.socket.send(&buf) {
            Ok(_) => Ok(()),
            // A full socket buffer just loses this packet; the code covers it.
            Err(e) if e.raw_os_error() == Some(105) || e.kind() == io::ErrorKind::WouldBlock => {
                Ok(())
            }
            Err(e) => Err(e),
        };
        if let Some(r) = &mut self.rtcp {
            r.sent(&buf, Instant::now());
        }
        self.first = false;
        self.sequence = self.sequence.wrapping_add(1);
        self.buf = buf;
        result
    }

    fn send_meta(
        &mut self,
        object: u32,
        oti: &ObjectTransmissionInformation,
        meta: &Meta,
    ) -> io::Result<()> {
        self.send(|b| wire::write_meta(b, object, oti, meta))
    }

    /// Send one file. Returns the number of data packets sent.
    pub fn send_file(
        &mut self,
        name: &str,
        data: &[u8],
        stop: &AtomicBool,
        mut progress: impl FnMut(Progress),
    ) -> Result<u64> {
        if data.is_empty() {
            bail!("{name} is empty");
        }
        let encoder = Encoder::with_defaults(data, SYMBOL_SIZE);
        let oti = encoder.get_config();
        let object = random_u32();
        let meta = Meta {
            crc32: crc32fast::hash(data),
            name: name.to_string(),
            content_type: wire::content_type_for(name).to_string(),
        };
        let blocks = encoder.get_block_encoders();
        let sources: Vec<Vec<EncodingPacket>> = blocks.iter().map(|b| b.source_packets()).collect();
        let source_count: u64 = sources.iter().map(|s| s.len() as u64).sum();
        let total =
            (!self.looped).then(|| (source_count as f64 * (1.0 + self.redundancy)).ceil() as u64);

        self.send_meta(object, &oti, &meta)?;
        let mut sent = 0u64;
        let mut after = |s: &mut Self, sent: &mut u64, pkt: &EncodingPacket| -> Result<bool> {
            s.send(|b| wire::write_data(b, object, &oti, pkt))?;
            *sent += 1;
            if *sent % META_EVERY == 0 {
                s.send_meta(object, &oti, &meta)?;
                progress(Progress {
                    sent: *sent,
                    source: source_count,
                    total,
                });
            }
            Ok(stop.load(Ordering::Relaxed) || total.is_some_and(|t| *sent >= t))
        };

        // The file itself, interleaved across blocks so a burst of loss is
        // spread over them.
        let longest = sources.iter().map(Vec::len).max().unwrap_or(0);
        'outer: for i in 0..longest {
            for block in &sources {
                if let Some(pkt) = block.get(i) {
                    if after(self, &mut sent, pkt)? {
                        break 'outer;
                    }
                }
            }
        }
        // Then fresh repair packets, as many as asked (or until stopped).
        let mut next_repair = vec![0u32; blocks.len()];
        const BATCH: u32 = 32;
        while !stop.load(Ordering::Relaxed) && total.map_or(true, |t| sent < t) {
            let batches: Vec<Vec<EncodingPacket>> = blocks
                .iter()
                .zip(&mut next_repair)
                .map(|(b, next)| {
                    let p = b.repair_packets(*next, BATCH);
                    *next += BATCH;
                    p
                })
                .collect();
            'batch: for i in 0..BATCH as usize {
                for batch in &batches {
                    if after(self, &mut sent, &batch[i])? {
                        break 'batch;
                    }
                }
            }
        }
        self.send_meta(object, &oti, &meta)?;
        progress(Progress {
            sent,
            source: source_count,
            total,
        });
        Ok(sent)
    }
}
