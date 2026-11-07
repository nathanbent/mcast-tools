//! SAP (RFC 2974): periodic multicast announcements of SDP session descriptions.
//!
//! [`Announcer`] advertises a session until dropped, then sends a deletion.
//! [`Directory`] listens for announcements and tracks which sessions exist.

use crate::net::{open_receiver_groups, open_sender, Interface, MulticastOptions};
use crate::random_u32;
use crate::sdp::{self, SessionDescription};
use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const SAP_PORT: u16 = 9875;
/// Administratively scoped "local" announcement group; what AES67/Dante use.
pub const SAP_GROUP_LOCAL: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 255);
/// Organization-local scope (239.192.0.0/14) announcement group.
pub const SAP_GROUP_ORG: Ipv4Addr = Ipv4Addr::new(239, 195, 255, 255);
/// RFC 2974 global-scope group; ffmpeg's `sap` muxer default.
pub const SAP_GROUP_GLOBAL: Ipv4Addr = Ipv4Addr::new(224, 2, 127, 254);

const SDP_MIME: &str = "application/sdp";

/// Groups a listener joins by default, covering common announcers.
pub fn default_listen_groups() -> Vec<SocketAddr> {
    [SAP_GROUP_LOCAL, SAP_GROUP_ORG, SAP_GROUP_GLOBAL]
        .into_iter()
        .map(|g| SocketAddr::from((g, SAP_PORT)))
        .collect()
}

/// Where to announce a stream on `group`: IPv4 uses the AES67 convention
/// (239.255.255.255) except for global-scope streams; IPv6 uses the
/// same-scope `ffXe::2:7ffe`.
pub fn announce_group_for(group: IpAddr) -> SocketAddr {
    let ip: IpAddr = match group {
        IpAddr::V4(g) if g.octets()[0] == 239 => SAP_GROUP_LOCAL.into(),
        IpAddr::V4(_) => SAP_GROUP_GLOBAL.into(),
        IpAddr::V6(g) => {
            let scope = g.segments()[0] & 0x000f;
            std::net::Ipv6Addr::new(0xff00 | scope, 0, 0, 0, 0, 0, 2, 0x7ffe).into()
        }
    };
    SocketAddr::new(ip, SAP_PORT)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SapPacket {
    pub deletion: bool,
    pub msg_id_hash: u16,
    pub origin: IpAddr,
    /// SDP text (for deletions, possibly just the `o=` line).
    pub payload: String,
}

impl SapPacket {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.payload.len());
        let mut flags = 0x20; // version 1
        if self.origin.is_ipv6() {
            flags |= 0x10;
        }
        if self.deletion {
            flags |= 0x04;
        }
        out.push(flags);
        out.push(0); // no authentication data
        out.extend_from_slice(&self.msg_id_hash.to_be_bytes());
        match self.origin {
            IpAddr::V4(a) => out.extend_from_slice(&a.octets()),
            IpAddr::V6(a) => out.extend_from_slice(&a.octets()),
        }
        out.extend_from_slice(SDP_MIME.as_bytes());
        out.push(0);
        out.extend_from_slice(self.payload.as_bytes());
        out
    }

    /// Parse a SAP packet. Encrypted, compressed and non-SDP packets are rejected.
    pub fn parse(buf: &[u8]) -> Result<Self, &'static str> {
        if buf.len() < 4 {
            return Err("too short");
        }
        let flags = buf[0];
        if flags >> 5 != 1 {
            return Err("unsupported SAP version");
        }
        if flags & 0x02 != 0 {
            return Err("encrypted");
        }
        if flags & 0x01 != 0 {
            return Err("compressed");
        }
        let ipv6 = flags & 0x10 != 0;
        let auth_len = buf[1] as usize * 4;
        let msg_id_hash = u16::from_be_bytes([buf[2], buf[3]]);
        let addr_len = if ipv6 { 16 } else { 4 };
        let mut pos = 4 + addr_len;
        if buf.len() < pos + auth_len {
            return Err("too short");
        }
        let origin: IpAddr = if ipv6 {
            <[u8; 16]>::try_from(&buf[4..20]).unwrap().into()
        } else {
            <[u8; 4]>::try_from(&buf[4..8]).unwrap().into()
        };
        pos += auth_len;

        let mut payload = &buf[pos..];
        // The payload-type field is optional; SDP starts with "v=0".
        if !payload.starts_with(b"v=0") && !payload.starts_with(b"o=") {
            let nul = payload
                .iter()
                .position(|&b| b == 0)
                .ok_or("missing payload type")?;
            if !payload[..nul].eq_ignore_ascii_case(SDP_MIME.as_bytes()) {
                return Err("payload is not SDP");
            }
            payload = &payload[nul + 1..];
        }
        Ok(SapPacket {
            deletion: flags & 0x04 != 0,
            msg_id_hash,
            origin,
            payload: String::from_utf8_lossy(payload).into_owned(),
        })
    }
}

