//! Probes: small receivers (e.g. Raspberry Pis) spread around a network
//! that watch the multicast streams and report back to a server.
//!
//! The probe always initiates: every `report_interval_s` it POSTs a
//! [`CheckIn`] to the server, carrying the stats it measured since the last
//! one (and any it couldn't deliver earlier). The reply carries the
//! configuration the server wants it to run. The check-in is the heartbeat:
//! a probe that misses three is shown offline. Probes need no open port for
//! control, so they work behind NAT and firewalls.
//!
//! With `unicast` on, every sender the server runs also sends a unicast copy
//! of each packet to the probe. The probe measures both, and the server
//! compares them stream by stream (by SSRC): loss on multicast alone points
//! at the multicast path (IGMP snooping, a missing querier, PIM, a wireless
//! hop's multicast rate), loss on both at the network or the sender.
//!
//! [`Probes`] is the server's side: the registry, what each probe has
//! reported, and the configurations, optionally saved to a JSON file.

use crate::watch::{StreamRow, WatchedGroup};
use crate::{now_ms, Event, EventSink};
use anyhow::{Context, Result};
use mcast_core::mirror::Mirror;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

/// Reports kept per probe (an hour at the default interval).
pub const HISTORY: usize = 360;
/// Most reports a probe sends in one check-in; the rest wait for the next.
pub const MAX_REPORTS_PER_CHECKIN: usize = 30;
/// Default port probes listen on for unicast copies.
pub const DEFAULT_UNICAST_PORT: u16 = 5104;

/// What the server asks a probe to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeConfig {
    /// Shown in listings (e.g. "3rd floor IDF").
    pub label: Option<String>,
    /// Watch every stream announced with SAP.
    pub sap: bool,
    /// Also watch these groups.
    pub groups: Vec<SocketAddr>,
    /// Seconds per report; also how often the probe checks in.
    pub report_interval_s: f64,
    /// Ask the server to send this probe unicast copies of its streams.
    pub unicast: bool,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            label: None,
            sap: true,
            groups: Vec::new(),
            report_interval_s: 10.0,
            unicast: false,
        }
    }
}

impl ProbeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(1.0..=3600.0).contains(&self.report_interval_s) {
            return Err("report_interval_s must be between 1 and 3600".into());
        }
        if let Some(g) = self.groups.iter().find(|g| !g.ip().is_multicast()) {
            return Err(format!("{g} is not a multicast group"));
        }
        if self.label.as_ref().is_some_and(|l| l.chars().count() > 100) {
            return Err("label is limited to 100 characters".into());
        }
        Ok(())
    }

    /// How long without a check-in before a probe counts as offline.
    pub fn offline_after_ms(&self) -> u64 {
        (self.report_interval_s * 3000.0) as u64 + 5000
    }
}

/// Probe IDs are short names: letters, digits, '.', '_' and '-'.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// One reporting interval measured by a probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntervalReport {
    /// Counts up from 1 for each run of the probe (see [`CheckIn::boot`]).
    pub seq: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    /// One row per sender per group; the probe's unicast port appears as a
    /// group with a unicast address.
    pub streams: Vec<StreamRow>,
    #[serde(default)]
    pub groups: Vec<WatchedGroup>,
}

/// Probe → server, every report interval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckIn {
    pub version: String,
    #[serde(default)]
    pub hostname: Option<String>,
    /// Random for each run of the probe; with `seq` it identifies reports
    /// across restarts.
    pub boot: u32,
    #[serde(default)]
    pub uptime_s: f64,
    /// The configuration the probe is running (`None` before its first reply).
    #[serde(default)]
    pub config: Option<ProbeConfig>,
    /// Where the probe receives unicast copies; `None` when not listening.
    #[serde(default)]
    pub unicast_addr: Option<SocketAddr>,
    /// Reports not yet acknowledged, oldest first.
    #[serde(default)]
    pub reports: Vec<IntervalReport>,
    /// Reports still queued on the probe beyond these.
    #[serde(default)]
    pub queued: usize,
    /// Reports the probe discarded because its queue was full.
    #[serde(default)]
    pub dropped: u64,
}

