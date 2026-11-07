//! The Probes tab: a row per probe, and the selected one in detail
//! (streams, multicast vs. unicast verdicts, a loss timeline).

use super::{empty, highlight};
use crate::app::App;
use crate::model::{Health, Probe, Report};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn ago(ms: u64) -> String {
    let s = now_ms().saturating_sub(ms) / 1000;
    match s {
        0..=89 => format!("{s} s ago"),
        90..=5399 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

fn duration(s: f64) -> String {
    match s {
        s if s < 90.0 => format!("{s:.0} s"),
        s if s < 5400.0 => format!("{:.0} min", s / 60.0),
        s if s < 172_800.0 => format!("{:.1} h", s / 3600.0),
        s => format!("{:.0} d", s / 86400.0),
    }
}

pub fn health_color(h: Health) -> Color {
    match h {
        Health::Problem | Health::Offline => Color::Red,
        Health::Loss => Color::Yellow,
        Health::Ok => Color::Green,
        Health::NeverSeen => Color::Gray,
    }
}

/// verdict -> (label, color, what it points at)
fn verdict(v: &str) -> (&'static str, Color, &'static str) {
    match v {
        "ok" => ("ok", Color::Green, ""),
        "multicast_loss" => (
            "MULTICAST LOSS",
            Color::Red,
            "loss on multicast only: check IGMP snooping/querier, PIM, a Wi-Fi hop's multicast rate",
        ),
        "both_loss" => (
            "loss on both",
            Color::Yellow,
            "the network as a whole or the sender, not multicast as such",
        ),
        "unicast_loss" => ("unicast loss", Color::Yellow, "loss on the unicast copy only"),
        "no_multicast" => (
            "NO MULTICAST",
            Color::Red,
            "the unicast copy arrives but the group doesn't: not joined? IGMP, routing, TTL",
        ),
        "idle" => ("idle", Color::Gray, ""),
        _ => ("?", Color::Gray, ""),
    }
}

/// The worst verdict, for the list.
fn worst(p: &Probe) -> Option<&str> {
    let rank = |v: &str| match v {
        "no_multicast" => 0,
        "multicast_loss" => 1,
        "both_loss" => 2,
        "unicast_loss" => 3,
        "ok" => 4,
        _ => 5,
    };
    p.comparisons
        .iter()
        .map(|c| c.verdict.as_str())
        .min_by_key(|v| rank(v))
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if app.probes.is_empty() {
        return empty(
            f,
            area,
            "No probes yet. On each probe, run: mcast probe run --token TOKEN \
             (it finds this server by its multicast beacon; add --server URL where \
             multicast doesn't reach).",
        );
    }
    // Active alerts first, then the list, then the selected probe.
    let shown = app.alerts.len().min(5);
    let alert_height = match app.alerts.len() {
        0 => 0,
        n => (shown + usize::from(n > shown) + 1) as u16,
    };
    let list_height = (app.probes.len() as u16 + 1).clamp(2, (area.height * 2 / 5).max(3));
    let [alerts_area, list, _, detail] = Layout::vertical([
        Constraint::Length(alert_height),
        Constraint::Length(list_height),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(area);
    if !app.alerts.is_empty() {
        let mut lines: Vec<Line> = app
            .alerts
            .iter()
            .rev()
            .take(shown)
            .map(|a| {
                Line::from(vec![
                    Span::raw("▲ ").fg(Color::Red).bold(),
                    Span::raw(a.message.clone()),
                    Span::raw(format!("  since {}", ago(a.raised_ms))).dim(),
                ])
            })
            .collect();
        if app.alerts.len() > shown {
            lines.push(
                Line::from(format!(
                    "  and {} more (mcast probe alerts)",
                    app.alerts.len() - shown
                ))
                .dim(),
            );
        }
        f.render_widget(Paragraph::new(lines), alerts_area);
    }

    let rows = app.probes.iter().map(|p| {
        let h = p.health();
        let seen = p.last_seen_ms.map_or("–".into(), ago);
        let mc = p.multicast();
        let lost: u64 = mc.iter().map(|r| r.lost).sum();
        let streams = if p.latest.is_none() {
            "–".to_string()
        } else {
            format!(
                "{} stream{}, {lost} lost",
                mc.len(),
                if mc.len() == 1 { "" } else { "s" }
            )
        };
        let unicast = match (p.flag("unicast"), worst(p)) {
            (false, _) => Span::raw("off").dim(),
            (true, None) => Span::raw("waiting").dim(),
            (true, Some(v)) => {
                let (label, color, _) = verdict(v);
                Span::raw(label).fg(color)
            }
        };
        let row = Row::new(vec![
            Line::from(p.id.clone()).bold(),
            Line::from(p.label().unwrap_or("").to_string()),
            Line::from(h.label()).fg(health_color(h)),
            Line::from(seen),
            Line::from(streams).style(if lost > 0 {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new()
            }),
            Line::from(unicast),
        ]);
        if h == Health::Offline {
            row.dim()
        } else {
            row
        }
    });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Length(20),
            Constraint::Length(14),
        ],
    )
    .header(
        Row::new([
            "Probe",
            "Label",
            "State",
            "Checked in",
            "Multicast",
            "vs. unicast",
        ])
        .bold()
        .fg(Color::Gray),
    )
    .row_highlight_style(highlight());
    let mut state = TableState::default().with_selected(Some(app.selected()));
    f.render_stateful_widget(table, list, &mut state);

    if let Some(p) = app.selected_probe() {
        let lines = details(
            p,
            app.probe_reports(),
            detail.width.saturating_sub(1) as usize,
        );
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), detail);
    }
}