/// Announces a session in the background until dropped, then sends a deletion.
pub struct Announcer {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Announcer {
    /// `net.group` is the SAP group to announce on (see [`announce_group_for`]).
    /// The origin address is taken from the SDP's `o=` line when it parses as
    /// an IP, else from the socket's local address.
    pub fn start(
        net: &MulticastOptions,
        sdp: &SessionDescription,
        interval: Duration,
    ) -> io::Result<Self> {
        let socket = open_sender(net)?;
        let origin = sdp
            .origin
            .address
            .parse()
            .or_else(|_| socket.local_addr().map(|a| a.ip()))?;
        let msg_id_hash = random_u32() as u16;
        let announce = SapPacket {
            deletion: false,
            msg_id_hash,
            origin,
            payload: sdp.to_string(),
        }
        .encode();
        let o = &sdp.origin;
        let family = if o.address.contains(':') {
            "IP6"
        } else {
            "IP4"
        };
        let delete = SapPacket {
            deletion: true,
            msg_id_hash,
            origin,
            payload: format!(
                "o={} {} {} IN {family} {}\r\n",
                o.username, o.session_id, o.version, o.address
            ),
        }
        .encode();

        // Send once up front so a failure (e.g. no route) is reported to the caller.
        socket.send(&announce)?;
        let (tx, rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            loop {
                // RFC 2974 asks for +/- 1/3 jitter so announcers don't synchronize.
                let jitter = (random_u32() % 667) as f64 / 1000.0 + 0.667;
                match rx.recv_timeout(interval.mul_f64(jitter)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let _ = socket.send(&announce);
                    }
                    _ => break,
                }
            }
            let _ = socket.send(&delete);
        });
        Ok(Self {
            stop: Some(tx),
            thread: Some(thread),
        })
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A session currently being announced.
#[derive(Debug, Clone)]
pub struct Session {
    pub sdp: SessionDescription,
    /// Origin address from the SAP header.
    pub origin: IpAddr,
    /// Source address the announcement arrived from.
    pub from: SocketAddr,
    pub msg_id_hash: u16,
    pub last_seen: Instant,
}

#[derive(Debug, Clone)]
pub enum DirectoryEvent {
    Added(Session),
    /// The SDP changed (new version or different content).
    Changed(Session),
    /// Explicitly deleted by the announcer.
    Deleted(Session),
    /// Not announced for longer than the expiry time.
    Expired(Session),
}

/// SAP origin + message hash + SDP origin. The SDP `o=` line alone isn't
/// unique in practice (ffmpeg announces every session as `o=- 0 0 ... 127.0.0.1`).
type Key = (IpAddr, u16, (String, String, String));

/// Listens for SAP announcements and keeps track of live sessions.
pub struct Directory {
    socket: std::net::UdpSocket,
    sessions: HashMap<Key, Session>,
    expiry: Duration,
    buf: Vec<u8>,
}

impl Directory {
    /// Join `groups` (see [`default_listen_groups`]). Sessions not re-announced
    /// within `expiry` are reported as expired.
    pub fn open(groups: &[SocketAddr], interface: Interface, expiry: Duration) -> io::Result<Self> {
        let socket = open_receiver_groups(groups, interface, true)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        Ok(Self {
            socket,
            sessions: HashMap::new(),
            expiry,
            buf: vec![0; 65_536],
        })
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    /// Receive announcements for up to `wait`, returning what changed.
    /// Returns early as soon as there is at least one event.
    pub fn poll(&mut self, wait: Duration) -> io::Result<Vec<DirectoryEvent>> {
        let deadline = Instant::now() + wait;
        let mut events = Vec::new();
        loop {
            match self.socket.recv_from(&mut self.buf) {
                Ok((len, from)) => {
                    if let Ok(pkt) = SapPacket::parse(&self.buf[..len]) {
                        self.handle(pkt, from, &mut events);
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
            let now = Instant::now();
            let expiry = self.expiry;
            let expired: Vec<Key> = self
                .sessions
                .iter()
                .filter(|(_, s)| now.duration_since(s.last_seen) > expiry)
                .map(|(k, _)| k.clone())
                .collect();
            for k in expired {
                if let Some(s) = self.sessions.remove(&k) {
                    events.push(DirectoryEvent::Expired(s));
                }
            }
            if !events.is_empty() || now >= deadline {
                return Ok(events);
            }
        }
    }

    fn handle(&mut self, pkt: SapPacket, from: SocketAddr, events: &mut Vec<DirectoryEvent>) {
        if pkt.deletion {
            // Match on (origin, hash); fall back to the o= line for announcers
            // that change the hash between announcement and deletion.
            let o_key = pkt
                .payload
                .lines()
                .find_map(|l| l.trim().strip_prefix("o="))
                .and_then(sdp::parse_origin)
                .map(|o| o.key());
            let key = self
                .sessions
                .keys()
                .find(|k| k.0 == pkt.origin && k.1 == pkt.msg_id_hash)
                .or_else(|| {
                    self.sessions
                        .keys()
                        .find(|k| k.0 == pkt.origin && Some(&k.2) == o_key.as_ref())
                })
                .cloned();
            if let Some(s) = key.and_then(|k| self.sessions.remove(&k)) {
                events.push(DirectoryEvent::Deleted(s));
            }
            return;
        }
        let Ok(sdp) = sdp::parse(&pkt.payload) else {
            return;
        };
        let key = (pkt.origin, pkt.msg_id_hash, sdp.origin.key());
        let session = Session {
            sdp,
            origin: pkt.origin,
            from,
            msg_id_hash: pkt.msg_id_hash,
            last_seen: Instant::now(),
        };
        match self.sessions.insert(key, session.clone()) {
            None => events.push(DirectoryEvent::Added(session)),
            Some(old) if old.sdp != session.sdp => events.push(DirectoryEvent::Changed(session)),
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_roundtrip_v4_and_v6() {
        for origin in ["10.1.2.3", "fe80::1"] {
            let p = SapPacket {
                deletion: origin.contains(':'),
                msg_id_hash: 0xabcd,
                origin: origin.parse().unwrap(),
                payload: "v=0\r\no=- 1 1 IN IP4 10.1.2.3\r\n".into(),
            };
            assert_eq!(SapPacket::parse(&p.encode()).unwrap(), p);
        }
    }

    #[test]
    fn accepts_missing_payload_type_and_skips_auth() {
        // version 1, 1 word of auth data, no MIME type
        let mut buf = vec![0x20, 1, 0, 7, 10, 0, 0, 1, 0xde, 0xad, 0xbe, 0xef];
        buf.extend_from_slice(b"v=0\r\n");
        let p = SapPacket::parse(&buf).unwrap();
        assert_eq!(p.payload, "v=0\r\n");
        assert_eq!(p.msg_id_hash, 7);
    }

    #[test]
    fn rejects_encrypted_compressed_and_other_types() {
        assert!(SapPacket::parse(&[0x22, 0, 0, 0, 1, 2, 3, 4]).is_err());
        assert!(SapPacket::parse(&[0x21, 0, 0, 0, 1, 2, 3, 4]).is_err());
        let mut buf = vec![0x20, 0, 0, 0, 1, 2, 3, 4];
        buf.extend_from_slice(b"text/plain\0hi");
        assert!(SapPacket::parse(&buf).is_err());
    }

    #[test]
    fn tracks_sessions_with_identical_origin_lines_separately() {
        let mut dir = Directory {
            socket: std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
            sessions: HashMap::new(),
            expiry: Duration::from_secs(60),
            buf: Vec::new(),
        };
        let from: SocketAddr = "10.0.0.1:9875".parse().unwrap();
        let pkt = |hash, deletion, name: &str| SapPacket {
            deletion,
            msg_id_hash: hash,
            origin: "10.0.0.1".parse().unwrap(),
            payload: format!("v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns={name}\r\n"),
        };
        let mut ev = Vec::new();
        dir.handle(pkt(1, false, "A"), from, &mut ev);
        dir.handle(pkt(2, false, "B"), from, &mut ev);
        dir.handle(pkt(1, true, ""), from, &mut ev);
        let kinds: Vec<_> = ev
            .iter()
            .map(|e| match e {
                DirectoryEvent::Added(s) => format!("+{}", s.sdp.name),
                DirectoryEvent::Deleted(s) => format!("-{}", s.sdp.name),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["+A", "+B", "-A"]);
        assert_eq!(dir.sessions().count(), 1);
    }

    #[test]
    fn picks_announce_group_by_scope() {
        let g = |s: &str| announce_group_for(s.parse().unwrap()).to_string();
        assert_eq!(g("239.69.0.1"), "239.255.255.255:9875");
        assert_eq!(g("239.192.2.1"), "239.255.255.255:9875");
        assert_eq!(g("224.5.6.7"), "224.2.127.254:9875");
        assert_eq!(g("ff15::69"), "[ff05::2:7ffe]:9875");
    }
}
