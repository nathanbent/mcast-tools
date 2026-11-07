//! The probe side of `mcast probe`: watch multicast streams (and the
//! server's unicast copies of them), and check in with an `mcast serve`
//! server every report interval with what was measured. The server's reply
//! says what to watch; see [`mcast_control::probes`] for the protocol.
//!
//! Reports that can't be delivered are queued (up to `max_queue`) and sent
//! once the server is reachable again, so an outage of the control path
//! doesn't lose the record of how multicast did meanwhile.

pub mod client;
pub mod discovery;

use anyhow::Result;
use mcast_control::probes::{CheckIn, CheckInReply, IntervalReport, MAX_REPORTS_PER_CHECKIN};
use mcast_control::{now_ms, Event, MonitorSnapshot, ProbeConfig, Watch};
use mcast_core::Interface;
use mcast_monitor::{MonitorConfig, Target};
use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use mcast_control::probes::{DEFAULT_BEACON_GROUP, DEFAULT_UNICAST_PORT};

/// Which server to report to.
#[derive(Debug, Clone)]
pub enum Server {
    /// This one, e.g. "http://10.0.0.5:8650".
    Url(String),
    /// Whichever announces itself on `group` (and knows our token).
    Discover {
        group: SocketAddr,
        /// Only servers whose name contains this.
        name: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub server: Server,
    pub token: Option<String>,
    /// This probe's name on the server.
    pub id: String,
    /// Interface to join groups on.
    pub interface: Interface,
    /// SAP groups to listen on when the server asks for SAP discovery.
    pub sap_groups: Vec<SocketAddr>,
    /// UDP port for unicast copies.
    pub unicast_port: u16,
    /// Address to give the server for unicast copies; by default the one
    /// this host uses to reach the server.
    pub advertise: Option<IpAddr>,
    /// Reports kept while the server is unreachable.
    pub max_queue: usize,
    /// Print a line per stream per report.
    pub verbose: bool,
}

/// What the probe is currently running.
struct Running {
    config: ProbeConfig,
    /// `None` if the monitor couldn't start (the error is logged).
    watch: Option<(Watch, Receiver<MonitorSnapshot>)>,
    unicast_addr: Option<SocketAddr>,
    /// The server the unicast address was chosen for.
    server: String,
    since_ms: u64,
}

/// Run until `stop` is set.
pub fn run(cfg: AgentConfig, stop: &AtomicBool) -> Result<()> {
    let discovery = match &cfg.server {
        Server::Url(_) => None,
        Server::Discover { group, name } => {
            let d = discovery::Discovery::start(discovery::DiscoveryConfig {
                group: *group,
                interface: cfg.interface,
                name: name.clone(),
                token: cfg.token.clone(),
            })
            .map_err(|e| anyhow::anyhow!("listening for servers on {group}: {e}"))?;
            eprintln!(
                "probe \"{}\" looking for a server on {group}{} (Ctrl-C to stop)",
                cfg.id,
                name.as_ref()
                    .map_or(String::new(), |n| format!(" named like \"{n}\""))
            );
            Some(d)
        }
    };
    let mut client = match &cfg.server {
        Server::Url(u) => {
            eprintln!("probe \"{}\" checking in with {u} (Ctrl-C to stop)", cfg.id);
            Some(client::Client::new(u, cfg.token.clone()))
        }
        Server::Discover { .. } => None,
    };
    let boot = mcast_core::random_u32();
    let started = Instant::now();
    let interval = |r: &Option<Running>| {
        Duration::from_secs_f64(
            r.as_ref()
                .map_or(ProbeConfig::default().report_interval_s, |r| {
                    r.config.report_interval_s
                }),
        )
    };
    let mut running: Option<Running> = None;
    let mut queue: VecDeque<IntervalReport> = VecDeque::new();
    let mut seq = 0u64;
    let mut dropped = 0u64;
    let mut next_check_in = Instant::now();
    let mut failures = 0u32;
    let mut last_error: Option<String> = None;

    while !stop.load(Ordering::Relaxed) {
        // Wait for the monitor's next report (or a moment, with none running).
        let snapshot = match running.as_ref().and_then(|r| r.watch.as_ref()) {
            Some((_, rx)) => match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(s) => Some(s),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => None,
            },
            None => {
                std::thread::sleep(Duration::from_millis(200));
                None
            }
        };
        if let (Some(snapshot), Some(r)) = (snapshot, running.as_mut()) {
            seq += 1;
            let end = now_ms();
            let report = IntervalReport {
                seq,
                start_ms: r.since_ms,
                end_ms: end,
                streams: snapshot.streams,
                groups: snapshot.groups,
            };
            r.since_ms = end;
            if cfg.verbose {
                print_report(&report);
            }
            queue.push_back(report);
            while queue.len() > cfg.max_queue.max(1) {
                queue.pop_front();
                dropped += 1;
            }
            // Report as soon as there's something to say, unless backing off.
            if failures == 0 {
                next_check_in = Instant::now();
            }
        }
        if let Some(d) = &discovery {
            for why in d.refused() {
                eprintln!("{why}");
            }
            if let Some(found) = d.current() {
                if client.as_ref().map_or(true, |c| c.base() != found.url) {
                    eprintln!("found server \"{}\" at {}", found.name, found.url);
                    client = Some(client::Client::new(&found.url, cfg.token.clone()));
                    next_check_in = Instant::now();
                    failures = 0;
                }
            }
        }
        let Some(client) = client.as_ref() else {
            continue; // nothing found yet
        };
        if Instant::now() < next_check_in {
            continue;
        }

        let batch: Vec<IntervalReport> = queue
            .iter()
            .take(MAX_REPORTS_PER_CHECKIN)
            .cloned()
            .collect();
        let c = CheckIn {
            version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: Some(mcast_control::request::hostname()),
            boot,
            uptime_s: started.elapsed().as_secs_f64(),
            config: running
                .as_ref()
                .filter(|r| r.watch.is_some())
                .map(|r| r.config.clone()),
            unicast_addr: running.as_ref().and_then(|r| r.unicast_addr),
            queued: queue.len() - batch.len(),
            reports: batch,
            dropped,
        };
        match client.check_in(&cfg.id, &c) {
            Ok(CheckInReply {
                config, acked_seq, ..
            }) => {
                if failures > 0 || last_error.is_some() {
                    eprintln!("reconnected to the server");
                }
                failures = 0;
                last_error = None;
                if let Some(acked) = acked_seq {
                    while queue.front().is_some_and(|r| r.seq <= acked) {
                        queue.pop_front();
                    }
                }
                // A new configuration, a retry of one that failed to start,
                // or a new server that may need unicast copies elsewhere.
                let changed = running.as_ref().map_or(true, |r| {
                    r.config != config
                        || r.watch.is_none()
                        || (config.unicast && r.server != client.base())
                });
                if changed {
                    drop(running.take()); // stop the old monitor (and free its ports) first
                    running = Some(start(&cfg, config, client.base()));
                }
                // More queued, or a new config to confirm? Check in again at
                // once; otherwise this is the heartbeat until the next report.
                next_check_in = if queue.is_empty() && !changed {
                    Instant::now() + interval(&running)
                } else {
                    Instant::now()
                };
            }
            Err(e) => {
                failures += 1;
                let msg = e.to_string();
                if last_error.as_deref() != Some(msg.as_str()) {
                    eprintln!("{msg} (reports are kept until it's back)");
                    last_error = Some(msg);
                }
                // Back off up to a minute, but never longer than the
                // report interval: the heartbeat is how we're seen.
                let backoff = Duration::from_secs(1 << failures.min(6)).min(interval(&running));
                next_check_in = Instant::now() + backoff;
            }
        }
    }
    Ok(())
}

