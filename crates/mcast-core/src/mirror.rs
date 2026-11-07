//! Unicast copies of a multicast stream.
//!
//! A [`Mirror`] is a list of unicast destinations that can change while
//! senders run (probes come and go). Each sender keeps a [`MirrorSocket`]
//! and hands it every packet it sends to the group; the socket sends an
//! identical copy (same SSRC, sequence numbers and timestamps) to each
//! destination, so a receiver can compare the multicast and unicast paths
//! packet for packet.

use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Unicast destinations shared by every sender. Cheap to check per packet:
/// readers only take the lock when the version changes.
#[derive(Debug, Default)]
pub struct Mirror {
    targets: RwLock<Vec<SocketAddr>>,
    version: AtomicU64,
}

impl Mirror {
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Replace the destinations (no-op if unchanged).
    pub fn set(&self, mut targets: Vec<SocketAddr>) {
        targets.sort();
        targets.dedup();
        let mut t = self.targets.write().unwrap_or_else(|e| e.into_inner());
        if *t != targets {
            *t = targets;
            self.version.fetch_add(1, Ordering::Release);
        }
    }

    pub fn targets(&self) -> Vec<SocketAddr> {
        self.targets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

/// One sender's view of a [`Mirror`]: its own sockets, and a cached copy of
/// the destinations.
#[derive(Debug)]
pub struct MirrorSocket {
    mirror: Arc<Mirror>,
    tos: Option<u32>,
    seen: Option<u64>,
    targets: Vec<SocketAddr>,
    v4: Option<UdpSocket>,
    v6: Option<UdpSocket>,
    /// Copies that couldn't be sent (no route, full buffers...).
    pub errors: u64,
    pub sent: u64,
}

impl MirrorSocket {
    /// `tos` marks the copies like the multicast packets, so DSCP handling
    /// on the two paths is comparable.
    pub fn new(mirror: Arc<Mirror>, tos: Option<u32>) -> Self {
        Self {
            mirror,
            tos,
            seen: None,
            targets: Vec::new(),
            v4: None,
            v6: None,
            errors: 0,
            sent: 0,
        }
    }

    /// Send a copy of `pkt` to every destination. Never fails: the copy is
    /// for diagnosis and mustn't disturb the multicast stream.
    pub fn send(&mut self, pkt: &[u8]) {
        let v = self.mirror.version();
        if self.seen != Some(v) {
            self.seen = Some(v);
            self.targets = self.mirror.targets();
        }
        for i in 0..self.targets.len() {
            let to = self.targets[i];
            let ok = self
                .socket(to.is_ipv4())
                .is_some_and(|s| s.send_to(pkt, to).is_ok());
            if ok {
                self.sent += 1;
            } else {
                self.errors += 1;
            }
        }
    }

    fn socket(&mut self, v4: bool) -> Option<&UdpSocket> {
        let tos = self.tos;
        let slot = if v4 { &mut self.v4 } else { &mut self.v6 };
        if slot.is_none() {
            *slot = open(v4, tos).ok();
        }
        slot.as_ref()
    }
}

fn open(v4: bool, tos: Option<u32>) -> std::io::Result<UdpSocket> {
    let bind: SocketAddr = if v4 {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
    if let Some(tos) = tos {
        if v4 {
            let _ = socket.set_tos(tos);
        } else {
            #[cfg(any(
                target_os = "linux",
                target_os = "android",
                target_os = "macos",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ))]
            let _ = socket.set_tclass_v6(tos);
        }
    }
    // Copies to a vanished probe shouldn't block the sender.
    socket.set_nonblocking(true)?;
    socket.bind(&bind.into())?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn copies_reach_every_target_and_follow_changes() {
        let a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").unwrap();
        for s in [&a, &b] {
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        }
        let mirror = Mirror::new();
        let mut out = MirrorSocket::new(mirror.clone(), None);
        out.send(b"nobody");
        assert_eq!(out.sent, 0);

        mirror.set(vec![a.local_addr().unwrap()]);
        out.send(b"one");
        let mut buf = [0u8; 16];
        let n = a.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"one");

        mirror.set(vec![b.local_addr().unwrap(), a.local_addr().unwrap()]);
        out.send(b"two");
        for s in [&a, &b] {
            let n = s.recv(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"two");
        }
        assert_eq!(out.sent, 3);
    }
}