fn details(p: &Probe, reports: Option<&[Report]>, width: usize) -> Vec<Line<'static>> {
    let h = p.health();
    let mut head = vec![Span::raw(p.id.clone()).bold()];
    if let Some(l) = p.label() {
        head.push(Span::raw(format!(" · {l}")).dim());
    }
    head.push(Span::raw("  "));
    head.push(Span::raw(h.label()).fg(health_color(h)));
    let mut lines = vec![Line::from(head)];

    let mut facts = Vec::new();
    if let Some(host) = &p.hostname {
        facts.push(format!("host {host}"));
    }
    if let Some(v) = &p.version {
        facts.push(format!("v{v}"));
    }
    if let (true, Some(u)) = (p.online, p.uptime_s) {
        facts.push(format!("up {}", duration(u)));
    }
    if let Some(t) = p.last_seen_ms {
        facts.push(format!("checked in {}", ago(t)));
    }
    if !facts.is_empty() {
        lines.push(Line::from(facts.join(" · ")).dim());
    }
    let mut watch = Vec::new();
    if p.flag("sap") {
        watch.push("announced streams".to_string());
    }
    watch.extend(p.groups());
    let unicast = match (&p.unicast_addr, p.flag("unicast")) {
        (_, false) => "no unicast copies".to_string(),
        (Some(a), true) => format!("unicast copies to {a}"),
        (None, true) => "unicast copies (waiting for its address)".to_string(),
    };
    lines.push(
        Line::from(format!(
            "watches {} · every {} s · {unicast} · {}",
            if watch.is_empty() {
                "nothing".into()
            } else {
                watch.join(", ")
            },
            p.interval_s(),
            if p.custom_config {
                "own config"
            } else {
                "default config"
            },
        ))
        .dim(),
    );
    if p.last_seen_ms.is_some() && !p.config_applied {
        lines.push(Line::from("It hasn't applied its configuration yet.").fg(Color::Yellow));
    }
    if p.queued > 0 {
        lines.push(
            Line::from(format!(
                "Catching up: {} reports still queued on the probe.",
                p.queued
            ))
            .fg(Color::Yellow),
        );
    }
    if p.dropped > 0 {
        lines.push(
            Line::from(format!(
                "{} reports were lost while it couldn't reach the server.",
                p.dropped
            ))
            .fg(Color::Yellow),
        );
    }

    lines.push(Line::from(""));
    match reports {
        Some(r) if !r.is_empty() => {
            lines.push(timeline(r, width));
            let lossy = r.iter().filter(|r| r.totals().1 > 0).count();
            let span = r.last().map_or(0, |l| l.end_ms) - r[0].start_ms;
            lines.push(
                Line::from(format!(
                    "{lossy} of {} intervals with multicast loss over the last {}",
                    r.len(),
                    duration(span as f64 / 1000.0)
                ))
                .dim(),
            );
        }
        Some(_) => lines.push(Line::from("No reports yet.").dim()),
        None if p.last_seen_ms.is_none() => {
            lines.push(Line::from("Waiting for it to check in.").dim())
        }
        None => lines.push(Line::from("Loading its reports…").dim()),
    }

    let rows = p.multicast();
    if !rows.is_empty() {
        lines.push(Line::from(""));
        for r in rows {
            let lost = if r.lost > 0 {
                Span::raw(format!("{} lost ({:.2} %)", r.lost, r.loss_percent)).fg(Color::Yellow)
            } else {
                Span::raw("0 lost")
            };
            lines.push(Line::from(vec![
                Span::raw(format!("{:<20} ", r.name.as_deref().unwrap_or("–"))).bold(),
                Span::raw(format!("{:<22} ", r.group)).dim(),
                Span::raw(format!("{:>6.1} pkt/s  ", r.packets_per_sec)),
                lost,
                Span::raw(format!(
                    "  jitter {}  max gap {:.0} ms",
                    r.jitter_ms.map_or("–".into(), |j| format!("{j:.2} ms")),
                    r.max_gap_ms
                )),
            ]));
        }
    } else if p.latest.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from("No multicast traffic on the groups it watches.").dim());
    }

    if !p.comparisons.is_empty() {
        lines.push(Line::from(""));
        for c in &p.comparisons {
            let (label, color, why) = verdict(&c.verdict);
            let name = c
                .name
                .clone()
                .or_else(|| c.group.clone())
                .unwrap_or_else(|| format!("SSRC {:08x}", c.ssrc));
            let m = c.multicast.as_ref().map_or("nothing".to_string(), |m| {
                format!("{} pkts, {} lost", m.packets, m.lost)
            });
            lines.push(Line::from(vec![
                Span::raw(format!("[{label}] ")).fg(color).bold(),
                Span::raw(name).bold(),
                Span::raw(format!(
                    "  multicast {m} · unicast copy {} pkts, {} lost",
                    c.unicast.packets, c.unicast.lost
                ))
                .dim(),
            ]));
            if !why.is_empty() {
                lines.push(Line::from(format!("  {why}")).fg(color));
            }
        }
    }
    lines
}

/// One cell per report, newest on the right: height and colour show loss.
fn timeline(reports: &[Report], width: usize) -> Line<'static> {
    let shown = &reports[reports.len().saturating_sub(width.max(1))..];
    let spans: Vec<Span> = shown
        .iter()
        .map(|r| {
            let (packets, lost) = r.totals();
            let loss = if packets + lost == 0 {
                0.0
            } else {
                lost as f64 * 100.0 / (packets + lost) as f64
            };
            match (packets, lost) {
                (0, 0) => Span::raw("·").fg(Color::DarkGray),
                (_, 0) => Span::raw("▁").fg(Color::Green),
                _ if loss < 1.0 => Span::raw("▄").fg(Color::Yellow),
                _ => Span::raw("█").fg(Color::Red),
            }
        })
        .collect();
    Line::from(spans)
}