/// Server → probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckInReply {
    pub config: ProbeConfig,
    /// Highest `seq` of this boot the server has; the probe can forget it
    /// and everything before.
    pub acked_seq: Option<u64>,
    pub server_time_ms: u64,
}

/// Packets, loss and timing on one path over one interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathStats {
    pub packets: u64,
    pub lost: u64,
    pub loss_percent: f64,
    pub jitter_ms: Option<f64>,
    pub max_gap_ms: f64,
}

impl From<&StreamRow> for PathStats {
    fn from(r: &StreamRow) -> Self {
        Self {
            packets: r.packets,
            lost: r.lost,
            loss_percent: r.loss_percent,
            jitter_ms: r.jitter_ms,
            max_gap_ms: r.max_gap_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Both paths clean.
    Ok,
    /// Loss on multicast only: look at the multicast path.
    MulticastLoss,
    /// Loss on unicast only (unusual: the copy is the extra traffic).
    UnicastLoss,
    /// Loss on both: the network as a whole, or the sender.
    BothLoss,
    /// The unicast copy arrives but the multicast stream doesn't: the probe
    /// isn't getting the group (IGMP snooping, querier, routing, TTL).
    NoMulticast,
    /// Neither path saw packets this interval.
    Idle,
}

/// The multicast stream and its unicast copy, side by side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub ssrc: u32,
    pub name: Option<String>,
    /// The multicast group, if the stream was seen on one.
    pub group: Option<SocketAddr>,
    pub multicast: Option<PathStats>,
    pub unicast: PathStats,
    pub verdict: Verdict,
}

/// Compare each unicast copy in `report` with its multicast original.
pub fn compare(report: &IntervalReport) -> Vec<Comparison> {
    let mut multicast: HashMap<u32, &StreamRow> = HashMap::new();
    for r in &report.streams {
        if let (true, Some(ssrc)) = (r.group.ip().is_multicast(), r.ssrc) {
            multicast.insert(ssrc, r);
        }
    }
    let mut out: Vec<Comparison> = report
        .streams
        .iter()
        .filter(|r| !r.group.ip().is_multicast())
        .filter_map(|u| {
            let ssrc = u.ssrc?;
            let m = multicast.get(&ssrc).copied();
            let unicast = PathStats::from(u);
            let multicast = m.map(PathStats::from);
            let verdict = verdict(multicast.as_ref(), &unicast);
            Some(Comparison {
                ssrc,
                name: m.and_then(|m| m.name.clone()).or_else(|| u.name.clone()),
                group: m.map(|m| m.group),
                multicast,
                unicast,
                verdict,
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.name, a.ssrc).cmp(&(&b.name, b.ssrc)));
    out
}

fn verdict(m: Option<&PathStats>, u: &PathStats) -> Verdict {
    let m_packets = m.map_or(0, |m| m.packets);
    if m_packets == 0 && u.packets == 0 {
        return Verdict::Idle;
    }
    if m_packets == 0 {
        return Verdict::NoMulticast;
    }
    match (m.is_some_and(|m| m.lost > 0), u.lost > 0) {
        (false, false) => Verdict::Ok,
        (true, false) => Verdict::MulticastLoss,
        (false, true) => Verdict::UnicastLoss,
        (true, true) => Verdict::BothLoss,
    }
}

/// Everything a probe has reported about one stream since the server
/// started (or the probe was first seen).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamTotals {
    pub group: SocketAddr,
    pub unicast: bool,
    pub name: Option<String>,
    pub ssrc: Option<u32>,
    pub packets: u64,
    pub lost: u64,
    pub late: u64,
    pub loss_percent: f64,
    /// Intervals the stream was seen in, and how many had any loss.
    pub intervals: u64,
    pub lossy_intervals: u64,
    pub max_gap_ms: f64,
    pub worst_jitter_ms: Option<f64>,
    pub last_seen_ms: u64,
}

impl StreamTotals {
    fn add(&mut self, r: &StreamRow, at_ms: u64) {
        self.name = r.name.clone().or(self.name.take());
        self.packets += r.packets;
        self.lost += r.lost;
        self.late += r.late;
        let expected = self.packets + self.lost;
        self.loss_percent = if expected == 0 {
            0.0
        } else {
            self.lost as f64 * 100.0 / expected as f64
        };
        self.intervals += 1;
        if r.lost > 0 {
            self.lossy_intervals += 1;
        }
        self.max_gap_ms = self.max_gap_ms.max(r.max_gap_ms);
        self.worst_jitter_ms = match (self.worst_jitter_ms, r.jitter_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if r.packets > 0 {
            self.last_seen_ms = at_ms;
        }
    }
}

/// A probe as the server sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeInfo {
    pub id: String,
    pub online: bool,
    /// The configuration it should run: its own, or the defaults.
    pub config: ProbeConfig,
    /// It has a configuration of its own rather than the defaults.
    pub custom_config: bool,
    /// Its last check-in said it runs `config`.
    pub config_applied: bool,
    pub hostname: Option<String>,
    pub version: Option<String>,
    pub unicast_addr: Option<SocketAddr>,
    pub first_seen_ms: Option<u64>,
    pub last_seen_ms: Option<u64>,
    pub uptime_s: Option<f64>,
    /// Reports still queued on the probe at its last check-in (it was cut
    /// off from the server for a while and is catching up).
    pub queued: usize,
    /// Reports the probe discarded because the server was unreachable for
    /// too long.
    pub dropped: u64,
    pub latest: Option<IntervalReport>,
    /// Multicast vs. unicast, from the latest report.
    pub comparisons: Vec<Comparison>,
    pub totals: Vec<StreamTotals>,
}

#[derive(Default)]
struct Probe {
    /// `None` follows the defaults.
    config: Option<ProbeConfig>,
    running: Option<ProbeConfig>,
    hostname: Option<String>,
    version: Option<String>,
    unicast_addr: Option<SocketAddr>,
    first_seen_ms: Option<u64>,
    last_seen_ms: Option<u64>,
    uptime_s: Option<f64>,
    queued: usize,
    dropped: u64,
    boot: Option<u32>,
    last_seq: Option<u64>,
    history: VecDeque<IntervalReport>,
    totals: BTreeMap<(SocketAddr, Option<u32>, SocketAddr), StreamTotals>,
    /// Online as of the last event sent about it.
    online: bool,
}

impl Probe {
    fn effective<'a>(&'a self, defaults: &'a ProbeConfig) -> &'a ProbeConfig {
        self.config.as_ref().unwrap_or(defaults)
    }

    fn is_online(&self, defaults: &ProbeConfig, now: u64) -> bool {
        self.last_seen_ms
            .is_some_and(|t| now.saturating_sub(t) <= self.effective(defaults).offline_after_ms())
    }

    fn info(&self, id: &str, defaults: &ProbeConfig, now: u64) -> ProbeInfo {
        let config = self.effective(defaults).clone();
        let latest = self.history.back().cloned();
        ProbeInfo {
            id: id.to_string(),
            online: self.is_online(defaults, now),
            config_applied: self.running.as_ref() == Some(&config),
            custom_config: self.config.is_some(),
            config,
            hostname: self.hostname.clone(),
            version: self.version.clone(),
            unicast_addr: self.unicast_addr,
            first_seen_ms: self.first_seen_ms,
            last_seen_ms: self.last_seen_ms,
            uptime_s: self.uptime_s,
            queued: self.queued,
            dropped: self.dropped,
            comparisons: latest.as_ref().map(compare).unwrap_or_default(),
            latest,
            totals: self.totals.values().cloned().collect(),
        }
    }

    /// Store new reports; returns the highest seq held for this boot.
    fn absorb(&mut self, boot: u32, reports: Vec<IntervalReport>) -> Option<u64> {
        if self.boot != Some(boot) {
            self.boot = Some(boot);
            self.last_seq = None;
        }
        for r in reports {
            if self.last_seq.is_some_and(|s| r.seq <= s) {
                continue; // a retry of something we already have
            }
            self.last_seq = Some(r.seq);
            for row in &r.streams {
                let key = (row.group, row.ssrc, row.source);
                self.totals
                    .entry(key)
                    .or_insert_with(|| StreamTotals {
                        group: row.group,
                        unicast: !row.group.ip().is_multicast(),
                        name: None,
                        ssrc: row.ssrc,
                        packets: 0,
                        lost: 0,
                        late: 0,
                        loss_percent: 0.0,
                        intervals: 0,
                        lossy_intervals: 0,
                        max_gap_ms: 0.0,
                        worst_jitter_ms: None,
                        last_seen_ms: 0,
                    })
                    .add(row, r.end_ms);
            }
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(r);
        }
        self.last_seq
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    defaults: ProbeConfig,
    #[serde(default)]
    probes: BTreeMap<String, SavedProbe>,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedProbe {
    #[serde(default)]
    config: Option<ProbeConfig>,
}

struct State {
    defaults: ProbeConfig,
    probes: BTreeMap<String, Probe>,
}

/// The server's registry of probes.
pub struct Probes {
    state: Mutex<State>,
    sink: EventSink,
    mirror: Arc<Mirror>,
    path: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ProbeError {
    Invalid(String),
    NotFound,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Invalid(m) => f.write_str(m),
            ProbeError::NotFound => f.write_str("no such probe"),
        }
    }
}

impl std::error::Error for ProbeError {}

impl Probes {
    /// `mirror` is the senders' unicast copy list (see
    /// [`crate::Jobs::mirror`]); `path`, if given, keeps the known probes
    /// and their configurations across restarts.
    pub fn new(sink: EventSink, mirror: Arc<Mirror>, path: Option<PathBuf>) -> Result<Self> {
        let saved: Saved = match &path {
            Some(p) => match std::fs::read(p) {
                Ok(bytes) => serde_json::from_slice(&bytes)
                    .with_context(|| format!("reading probes from {}", p.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Saved::default(),
                Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
            },
            None => Saved::default(),
        };
        let probes = saved
            .probes
            .into_iter()
            .filter(|(id, _)| valid_id(id))
            .map(|(id, p)| {
                (
                    id,
                    Probe {
                        config: p.config,
                        ..Probe::default()
                    },
                )
            })
            .collect();
        Ok(Self {
            state: Mutex::new(State {
                defaults: saved.defaults,
                probes,
            }),
            sink,
            mirror,
            path,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<ProbeInfo> {
        let st = self.lock();
        let now = now_ms();
        st.probes
            .iter()
            .map(|(id, p)| p.info(id, &st.defaults, now))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<ProbeInfo> {
        let st = self.lock();
        st.probes
            .get(id)
            .map(|p| p.info(id, &st.defaults, now_ms()))
    }

    /// The reports kept for a probe, oldest first; only those after `after`
    /// (an `end_ms`) if given.
    pub fn reports(&self, id: &str, after: Option<u64>) -> Option<Vec<IntervalReport>> {
        let st = self.lock();
        let p = st.probes.get(id)?;
        Some(
            p.history
                .iter()
                .filter(|r| after.map_or(true, |a| r.end_ms > a))
                .cloned()
                .collect(),
        )
    }

    pub fn defaults(&self) -> ProbeConfig {
        self.lock().defaults.clone()
    }

    pub fn set_defaults(&self, config: ProbeConfig) -> Result<ProbeConfig, ProbeError> {
        config.validate().map_err(ProbeError::Invalid)?;
        let infos = {
            let mut st = self.lock();
            st.defaults = config.clone();
            self.save(&st);
            let now = now_ms();
            let st = &*st;
            st.probes
                .iter()
                .filter(|(_, p)| p.config.is_none())
                .map(|(id, p)| p.info(id, &st.defaults, now))
                .collect::<Vec<_>>()
        };
        for probe in infos {
            (self.sink)(Event::ProbeUpdated { probe });
        }
        self.refresh_mirror();
        Ok(config)
    }

    /// Give a probe its own configuration (`None`: follow the defaults).
    /// Unknown probes are added, so probes can be set up before they're deployed.
    pub fn set_config(
        &self,
        id: &str,
        config: Option<ProbeConfig>,
    ) -> Result<ProbeInfo, ProbeError> {
        if !valid_id(id) {
            return Err(ProbeError::Invalid(
                "probe IDs are 1-64 letters, digits, '.', '_' or '-'".into(),
            ));
        }
        if let Some(c) = &config {
            c.validate().map_err(ProbeError::Invalid)?;
        }
        let info = {
            let mut st = self.lock();
            let st = &mut *st;
            let p = st.probes.entry(id.to_string()).or_default();
            p.config = config;
            let info = p.info(id, &st.defaults, now_ms());
            self.save(st);
            info
        };
        (self.sink)(Event::ProbeUpdated {
            probe: info.clone(),
        });
        self.refresh_mirror();
        Ok(info)
    }

    /// Forget a probe (it reappears with the defaults if it checks in again).
    pub fn remove(&self, id: &str) -> Result<(), ProbeError> {
        {
            let mut st = self.lock();
            st.probes.remove(id).ok_or(ProbeError::NotFound)?;
            self.save(&st);
        }
        (self.sink)(Event::ProbeRemoved { id: id.to_string() });
        self.refresh_mirror();
        Ok(())
    }

    /// A probe's heartbeat and stats; the reply tells it what to run.
    pub fn check_in(&self, id: &str, c: CheckIn) -> Result<CheckInReply, ProbeError> {
        if !valid_id(id) {
            return Err(ProbeError::Invalid(
                "probe IDs are 1-64 letters, digits, '.', '_' or '-'".into(),
            ));
        }
        if c.reports.len() > HISTORY {
            return Err(ProbeError::Invalid(format!(
                "at most {HISTORY} reports per check-in"
            )));
        }
        let now = now_ms();
        let (reply, info) = {
            let mut st = self.lock();
            let st = &mut *st;
            let new = !st.probes.contains_key(id);
            let p = st.probes.entry(id.to_string()).or_default();
            p.first_seen_ms.get_or_insert(now);
            p.last_seen_ms = Some(now);
            p.hostname = c.hostname;
            p.version = Some(c.version);
            p.uptime_s = Some(c.uptime_s);
            p.unicast_addr = c.unicast_addr;
            p.running = c.config;
            p.queued = c.queued;
            p.dropped = c.dropped;
            p.online = true;
            let acked_seq = p.absorb(c.boot, c.reports);
            let reply = CheckInReply {
                config: p.effective(&st.defaults).clone(),
                acked_seq,
                server_time_ms: now,
            };
            let info = p.info(id, &st.defaults, now);
            if new {
                self.save(st);
            }
            (reply, info)
        };
        (self.sink)(Event::ProbeUpdated { probe: info });
        self.refresh_mirror();
        Ok(reply)
    }

    /// Notice probes that stopped checking in. Call about once a second.
    pub fn tick(&self) {
        let now = now_ms();
        let went_offline: Vec<ProbeInfo> = {
            let mut st = self.lock();
            let st = &mut *st;
            st.probes
                .iter_mut()
                .filter_map(|(id, p)| {
                    if p.online && !p.is_online(&st.defaults, now) {
                        p.online = false;
                        Some(p.info(id, &st.defaults, now))
                    } else {
                        None
                    }
                })
                .collect()
        };
        if went_offline.is_empty() {
            return;
        }
        for probe in went_offline {
            (self.sink)(Event::ProbeUpdated { probe });
        }
        self.refresh_mirror();
    }

    /// Send unicast copies to the probes that are online and asked for them.
    fn refresh_mirror(&self) {
        let targets = {
            let st = self.lock();
            let now = now_ms();
            st.probes
                .values()
                .filter(|p| p.effective(&st.defaults).unicast && p.is_online(&st.defaults, now))
                .filter_map(|p| p.unicast_addr)
                .collect()
        };
        self.mirror.set(targets);
    }

    fn save(&self, st: &State) {
        let Some(path) = &self.path else {
            return;
        };
        let saved = Saved {
            defaults: st.defaults.clone(),
            probes: st
                .probes
                .iter()
                .map(|(id, p)| {
                    (
                        id.clone(),
                        SavedProbe {
                            config: p.config.clone(),
                        },
                    )
                })
                .collect(),
        };
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            eprintln!("saving probes to {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(group: &str, ssrc: u32, packets: u64, lost: u64) -> StreamRow {
        StreamRow {
            group: group.parse().unwrap(),
            name: Some("Lobby".into()),
            source: "10.0.0.1:40000".parse().unwrap(),
            ssrc: Some(ssrc),
            payload_type: Some(96),
            codec: None,
            packets,
            bytes: packets * 300,
            packets_per_sec: packets as f64 / 10.0,
            kbit_per_sec: 0.0,
            lost,
            late: 0,
            loss_percent: if packets + lost == 0 {
                0.0
            } else {
                lost as f64 * 100.0 / (packets + lost) as f64
            },
            jitter_ms: Some(0.1),
            max_gap_ms: 1.0,
            ptime_ms: Some(1.0),
            ttl: Some(16),
            tos: None,
            peak_dbfs: None,
            rms_dbfs: None,
            idle_ms: 0.0,
        }
    }

    fn report(seq: u64, streams: Vec<StreamRow>) -> IntervalReport {
        IntervalReport {
            seq,
            start_ms: seq * 10_000,
            end_ms: (seq + 1) * 10_000,
            streams,
            groups: Vec::new(),
        }
    }

    fn sink() -> (EventSink, Arc<Mutex<Vec<Event>>>) {
        let events: Arc<Mutex<Vec<Event>>> = Arc::default();
        let e = events.clone();
        (Arc::new(move |ev| e.lock().unwrap().push(ev)), events)
    }

    fn check_in(boot: u32, reports: Vec<IntervalReport>) -> CheckIn {
        CheckIn {
            version: "test".into(),
            hostname: Some("pi-1".into()),
            boot,
            uptime_s: 1.0,
            config: None,
            unicast_addr: Some("10.0.0.9:5104".parse().unwrap()),
            reports,
            queued: 0,
            dropped: 0,
        }
    }

    #[test]
    fn verdicts() {
        let r = report(
            1,
            vec![
                row("239.69.0.1:5004", 1, 1000, 0),
                row("0.0.0.0:5104", 1, 1000, 0),
                row("239.69.0.2:5004", 2, 990, 10),
                row("0.0.0.0:5104", 2, 1000, 0),
                row("239.69.0.3:5004", 3, 990, 10),
                row("0.0.0.0:5104", 3, 990, 10),
                row("0.0.0.0:5104", 4, 1000, 0),
                row("239.69.0.5:5004", 5, 1000, 0), // not mirrored: no comparison
            ],
        );
        let v: Vec<(u32, Verdict)> = compare(&r).iter().map(|c| (c.ssrc, c.verdict)).collect();
        assert_eq!(
            v,
            vec![
                (1, Verdict::Ok),
                (2, Verdict::MulticastLoss),
                (3, Verdict::BothLoss),
                (4, Verdict::NoMulticast),
            ]
        );
    }

    #[test]
    fn check_ins_ack_dedupe_and_restart() {
        let (sink, events) = sink();
        let mirror = Mirror::new();
        let probes = Probes::new(sink, mirror.clone(), None).unwrap();

        let reply = probes
            .check_in(
                "pi-1",
                check_in(7, vec![report(1, vec![row("239.69.0.1:5004", 1, 100, 1)])]),
            )
            .unwrap();
        assert_eq!(reply.acked_seq, Some(1));
        assert_eq!(reply.config, ProbeConfig::default());
        // A retry of seq 1 plus seq 2: only seq 2 is new.
        let reply = probes
            .check_in(
                "pi-1",
                check_in(
                    7,
                    vec![
                        report(1, vec![row("239.69.0.1:5004", 1, 100, 1)]),
                        report(2, vec![row("239.69.0.1:5004", 1, 100, 0)]),
                    ],
                ),
            )
            .unwrap();
        assert_eq!(reply.acked_seq, Some(2));
        let info = probes.get("pi-1").unwrap();
        assert!(info.online);
        assert_eq!(info.totals.len(), 1);
        assert_eq!(info.totals[0].packets, 200);
        assert_eq!(info.totals[0].lost, 1);
        assert_eq!(info.totals[0].lossy_intervals, 1);
        assert_eq!(probes.reports("pi-1", None).unwrap().len(), 2);

        // The probe restarted: seq starts again from 1.
        let reply = probes
            .check_in("pi-1", check_in(8, vec![report(1, vec![])]))
            .unwrap();
        assert_eq!(reply.acked_seq, Some(1));
        assert_eq!(probes.reports("pi-1", None).unwrap().len(), 3);
        assert!(!events.lock().unwrap().is_empty());

        // Unicast copies only when configured.
        assert!(mirror.targets().is_empty());
        let cfg = ProbeConfig {
            unicast: true,
            ..ProbeConfig::default()
        };
        let info = probes.set_config("pi-1", Some(cfg.clone())).unwrap();
        assert!(info.custom_config && !info.config_applied);
        assert_eq!(mirror.targets(), vec!["10.0.0.9:5104".parse().unwrap()]);
        let mut c = check_in(8, vec![]);
        c.config = Some(cfg);
        probes.check_in("pi-1", c).unwrap();
        assert!(probes.get("pi-1").unwrap().config_applied);

        probes.remove("pi-1").unwrap();
        assert!(mirror.targets().is_empty());
        assert!(probes.get("pi-1").is_none());
    }

    #[test]
    fn rejects_bad_ids_and_configs() {
        let (sink, _) = sink();
        let probes = Probes::new(sink, Mirror::new(), None).unwrap();
        assert!(probes.check_in("../x", check_in(1, vec![])).is_err());
        let bad = ProbeConfig {
            report_interval_s: 0.1,
            ..ProbeConfig::default()
        };
        assert!(probes.set_config("pi-2", Some(bad)).is_err());
        let bad = ProbeConfig {
            groups: vec!["10.0.0.1:5004".parse().unwrap()],
            ..ProbeConfig::default()
        };
        assert!(probes.set_defaults(bad).is_err());
    }

    #[test]
    fn configs_survive_a_restart() {
        let path = std::env::temp_dir().join(format!(
            "mcast-probes-test-{}/probes.json",
            mcast_core::random_u32()
        ));
        let (sink, _) = sink();
        {
            let probes = Probes::new(sink.clone(), Mirror::new(), Some(path.clone())).unwrap();
            let cfg = ProbeConfig {
                label: Some("IDF 3".into()),
                ..ProbeConfig::default()
            };
            probes.set_config("pi-3", Some(cfg)).unwrap();
            probes
                .set_defaults(ProbeConfig {
                    report_interval_s: 5.0,
                    ..ProbeConfig::default()
                })
                .unwrap();
        }
        let probes = Probes::new(sink, Mirror::new(), Some(path.clone())).unwrap();
        assert_eq!(probes.defaults().report_interval_s, 5.0);
        let info = probes.get("pi-3").unwrap();
        assert_eq!(info.config.label.as_deref(), Some("IDF 3"));
        assert!(!info.online);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
