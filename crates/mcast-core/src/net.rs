//! Multicast UDP socket setup for IPv4 and IPv6.

use socket2::{Domain, Protocol, Socket, Type};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::str::FromStr;

/// Which network interface to send on / join with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interface {
    /// Let the OS pick (default route).
    #[default]
    Any,
    /// IPv4 interface address (IPv4 groups only).
    V4(Ipv4Addr),
    /// Interface index (required form for IPv6 groups).
    Index(u32),
}

impl FromStr for Interface {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() || s.eq_ignore_ascii_case("any") {
            return Ok(Interface::Any);
        }
        if let Ok(a) = s.parse::<Ipv4Addr>() {
            return Ok(Interface::V4(a));
        }
        if let Ok(i) = s.parse::<u32>() {
            return Ok(Interface::Index(i));
        }
        Err(format!(
            "invalid interface '{s}': expected 'any', an IPv4 address, or an interface index"
        ))
    }
}

impl fmt::Display for Interface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Interface::Any => write!(f, "any"),
            Interface::V4(a) => write!(f, "{a}"),
            Interface::Index(i) => write!(f, "#{i}"),
        }
    }
}

/// Options common to multicast senders and receivers.
#[derive(Debug, Clone)]
pub struct MulticastOptions {
    pub group: SocketAddr,
    pub interface: Interface,
    /// IPv4 TTL / IPv6 hop limit for outgoing packets.
    pub ttl: u32,
    /// Deliver our own packets back to local receivers.
    pub loopback: bool,
}

impl MulticastOptions {
    pub fn new(group: SocketAddr) -> io::Result<Self> {
        if !group.ip().is_multicast() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a multicast address", group.ip()),
            ));
        }
        Ok(Self {
            group,
            interface: Interface::Any,
            ttl: 16,
            loopback: true,
        })
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// Open a UDP socket connected to the multicast group, ready for `send`.
pub fn open_sender(opts: &MulticastOptions) -> io::Result<UdpSocket> {
    let socket = Socket::new(
        Domain::for_address(opts.group),
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;
    match opts.group.ip() {
        IpAddr::V4(_) => {
            socket.set_multicast_ttl_v4(opts.ttl)?;
            socket.set_multicast_loop_v4(opts.loopback)?;
            match opts.interface {
                Interface::Any => {}
                Interface::V4(addr) => socket.set_multicast_if_v4(&addr)?,
                Interface::Index(_) => {
                    return Err(invalid(
                        "IPv4 groups need an interface address, not an index",
                    ))
                }
            }
            socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
        }
        IpAddr::V6(_) => {
            socket.set_multicast_hops_v6(opts.ttl)?;
            socket.set_multicast_loop_v6(opts.loopback)?;
            match opts.interface {
                Interface::Any => {}
                Interface::Index(i) => socket.set_multicast_if_v6(i)?,
                Interface::V4(_) => {
                    return Err(invalid(
                        "IPv6 groups need an interface index, not an IPv4 address",
                    ))
                }
            }
            socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into())?;
        }
    }
    socket.connect(&opts.group.into())?;
    Ok(socket.into())
}

/// Open a UDP socket bound to the group's port and joined to the group.
///
/// `SO_REUSEADDR` (and `SO_REUSEPORT` on Unix) are set so several
/// receivers on the same host can listen to the same group.
pub fn open_receiver(opts: &MulticastOptions) -> io::Result<UdpSocket> {
    let socket = Socket::new(
        Domain::for_address(opts.group),
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    // Best effort: a larger buffer rides out scheduling hiccups.
    let _ = socket.set_recv_buffer_size(1 << 20);

    let port = opts.group.port();
    match opts.group.ip() {
        IpAddr::V4(group) => {
            // On Unix, binding to the group address filters out unrelated
            // traffic to the same port. Windows requires the wildcard.
            let bind_ip = if cfg!(windows) {
                Ipv4Addr::UNSPECIFIED
            } else {
                group
            };
            socket.bind(&SocketAddr::from((bind_ip, port)).into())?;
            let iface = match opts.interface {
                Interface::Any => Ipv4Addr::UNSPECIFIED,
                Interface::V4(a) => a,
                Interface::Index(_) => {
                    return Err(invalid(
                        "IPv4 groups need an interface address, not an index",
                    ))
                }
            };
            socket.join_multicast_v4(&group, &iface)?;
            socket.set_multicast_loop_v4(opts.loopback)?;
        }
        IpAddr::V6(group) => {
            socket.set_only_v6(true)?;
            let bind_ip = if cfg!(windows) {
                Ipv6Addr::UNSPECIFIED
            } else {
                group
            };
            socket.bind(&SocketAddr::from((bind_ip, port)).into())?;
            let index = match opts.interface {
                Interface::Any => 0,
                Interface::Index(i) => i,
                Interface::V4(_) => {
                    return Err(invalid(
                        "IPv6 groups need an interface index, not an IPv4 address",
                    ))
                }
            };
            socket.join_multicast_v6(&group, index)?;
            socket.set_multicast_loop_v6(opts.loopback)?;
        }
    }
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_interfaces() {
        assert_eq!("any".parse::<Interface>().unwrap(), Interface::Any);
        assert_eq!(
            "10.0.0.5".parse::<Interface>().unwrap(),
            Interface::V4(Ipv4Addr::new(10, 0, 0, 5))
        );
        assert_eq!("3".parse::<Interface>().unwrap(), Interface::Index(3));
        assert!("eth0".parse::<Interface>().is_err());
    }

    #[test]
    fn rejects_unicast_group() {
        assert!(MulticastOptions::new("10.0.0.1:5004".parse().unwrap()).is_err());
        assert!(MulticastOptions::new("239.1.2.3:5004".parse().unwrap()).is_ok());
        assert!(MulticastOptions::new("[ff15::1]:5004".parse().unwrap()).is_ok());
    }
}
