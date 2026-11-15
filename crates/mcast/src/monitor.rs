//! `mcast monitor`: watch streams, as a table that redraws.

use crate::{book, SapListenArgs};
use anyhow::Result;
use clap::Args;
use mcast_audio::{Codec, StreamSpec};
use mcast_core::sap;
use mcast_core::Interface;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Args)]
pub(crate) struct MonitorArgs {
    /// Start from a saved preset (`mcast preset`); options and groups given here win. Repeatable
    #[arg(short = 'P', long, value_name = "NAME")]
    preset: Vec<String>,
    /// Groups to watch (ADDR:PORT or phonebook names). With none, watches every stream announced via SAP
    #[arg(value_name = "GROUP", value_parser = crate::book::parse_group)]
    groups: Vec<SocketAddr>,
    /// Also watch SAP-announced streams when groups are given
    #[arg(long)]
    sap: bool,
    #[command(flatten)]
    sap_listen: SapListenArgs,
    /// Interface: IPv4 address (IPv4 groups) or interface index (IPv6 groups)
    #[arg(short, long, default_value = "any")]
    iface: Interface,
    /// Codec for level metering when the payload type is dynamic and not announced
    #[arg(short, long)]
    encoding: Option<Codec>,
    /// Seconds between updates
    #[arg(long, default_value_t = 1.0)]
    interval: f64,
    /// Stop after this many seconds (default: run until Ctrl-C)
    #[arg(short, long)]
    duration: Option<f64>,
    /// Print a line per stream per interval instead of a live table
    #[arg(long)]
    log: bool,
}

/// Monitor targets for `groups`, named from the phonebook where they're in it
/// (an SAP announcement for the group names it instead).
fn named_targets(groups: &[SocketAddr]) -> Vec<mcast_monitor::Target> {
    let book = book::load_quietly();
    groups
        .iter()
        .map(|&g| {
            let mut t = mcast_monitor::Target::new(g);
            t.name = book
                .as_ref()
                .and_then(|b| b.name_of(g))
                .map(|e| e.name.clone());
            t
        })
        .collect()
}

