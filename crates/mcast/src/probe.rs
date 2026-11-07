//! `mcast probe`: run a probe that reports to an `mcast serve` server, and
//! look after the server's probes.

use crate::SapListenArgs;
use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use mcast_control::probes::{Comparison, Verdict};
use mcast_control::{now_ms, Alert, AlertRules, ProbeConfig, ProbeInfo};
use mcast_core::Interface;
use mcast_probe::client::Client;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicBool;

#[derive(Subcommand)]
#[command(after_help = "Examples:
  # on the server
  mcast serve -l 0.0.0.0:8650 --token ADMIN --probe-token PROBES
  # on each Pi: find the server by its multicast beacon...
  mcast probe run --token PROBES --id idf-3
  # ...or name it
  mcast probe run --server http://10.0.0.5:8650 --token PROBES --id idf-3
  mcast probe discover                  # which servers announce themselves here?
  # from anywhere with the admin token
  mcast probe list --url http://10.0.0.5:8650 --token ADMIN
  mcast probe config idf-3 --label \"3rd floor IDF\" --unicast --interval 5
  mcast probe config --defaults --group 239.192.2.1:10000
  mcast probe alerts                    # what's wrong right now (and --all: lately)
  mcast probe alert-rules --loss-percent 0.5 --raise-after 3")]
pub enum ProbeCommand {
    /// Be a probe: watch streams as the server asks and report to it
    Run(RunArgs),
    /// List the server's probes, with multicast vs. unicast comparisons
    #[command(visible_alias = "ls")]
    List(ServerArgs),
    /// Show one probe in detail
    Show {
        id: String,
        #[command(flatten)]
        server: ServerArgs,
    },
    /// Change a probe's configuration (or the defaults, with --defaults)
    Config(ConfigArgs),
    /// List the servers announcing themselves to probes on this network
    Discover {
        /// Seconds to listen (servers announce every 5 s)
        #[arg(short, long, default_value_t = 6.0)]
        wait: f64,
        /// Beacon group
        #[arg(long, default_value = mcast_probe::DEFAULT_BEACON_GROUP, value_name = "ADDR:PORT")]
        beacon_group: SocketAddr,
        /// Interface to join on: IPv4 address or interface index
        #[arg(short, long, default_value = "any")]
        iface: Interface,
    },
    /// Alerts: probes offline, multicast loss
    Alerts {
        /// Also list resolved ones
        #[arg(short, long)]
        all: bool,
        #[command(flatten)]
        server: ServerArgs,
    },
    /// Show or change when alerts are raised
    AlertRules(AlertRulesArgs),
    /// Forget a probe
    #[command(visible_alias = "rm")]
    Remove {
        id: String,
        #[command(flatten)]
        server: ServerArgs,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// The `mcast serve` server to report to [default: find one by its
    /// multicast beacon]. Empty also means find one
    #[arg(short, long, env = "MCAST_URL")]
    server: Option<String>,
    /// Its probe token (or its admin token). When finding a server by beacon,
    /// only one that knows this token is used
    #[arg(long, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Group servers announce themselves on (ignored with --server)
    #[arg(long, default_value = mcast_probe::DEFAULT_BEACON_GROUP, value_name = "ADDR:PORT")]
    beacon_group: SocketAddr,
    /// Only use a discovered server whose name (its hostname) contains this
    /// (ignored with --server)
    #[arg(long, value_name = "NAME", env = "MCAST_SERVER_NAME")]
    server_name: Option<String>,
    /// This probe's name on the server [default: the hostname]
    #[arg(long, env = "MCAST_PROBE_ID")]
    id: Option<String>,
    /// Interface to join groups on: IPv4 address or interface index
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    #[command(flatten)]
    sap: SapListenArgs,
    /// UDP port for the server's unicast copies
    #[arg(long, default_value_t = mcast_probe::DEFAULT_UNICAST_PORT)]
    unicast_port: u16,
    /// Address the server should send unicast copies to [default: the one this host reaches it from]
    #[arg(long, value_name = "IP")]
    advertise: Option<IpAddr>,
    /// Reports to keep while the server is unreachable
    #[arg(long, default_value_t = 2000)]
    max_queue: usize,
    /// Print each stream's stats every report
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Args)]
pub struct ServerArgs {
    /// The server
    #[arg(
        short,
        long,
        default_value = "http://127.0.0.1:8650",
        env = "MCAST_URL"
    )]
    url: String,
    /// Its admin token
    #[arg(long, env = "MCAST_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

