//! Finding a server by its multicast beacon (see
//! [`mcast_control::probes::Beacon`]).
//!
//! A background thread listens on the beacon group. Each server heard is
//! checked before it's used: with a token, the server must prove it knows
//! that token (`GET /probe-hello`), so the probe never sends its token to a
//! server that doesn't already have it, and with several servers on a
//! network it picks the one it belongs to. The probe stays with its server
//! while that keeps announcing itself, and follows it if its address changes.

use crate::client::Client;
use mcast_control::probes::{hello_proof, Beacon, BEACON_INTERVAL_S};
use mcast_core::{Interface, MulticastOptions};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A server not heard for this long is given up for another.
const GONE: Duration = Duration::from_secs(BEACON_INTERVAL_S * 4);
/// How long before a server that failed the check is asked again.
const RECHECK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    pub group: SocketAddr,
    pub interface: Interface,
    /// Only servers whose name contains this (case-insensitive).
    pub name: Option<String>,
    /// The probe's token; servers must prove they know it.
    pub token: Option<String>,
}

/// A server heard on the beacon group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub url: String,
    pub name: String,
}

#[derive(Debug)]
struct Candidate {
    name: String,
    heard: Instant,
    /// `Some(true)` once it proved it knows the token (or there's none).
    checked: Option<(bool, Instant)>,
}

#[derive(Default)]
struct Shared {
    candidates: HashMap<String, Candidate>,
    current: Option<Found>,
    /// Why the last candidate was turned down, for the log.
    refused: Vec<String>,
}

pub struct Discovery {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Discovery {
    pub fn start(cfg: DiscoveryConfig) -> std::io::Result<Self> {
        let mut opts = MulticastOptions::new(cfg.group)?;
        opts.interface = cfg.interface;
        let socket = mcast_core::open_receiver(&opts)?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        let shared: Arc<Mutex<Shared>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (shared, stop) = (shared.clone(), stop.clone());
            std::thread::Builder::new()
                .name("mcast-discovery".into())
                .spawn(move || listen(socket, cfg, shared, stop))?
        };
        Ok(Self {
            shared,
            stop,
            thread: Some(thread),
        })
    }

    /// The server to use, if one has been found.
    pub fn current(&self) -> Option<Found> {
        self.lock().current.clone()
    }

    /// Messages about servers that were turned down since the last call.
    pub fn refused(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().refused)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn listen(
    socket: std::net::UdpSocket,
    cfg: DiscoveryConfig,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) {
    let lock = || shared.lock().unwrap_or_else(|e| e.into_inner());
    let mut buf = [0u8; 2048];
    let filter = cfg.name.as_ref().map(|n| n.to_lowercase());
    while !stop.load(Ordering::Relaxed) {
        let heard = match socket.recv_from(&mut buf) {
            Ok((n, from)) => Beacon::parse(&buf[..n]).map(|b| (b.server_url(from.ip()), b.name)),
            Err(_) => None,
        };
        let now = Instant::now();
        if let Some((url, name)) = heard {
            if filter
                .as_ref()
                .map_or(true, |f| name.to_lowercase().contains(f))
            {
                let mut s = lock();
                let c = s.candidates.entry(url.clone()).or_insert(Candidate {
                    name: name.clone(),
                    heard: now,
                    checked: None,
                });
                c.heard = now;
                c.name = name;
                let due = c
                    .checked
                    .map_or(true, |(ok, at)| !ok && now.duration_since(at) > RECHECK);
                if due {
                    drop(s);
                    // Outside the lock: this is an HTTP request.
                    let result = check(&url, cfg.token.as_deref());
                    let mut s = lock();
                    if let Err(why) = &result {
                        s.refused
                            .push(format!("ignoring the server at {url}: {why}"));
                    }
                    if let Some(c) = s.candidates.get_mut(&url) {
                        c.checked = Some((result.is_ok(), Instant::now()));
                    }
                }
            }
        }
        choose(&mut lock(), now);
    }
}

/// Keep the current server while it's heard; otherwise take the most
/// recently heard one that passed the check.
fn choose(s: &mut Shared, now: Instant) {
    s.candidates
        .retain(|_, c| now.duration_since(c.heard) < GONE * 3);
    let alive = |c: &Candidate| now.duration_since(c.heard) < GONE;
    let ok = |c: &Candidate| c.checked.is_some_and(|(ok, _)| ok);
    if let Some(cur) = &s.current {
        if s.candidates
            .get(&cur.url)
            .is_some_and(|c| alive(c) && ok(c))
        {
            return;
        }
        // The same server under a new address (DHCP, a second NIC) wins.
        let moved = s
            .candidates
            .iter()
            .filter(|(_, c)| alive(c) && ok(c) && c.name == cur.name)
            .max_by_key(|(_, c)| c.heard)
            .map(|(u, c)| Found {
                url: u.clone(),
                name: c.name.clone(),
            });
        if moved.is_some() {
            s.current = moved;
            return;
        }
    }
    let best = s
        .candidates
        .iter()
        .filter(|(_, c)| alive(c) && ok(c))
        .max_by_key(|(_, c)| c.heard)
        .map(|(u, c)| Found {
            url: u.clone(),
            name: c.name.clone(),
        });
    // With nothing better, keep the last one: a beacon outage alone
    // shouldn't stop check-ins to a server that still answers.
    if best.is_some() {
        s.current = best;
    }
}

/// Ask the server to prove it knows `token`.
fn check(url: &str, token: Option<&str>) -> Result<(), String> {
    let client = Client::new(url, None);
    let nonce = format!(
        "{:08x}{:08x}{:08x}{:08x}",
        mcast_core::random_u32(),
        mcast_core::random_u32(),
        mcast_core::random_u32(),
        mcast_core::random_u32()
    );
    let hello = client.hello(&nonce).map_err(|e| e.to_string())?;
    match token {
        None => Ok(()),
        Some(t) => {
            let want = hello_proof(t, &nonce);
            if hello.proofs.iter().any(|p| p.eq_ignore_ascii_case(&want)) {
                Ok(())
            } else {
                Err("it doesn't know this probe's token".into())
            }
        }
    }
}

/// Listen for `wait` and list the servers heard (for `mcast probe discover`).
pub fn listen_for(
    group: SocketAddr,
    interface: Interface,
    wait: Duration,
) -> std::io::Result<Vec<(Found, Beacon)>> {
    let mut opts = MulticastOptions::new(group)?;
    opts.interface = interface;
    let socket = mcast_core::open_receiver(&opts)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    let until = Instant::now() + wait;
    let mut found: Vec<(Found, Beacon)> = Vec::new();
    let mut buf = [0u8; 2048];
    while Instant::now() < until {
        if let Ok((n, from)) = socket.recv_from(&mut buf) {
            if let Some(b) = Beacon::parse(&buf[..n]) {
                let url = b.server_url(from.ip());
                if !found.iter().any(|(f, _)| f.url == url) {
                    found.push((
                        Found {
                            url,
                            name: b.name.clone(),
                        },
                        b,
                    ));
                }
            }
        }
    }
    Ok(found)
}