pub(crate) fn run(a: MonitorArgs, stop: &AtomicBool) -> Result<()> {
    use mcast_monitor::{Monitor, MonitorConfig};
    use std::io::{IsTerminal, Write};

    let watch_sap = a.groups.is_empty() || a.sap;
    let sap_groups = if a.sap_listen.sap_groups.is_empty() {
        sap::default_listen_groups()
    } else {
        a.sap_listen.sap_groups.clone()
    };
    let mut m = Monitor::start(MonitorConfig {
        targets: named_targets(&a.groups),
        sap: watch_sap.then_some(sap_groups),
        interface: a.iface,
        default_spec: a.encoding.map(|c| StreamSpec::new(c, 48_000, 1)),
    })?;
    let live = !a.log && std::io::stdout().is_terminal();
    let interval = Duration::from_secs_f64(a.interval.max(0.1));
    let start = Instant::now();
    let limit = a.duration.map(Duration::from_secs_f64);
    if !live {
        eprintln!(
            "watching {}{}. Ctrl-C to stop.",
            if a.groups.is_empty() {
                String::new()
            } else {
                format!("{} group(s)", a.groups.len())
            },
            match (watch_sap, a.groups.is_empty()) {
                (true, true) => "SAP-announced streams",
                (true, false) => " and SAP-announced streams",
                _ => "",
            }
        );
    }
    let mut next = Instant::now() + interval;
    while !stop.load(Ordering::Relaxed) && limit.is_none_or(|l| start.elapsed() < l) {
        m.poll(next);
        next += interval;
        let rows = m.report();
        let groups = m.groups();
        let listeners = m.listeners();
        let mut out = std::io::stdout().lock();
        if live {
            write!(out, "\x1b[H\x1b[2J")?;
            writeln!(
                out,
                "mcast monitor | {} | {} stream(s) | Ctrl-C to stop\n",
                fmt_elapsed(start.elapsed()),
                rows.len()
            )?;
            write!(out, "{}", monitor_table(&groups, &rows, &listeners, true))?;
        } else {
            let t = fmt_elapsed(start.elapsed());
            for line in monitor_table(&groups, &rows, &listeners, false)
                .lines()
                .skip(1)
            {
                writeln!(out, "{t}  {line}")?;
            }
        }
        out.flush()?;
    }
    let totals = m.totals();
    println!(
        "\nsummary after {} (jitter is the maximum seen):",
        fmt_elapsed(start.elapsed())
    );
    print!(
        "{}",
        monitor_table(&m.groups(), &totals, &m.listeners(), false)
    );
    Ok(())
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

fn level_meter(dbfs: f64) -> String {
    // 10 cells from -60 to 0 dBFS.
    let filled = (((dbfs + 60.0) / 6.0).round() as i64).clamp(0, 10) as usize;
    format!("{}{}", "█".repeat(filled), "·".repeat(10 - filled))
}

fn monitor_table(
    groups: &[mcast_monitor::GroupStatus],
    rows: &[mcast_monitor::StreamReport],
    listeners: &[mcast_monitor::ListenerReport],
    meters: bool,
) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<30} {:<21} {:<13} {:>7} {:>8} {:>6} {:>5} {:>7} {:>6} {:>4} {:<5} LEVEL / STATUS",
        "STREAM",
        "SOURCE",
        "CODEC",
        "PKT/S",
        "KBIT/S",
        "LOSS",
        "LATE",
        "JITTER",
        "PTIME",
        "TTL",
        "DSCP",
    );
    for g in groups {
        let label = match &g.name {
            Some(n) => format!(
                "{} {}",
                truncate(n, 29 - g.group.to_string().len().min(28)),
                g.group
            ),
            None => g.group.to_string(),
        };
        let mine: Vec<_> = rows.iter().filter(|r| r.group == g.group).collect();
        if mine.is_empty() {
            let status = if let Some(e) = &g.error {
                format!("error: {e}")
            } else if g.ended {
                "ended".into()
            } else {
                "waiting for packets".into()
            };
            let _ = writeln!(s, "{label:<30} {:<21} {status}", "-");
            write_listeners(&mut s, listeners.iter().filter(|l| l.group == g.group));
            continue;
        }
        for (i, r) in mine.iter().enumerate() {
            let name = if i == 0 { label.clone() } else { String::new() };
            let opt =
                |v: Option<f64>, unit: &str| v.map_or("-".to_string(), |v| format!("{v:.1}{unit}"));
            let codec = match (&r.codec, r.payload_type, r.is_rtp()) {
                (_, _, false) if g.kind.is_some() && r.packets < 3 => g.kind.unwrap().to_string(),
                (_, _, false) => "not RTP".to_string(),
                (None, _, _) if g.kind.is_some() => g.kind.unwrap().to_string(),
                (Some(c), _, _) => c.clone(),
                (None, Some(pt), _) => format!("pt {pt}"),
                (None, None, _) => "-".into(),
            };
            let dscp = r.dscp().map_or("-".into(), mcast_monitor::dscp_name);
            let bye = r.rtcp.as_ref().and_then(|x| x.bye.as_ref());
            let status = if let Some(reason) = bye {
                match reason {
                    Some(why) => format!("ended (BYE: {why})"),
                    None => "ended (BYE)".into(),
                }
            } else if g.kind.is_some() && r.idle > Duration::from_secs(1) {
                format!("idle {}s", r.idle.as_secs())
            } else if g.kind.is_some() {
                "active".into()
            } else if r.idle > Duration::from_secs(2) {
                format!("no packets for {}s", r.idle.as_secs())
            } else if g.ended {
                "ended".into()
            } else {
                match r.peak_dbfs {
                    Some(p) if p < -60.0 => "silent".into(),
                    Some(p) if meters => format!("{} {p:>5.1} dBFS", level_meter(p)),
                    Some(p) => format!(
                        "peak {p:.1} dBFS, rms {:.1}",
                        r.rms_dbfs.unwrap_or(f64::NAN)
                    ),
                    None => String::new(),
                }
            };
            let _ = writeln!(
                s,
                "{:<30} {:<21} {:<13} {:>7.1} {:>8.1} {:>5.1}% {:>5} {:>7} {:>6} {:>4} {:<5} {}",
                truncate(&name, 30),
                r.source.to_string(),
                truncate(&codec, 13),
                r.packets_per_sec,
                r.kbit_per_sec,
                r.loss_percent,
                r.late,
                opt(r.jitter_ms, "ms"),
                opt(r.ptime_ms, "ms"),
                r.ttl.map_or("-".into(), |t| t.to_string()),
                dscp,
                status
            );
            if let Some(x) = &r.rtcp {
                let _ = writeln!(s, "{:<30} {}", "", format_sender_rtcp(x));
            }
            write_listeners(
                &mut s,
                listeners
                    .iter()
                    .filter(|l| l.group == g.group && (l.about.is_none() || l.about == r.ssrc)),
            );
        }
        // Listeners reporting on a source we no longer see.
        let orphans = listeners.iter().filter(|l| {
            l.group == g.group && l.about.is_some() && !mine.iter().any(|r| r.ssrc == l.about)
        });
        write_listeners(&mut s, orphans);
    }
    s
}