impl ServerArgs {
    fn client(&self) -> Client {
        Client::new(&self.url, self.token.clone())
    }
}

#[derive(Args)]
pub struct AlertRulesArgs {
    /// Loss on a multicast stream, in percent of one report interval, that counts as bad
    #[arg(long, value_name = "PERCENT")]
    loss_percent: Option<f64>,
    /// Bad intervals in a row before an alert is raised
    #[arg(long, value_name = "N")]
    raise_after: Option<u32>,
    /// Clean intervals in a row before it's resolved
    #[arg(long, value_name = "N")]
    clear_after: Option<u32>,
    /// Alert when a probe stops checking in: on or off
    #[arg(long, value_name = "on|off", value_parser = on_off)]
    offline: Option<bool>,
    /// Alert on multicast loss: on or off
    #[arg(long, value_name = "on|off", value_parser = on_off)]
    multicast_loss: Option<bool>,
    /// Alert when the unicast copy arrives but the multicast stream doesn't: on or off
    #[arg(long, value_name = "on|off", value_parser = on_off)]
    no_multicast: Option<bool>,
    #[command(flatten)]
    server: ServerArgs,
}

fn on_off(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" => Ok(true),
        "off" | "no" | "false" | "0" => Ok(false),
        _ => Err("use on or off".into()),
    }
}

#[derive(Args)]
pub struct ConfigArgs {
    /// The probe (it needn't have checked in yet)
    #[arg(required_unless_present = "defaults")]
    id: Option<String>,
    /// Change the defaults, used by every probe without its own configuration
    #[arg(long, conflicts_with_all = ["id", "reset"])]
    defaults: bool,
    /// Go back to the defaults
    #[arg(long, conflicts_with_all = ["label", "no_label", "sap", "no_sap", "group", "no_groups", "interval", "unicast", "no_unicast"])]
    reset: bool,
    /// Label shown in listings and alerts
    #[arg(long)]
    label: Option<String>,
    /// Remove the label
    #[arg(long, conflicts_with = "label")]
    no_label: bool,
    /// Watch streams announced with SAP
    #[arg(long)]
    sap: bool,
    /// Don't watch announced streams (only --group ones)
    #[arg(long, conflicts_with = "sap")]
    no_sap: bool,
    /// Watch this group (replaces the list; repeat for more)
    #[arg(short, long, value_name = "ADDR:PORT")]
    group: Vec<SocketAddr>,
    /// Watch no groups besides announced ones
    #[arg(long, conflicts_with = "group")]
    no_groups: bool,
    /// Seconds per report (and check-in), 1 to 3600
    #[arg(long, value_name = "SECONDS")]
    interval: Option<f64>,
    /// Have the server send unicast copies of its pages, to compare
    #[arg(long)]
    unicast: bool,
    /// Stop the unicast copies
    #[arg(long, conflicts_with = "unicast")]
    no_unicast: bool,
    #[command(flatten)]
    server: ServerArgs,
}