/// Start monitoring as `config` asks.
fn start(cfg: &AgentConfig, config: ProbeConfig, server: &str) -> Running {
    let mut targets: Vec<Target> = config.groups.iter().map(|&g| Target::new(g)).collect();
    let mut unicast_addr = None;
    if config.unicast {
        match advertised_ip(cfg, server) {
            Ok(ip) => {
                let bind: SocketAddr = match ip {
                    IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, cfg.unicast_port).into(),
                    IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, cfg.unicast_port).into(),
                };
                targets.push(Target::new(bind));
                unicast_addr = Some(SocketAddr::new(ip, cfg.unicast_port));
            }
            Err(e) => eprintln!(
                "can't tell which address the server should send unicast to: {e}; use --advertise"
            ),
        }
    }
    let monitor = MonitorConfig {
        targets,
        sap: config.sap.then(|| cfg.sap_groups.clone()),
        interface: cfg.interface,
        default_spec: None,
    };
    let (tx, rx) = mpsc::channel();
    let sink: mcast_control::EventSink = Arc::new(move |e| {
        if let Event::Monitor { snapshot } = e {
            let _ = tx.send(snapshot);
        }
    });
    let interval = Duration::from_secs_f64(config.report_interval_s);
    let watch = match Watch::start(monitor, interval, sink) {
        Ok(w) => Some((w, rx)),
        Err(e) => {
            eprintln!("can't start monitoring: {e}");
            None
        }
    };
    if watch.is_some() {
        eprintln!("now running: {}", describe(&config, unicast_addr));
    }
    Running {
        config,
        unicast_addr: unicast_addr.filter(|_| watch.is_some()),
        watch,
        server: server.to_string(),
        since_ms: now_ms(),
    }
}

