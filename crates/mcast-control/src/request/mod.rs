//! What to send or receive, as plain data. The CLI builds these from its
//! flags and the API server deserializes them from JSON; both turn them into
//! library configs the same way.

//!
//! Shared pieces live here; each kind of stream has its own module.

mod audio;

pub use audio::*;

use anyhow::{bail, Result};
use mcast_core::{Interface, MulticastOptions};
use std::net::SocketAddr;
use std::time::Duration;

/// Network options shared by senders and receivers.
#[derive(Debug, Clone, PartialEq)]
pub struct NetRequest {
    /// `None` uses the caller's default.
    pub interface: Option<Interface>,
    pub ttl: u32,
    /// Deliver our own packets to receivers on this host.
    pub loopback: bool,
}

impl Default for NetRequest {
    fn default() -> Self {
        Self {
            interface: None,
            ttl: 16,
            loopback: true,
        }
    }
}

impl NetRequest {
    pub fn options(&self, group: SocketAddr, default_iface: Interface) -> Result<MulticastOptions> {
        let mut o = MulticastOptions::new(group)?;
        o.interface = self.interface.unwrap_or(default_iface);
        o.ttl = self.ttl;
        o.loopback = self.loopback;
        Ok(o)
    }
}

pub(crate) fn ms(v: f64) -> Duration {
    Duration::from_secs_f64(v.max(0.0) / 1000.0)
}

pub(crate) fn secs(v: f64, what: &str) -> Result<Duration> {
    if !v.is_finite() || v < 0.0 {
        bail!("{what} must be a non-negative number of seconds");
    }
    Ok(Duration::from_secs_f64(v))
}

/// This host's name, for default stream names.
pub fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "mcast".into())
}

/// Serde for `Option<T>` through `T`'s `FromStr` and `Display`.
pub(crate) mod opt_str {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    use std::fmt::Display;
    use std::str::FromStr;

    pub fn serialize<T: Display, S: Serializer>(v: &Option<T>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => s.collect_str(&v.to_string().to_lowercase()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
    where
        T: FromStr,
        T::Err: Display,
        D: Deserializer<'de>,
    {
        Option::<String>::deserialize(d)?
            .map(|s| s.parse().map_err(D::Error::custom))
            .transpose()
    }
}