pub fn run(c: ProbeCommand, stop: &AtomicBool) -> Result<()> {
    match c {
        ProbeCommand::Run(a) => {
            // Empty values (from an env file left blank) mean "the default".
            let nonempty = |v: Option<String>| v.filter(|v| !v.trim().is_empty());
            let id = nonempty(a.id).unwrap_or_else(|| {
                // Hostnames can have characters IDs can't.
                mcast_control::request::hostname()
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                            c
                        } else {
                            '-'
                        }
                    })
                    .take(64)
                    .collect()
            });
            if !mcast_control::probes::valid_id(&id) {
                bail!("--id must be 1-64 letters, digits, '.', '_' or '-'");
            }
            let sap_groups = if a.sap.sap_groups.is_empty() {
                mcast_core::sap::default_listen_groups()
            } else {
                a.sap.sap_groups
            };
            let server = match nonempty(a.server) {
                Some(u) => mcast_probe::Server::Url(u),
                None => mcast_probe::Server::Discover {
                    group: a.beacon_group,
                    name: nonempty(a.server_name),
                },
            };
            mcast_probe::run(
                mcast_probe::AgentConfig {
                    server,
                    token: a.token,
                    id,
                    interface: a.iface,
                    sap_groups,
                    unicast_port: a.unicast_port,
                    advertise: a.advertise,
                    max_queue: a.max_queue,
                    verbose: a.verbose,
                },
                stop,
            )
        }
        ProbeCommand::List(s) => {
            let probes = s.client().probes()?;
            if probes.is_empty() {
                println!(
                    "no probes yet (start one with: mcast probe run --server {})",
                    s.url
                );
            }
            for p in &probes {
                print!("{}", summary(p));
            }
            Ok(())
        }
        ProbeCommand::Show { id, server } => {
            let p = server.client().probe(&id)?;
            print!("{}", summary(&p));
            println!("  config: {}", serde_json::to_string(&p.config)?);
            if !p.totals.is_empty() {
                println!("  since first seen:");
                for t in &p.totals {
                    println!(
                        "    {:<9} {:<22} {:<20} {:>9} pkts {:>6} lost ({:.3}%) {}/{} intervals lossy, max gap {:.0} ms, worst jitter {}",
                        if t.unicast { "unicast" } else { "multicast" },
                        t.group.to_string(),
                        t.name.as_deref().unwrap_or("-"),
                        t.packets,
                        t.lost,
                        t.loss_percent,
                        t.lossy_intervals,
                        t.intervals,
                        t.max_gap_ms,
                        t.worst_jitter_ms.map_or("-".into(), |j| format!("{j:.2} ms")),
                    );
                }
            }
            Ok(())
        }
        ProbeCommand::Config(a) => config(a),
        ProbeCommand::Discover {
            wait,
            beacon_group,
            iface,
        } => {
            let found = mcast_probe::discovery::listen_for(
                beacon_group,
                iface,
                std::time::Duration::from_secs_f64(wait.max(0.1)),
            )?;
            if found.is_empty() {
                println!(
                    "no servers heard on {beacon_group} in {wait} s \
                     (mcast serve announces itself unless it listens on loopback or has --no-beacon)"
                );
            }
            for (f, _) in found {
                println!("{:<24} {}", f.name, f.url);
            }
            Ok(())
        }
        ProbeCommand::Alerts { all, server } => {
            let list = server.client().alerts()?;
            if list.active.is_empty() {
                println!("no active alerts");
            }
            for a in &list.active {
                println!("{}", alert_line(a));
            }
            if all && !list.recent.is_empty() {
                println!("\nresolved:");
                for a in &list.recent {
                    println!("{}", alert_line(a));
                }
            }
            Ok(())
        }
        ProbeCommand::AlertRules(a) => {
            let client = a.server.client();
            let mut r: AlertRules = client.alert_rules()?;
            let before = r.clone();
            if let Some(v) = a.loss_percent {
                r.loss_percent = v;
            }
            if let Some(v) = a.raise_after {
                r.raise_after = v;
            }
            if let Some(v) = a.clear_after {
                r.clear_after = v;
            }
            if let Some(v) = a.offline {
                r.offline = v;
            }
            if let Some(v) = a.multicast_loss {
                r.multicast_loss = v;
            }
            if let Some(v) = a.no_multicast {
                r.no_multicast = v;
            }
            if r != before {
                r = client.set_alert_rules(&r)?;
            }
            let onoff = |b: bool| if b { "on" } else { "off" };
            println!(
                "probe offline: {}\nmulticast loss: {} (at {}% or more)\nno multicast: {}\n\
                 raised after {} bad intervals in a row, resolved after {} clean ones",
                onoff(r.offline),
                onoff(r.multicast_loss),
                r.loss_percent,
                onoff(r.no_multicast),
                r.raise_after,
                r.clear_after
            );
            Ok(())
        }
        ProbeCommand::Remove { id, server } => {
            server.client().remove(&id)?;
            println!("forgot {id}");
            Ok(())
        }
    }
}

fn config(a: ConfigArgs) -> Result<()> {
    let client = a.server.client();
    if a.reset {
        let id = a.id.as_deref().ok_or_else(|| anyhow!("which probe?"))?;
        let p = client.reset_config(id)?;
        print!("{}", summary(&p));
        return Ok(());
    }
    let mut c: ProbeConfig = match &a.id {
        _ if a.defaults => client.defaults()?,
        Some(id) => match client.probe(id) {
            Ok(p) => p.config,
            Err(e) if e.status == 404 => client.defaults()?,
            Err(e) => return Err(e.into()),
        },
        None => unreachable!("clap requires an id without --defaults"),
    };
    if let Some(l) = a.label {
        c.label = Some(l);
    }
    if a.no_label {
        c.label = None;
    }
    if a.sap {
        c.sap = true;
    }
    if a.no_sap {
        c.sap = false;
    }
    if !a.group.is_empty() {
        c.groups = a.group;
    }
    if a.no_groups {
        c.groups.clear();
    }
    if let Some(i) = a.interval {
        c.report_interval_s = i;
    }
    if a.unicast {
        c.unicast = true;
    }
    if a.no_unicast {
        c.unicast = false;
    }
    if a.defaults {
        let c = client.set_defaults(&c)?;
        println!("defaults: {}", serde_json::to_string(&c)?);
    } else {
        let p = client.set_config(a.id.as_deref().expect("checked above"), &c)?;
        print!("{}", summary(&p));
        if !p.online {
            println!("  (it gets this the next time it checks in)");
        }
    }
    Ok(())
}