fn describe(c: &ProbeConfig, unicast: Option<SocketAddr>) -> String {
    let mut parts = Vec::new();
    if c.sap {
        parts.push("announced streams".to_string());
    }
    if !c.groups.is_empty() {
        parts.push(
            c.groups
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if parts.is_empty() {
        parts.push("nothing".into());
    }
    let mut s = format!(
        "watching {}, reporting every {} s",
        parts.join(" + "),
        c.report_interval_s
    );
    if let Some(u) = unicast {
        s += &format!(", unicast copies to {u}");
    }
    if let Some(l) = &c.label {
        s = format!("[{l}] {s}");
    }
    s
}

/// The address the server should send unicast copies to.
fn advertised_ip(cfg: &AgentConfig, server: &str) -> std::io::Result<IpAddr> {
    if let Some(ip) = cfg.advertise {
        return Ok(ip);
    }
    // The source address this host would use to reach the server. No packet
    // is sent: connecting a UDP socket only picks a route.
    let server = server_addr(server)?;
    let bind: SocketAddr = if server.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let s = UdpSocket::bind(bind)?;
    s.connect(server)?;
    Ok(s.local_addr()?.ip())
}

/// Host and port from a URL like "http://host:8650/...".
fn server_addr(url: &str) -> std::io::Result<SocketAddr> {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let authority = rest.split('/').next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let has_port = match authority.rfind(']') {
        Some(i) => authority[i..].contains(':'),
        None => authority.contains(':'),
    };
    let hostport = if has_port {
        authority.to_string()
    } else {
        let port = if scheme.eq_ignore_ascii_case("https") {
            443
        } else {
            80
        };
        format!("{authority}:{port}")
    };
    hostport.to_socket_addrs()?.next().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("can't resolve {hostport}"),
        )
    })
}

fn print_report(r: &IntervalReport) {
    if r.streams.is_empty() {
        println!("#{} no streams", r.seq);
    }
    for s in &r.streams {
        println!(
            "#{} {:<22} {:<20} {:>6} pkts {:>4} lost {:>6} jitter {:>7} max gap",
            r.seq,
            s.group,
            s.name.as_deref().unwrap_or("-"),
            s.packets,
            s.lost,
            s.jitter_ms.map_or("-".into(), |j| format!("{j:.2}ms")),
            format!("{:.0}ms", s.max_gap_ms),
        );
    }
}

#[cfg(test)]
mod tests;