/// Milliseconds with one decimal, without "-0.0".
pub(crate) fn ms(v: f64) -> String {
    let v = if v.abs() < 0.05 { 0.0 } else { v };
    format!("{v:.1}ms")
}

fn format_sender_rtcp(x: &mcast_monitor::SenderRtcp) -> String {
    let mut parts = Vec::new();
    if let Some(who) = x.name.as_ref().or(x.cname.as_ref()) {
        parts.push(format!("\"{who}\""));
    }
    let every = x
        .interval
        .map_or(String::new(), |i| format!(" every {:.1}s", i.as_secs_f64()));
    parts.push(format!("SR{every}, last {}s ago", x.last_report.as_secs()));
    parts.push(format!("one-way {}", ms(x.one_way_secs * 1000.0)));
    if let Some(ppm) = x.media_clock_ppm {
        parts.push(format!("media clock {ppm:+.0} ppm"));
    }
    let mut line = format!("└ rtcp: {}", parts.join(", "));
    if x.last_report > Duration::from_secs(15) && x.bye.is_none() {
        line += "  (reports stopped)";
    }
    line
}

/// One line per listener, then the spread of their playout delays.
fn write_listeners<'a>(
    s: &mut String,
    listeners: impl Iterator<Item = &'a mcast_monitor::ListenerReport>,
) {
    use std::fmt::Write;
    let listeners: Vec<_> = listeners.collect();
    for l in &listeners {
        let mut parts = Vec::new();
        if let Some(p) = l.loss_percent {
            parts.push(format!("loss {p:.1}%"));
        }
        if let Some(j) = l.jitter_ms {
            parts.push(format!("jitter {j:.1}ms"));
        }
        if let (Some(lat), Some(t), Some(b)) = (l.latency_ms(), l.transit_ms, l.buffered_ms) {
            parts.push(format!(
                "plays {} behind (transit {} + buffer {})",
                ms(lat),
                ms(t),
                ms(b)
            ));
        }
        let heard = if l.left {
            "left".to_string()
        } else {
            format!("heard {}s ago", l.last_heard.as_secs())
        };
        parts.push(heard);
        let _ = writeln!(
            s,
            "{:<30} └ listener {} ({}): {}",
            "",
            truncate(&l.label(), 24),
            l.from.ip(),
            parts.join(", ")
        );
    }
    let latencies: Vec<f64> = listeners
        .iter()
        .filter(|l| !l.left)
        .filter_map(|l| l.latency_ms())
        .collect();
    if latencies.len() >= 2 {
        let min = latencies.iter().copied().fold(f64::INFINITY, f64::min);
        let max = latencies.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let _ = writeln!(
            s,
            "{:<30} └ sync: {} listeners within {:.1}ms of each other",
            "",
            latencies.len(),
            max - min
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