fn ago(ms: u64) -> String {
    let s = now_ms().saturating_sub(ms) / 1000;
    match s {
        0..=89 => format!("{s} s ago"),
        90..=5399 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

fn clock(ms: u64) -> String {
    // UTC, so it reads the same everywhere the server's alerts are seen.
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}Z", s / 3600 % 24, s / 60 % 60, s % 60)
}

fn alert_line(a: &Alert) -> String {
    match a.resolved_ms {
        None => format!(
            "ACTIVE   since {} ({})  {}",
            clock(a.raised_ms),
            ago(a.raised_ms),
            a.message
        ),
        Some(r) => format!(
            "resolved {} - {}  {}",
            clock(a.raised_ms),
            clock(r),
            a.message
        ),
    }
}

/// A few lines about a probe: who, where, how its streams did last interval.
fn summary(p: &ProbeInfo) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let state = match (p.online, p.last_seen_ms) {
        (true, _) => "online".to_string(),
        (false, Some(t)) => format!("OFFLINE (last seen {})", ago(t)),
        (false, None) => "never seen".to_string(),
    };
    let _ = write!(s, "{}", p.id);
    if let Some(l) = &p.config.label {
        let _ = write!(s, " [{l}]");
    }
    let _ = write!(s, "  {state}");
    if let Some(h) = &p.hostname {
        let _ = write!(s, "  host {h}");
    }
    if let Some(v) = &p.version {
        let _ = write!(s, "  v{v}");
    }
    if !p.config_applied && p.last_seen_ms.is_some() {
        let _ = write!(s, "  (config not applied yet)");
    }
    if p.queued > 0 {
        let _ = write!(s, "  catching up: {} reports queued", p.queued);
    }
    if p.dropped > 0 {
        let _ = write!(s, "  {} reports lost while unreachable", p.dropped);
    }
    let _ = writeln!(s);
    if let Some(r) = &p.latest {
        for row in r.streams.iter().filter(|r| r.group.ip().is_multicast()) {
            let _ = writeln!(
                s,
                "  {:<22} {:<20} {:>7.1} pkt/s {:>5} lost ({:.2}%) jitter {} max gap {:.0} ms",
                row.group.to_string(),
                row.name.as_deref().unwrap_or("-"),
                row.packets_per_sec,
                row.lost,
                row.loss_percent,
                row.jitter_ms.map_or("-".into(), |j| format!("{j:.2} ms")),
                row.max_gap_ms,
            );
        }
        for c in &p.comparisons {
            let _ = writeln!(s, "  {}", comparison(c));
        }
    }
    s
}

fn comparison(c: &Comparison) -> String {
    let path =
        |packets: u64, lost: u64, pct: f64| format!("{packets} pkts, {lost} lost ({pct:.2}%)");
    let m = c.multicast.as_ref().map_or("nothing".into(), |m| {
        path(m.packets, m.lost, m.loss_percent)
    });
    let u = path(c.unicast.packets, c.unicast.lost, c.unicast.loss_percent);
    let verdict = match c.verdict {
        Verdict::Ok => "ok",
        Verdict::MulticastLoss => "LOSS ON MULTICAST ONLY: check the multicast path (IGMP snooping/querier, PIM, Wi-Fi multicast rate)",
        Verdict::UnicastLoss => "loss on the unicast copy only",
        Verdict::BothLoss => "LOSS ON BOTH: the network or the sender, not multicast as such",
        Verdict::NoMulticast => "NO MULTICAST: the unicast copy arrives but the group doesn't (not joined? IGMP, routing, TTL)",
        Verdict::Idle => "idle",
    };
    let what = match (&c.name, c.group) {
        (Some(n), _) => n.clone(),
        (None, Some(g)) => g.to_string(),
        (None, None) => format!("SSRC {:08x}", c.ssrc),
    };
    format!("  {what} vs. its unicast copy: multicast {m} | unicast {u} -> {verdict}")
}
