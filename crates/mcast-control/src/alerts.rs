//! Alerts about probes: one that stops checking in, and multicast loss
//! where a probe sits.
//!
//! [`Alerts`] is a state machine fed by [`crate::Probes`]: every report a
//! probe delivers, and every time a probe goes offline or comes back. An
//! alert is raised after `raise_after` bad intervals in a row and resolved
//! after `clear_after` clean ones, so a single dropped packet doesn't page
//! anyone and a flapping stream doesn't raise a new alert every interval.
//! Times come from the reports, so loss a probe reports late (after being
//! cut off from the server) is dated when it happened.

use crate::probes::{Comparison, IntervalReport, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;

/// Resolved alerts kept for listing.
pub const RECENT: usize = 200;

/// When to raise and resolve alerts. Kept with the probe configurations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AlertRules {
    /// Alert when a probe stops checking in.
    pub offline: bool,
    /// Alert on multicast loss at or above `loss_percent`.
    pub multicast_loss: bool,
    /// Packet loss on a multicast stream, in percent over one report
    /// interval, that counts as a bad interval.
    pub loss_percent: f64,
    /// Alert when a probe gets a stream's unicast copy but not the stream.
    pub no_multicast: bool,
    /// Bad intervals in a row before an alert is raised.
    pub raise_after: u32,
    /// Clean intervals in a row before it's resolved.
    pub clear_after: u32,
}

impl Default for AlertRules {
    fn default() -> Self {
        Self {
            offline: true,
            multicast_loss: true,
            loss_percent: 1.0,
            no_multicast: true,
            raise_after: 2,
            clear_after: 3,
        }
    }
}

impl AlertRules {
    pub fn validate(&self) -> Result<(), String> {
        if !(self.loss_percent > 0.0 && self.loss_percent <= 100.0) {
            return Err("loss_percent must be more than 0 and at most 100".into());
        }
        if !(1..=100).contains(&self.raise_after) || !(1..=100).contains(&self.clear_after) {
            return Err("raise_after and clear_after must be between 1 and 100".into());
        }
        Ok(())
    }

    fn enabled(&self, kind: AlertKind) -> bool {
        match kind {
            AlertKind::ProbeOffline => self.offline,
            AlertKind::MulticastLoss => self.multicast_loss,
            AlertKind::NoMulticast => self.no_multicast,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// The probe stopped checking in.
    ProbeOffline,
    /// Loss at or above the threshold on a multicast stream.
    MulticastLoss,
    /// The probe gets a stream's unicast copy but not the stream itself.
    NoMulticast,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: u64,
    pub kind: AlertKind,
    pub probe: String,
    /// The probe's label when the alert was raised.
    pub label: Option<String>,
    /// The stream's name (or its group, if it has none).
    pub stream: Option<String>,
    pub group: Option<SocketAddr>,
    /// One line for people, e.g. for a chat message.
    pub message: String,
    pub raised_ms: u64,
    /// `None` while the alert is active.
    pub resolved_ms: Option<u64>,
    /// For `multicast_loss`: the worst interval's loss while active.
    pub worst_loss_percent: Option<f64>,
    /// For `multicast_loss`, in the interval that raised it: `true` if the
    /// stream's unicast copy arrived without loss (so the multicast path is
    /// at fault), `false` if it lost packets too, `None` if no copy of this
    /// stream arrived (the probe doesn't get copies, or the stream isn't one
    /// this server sends).
    pub unicast_clean: Option<bool>,
}

/// An alert that was raised or resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Raised(Alert),
    Resolved(Alert),
}

/// Bad and good intervals in a row for one (probe, kind, stream).
#[derive(Debug, Default)]
struct Tracker {
    bad: u32,
    good: u32,
    active: Option<u64>,
}

type Key = (String, AlertKind, String);

#[derive(Debug, Default)]
pub struct Alerts {
    next_id: u64,
    active: BTreeMap<u64, Alert>,
    recent: VecDeque<Alert>,
    trackers: HashMap<Key, Tracker>,
}

/// How to name a stream in messages.
fn stream_name(name: Option<&str>, group: Option<SocketAddr>) -> Option<String> {
    name.map(str::to_string).or(group.map(|g| g.to_string()))
}

fn who(probe: &str, label: Option<&str>) -> String {
    match label {
        Some(l) => format!("{probe} ({l})"),
        None => probe.to_string(),
    }
}

impl Alerts {
    pub fn active(&self) -> Vec<Alert> {
        self.active.values().cloned().collect()
    }

    /// Resolved alerts, newest first.
    pub fn recent(&self) -> Vec<Alert> {
        self.recent.iter().rev().cloned().collect()
    }

    fn raise(&mut self, mut alert: Alert, changes: &mut Vec<Change>) -> u64 {
        self.next_id += 1;
        alert.id = self.next_id;
        self.active.insert(alert.id, alert.clone());
        changes.push(Change::Raised(alert));
        self.next_id
    }

    fn resolve(&mut self, id: u64, at_ms: u64, why: Option<&str>, changes: &mut Vec<Change>) {
        if let Some(mut a) = self.active.remove(&id) {
            a.resolved_ms = Some(at_ms.max(a.raised_ms));
            if let Some(why) = why {
                a.message = format!("{} ({why})", a.message);
            }
            if self.recent.len() == RECENT {
                self.recent.pop_front();
            }
            self.recent.push_back(a.clone());
            changes.push(Change::Resolved(a));
        }
    }

    /// A probe went offline (`online` false) or came back.
    pub fn presence(
        &mut self,
        rules: &AlertRules,
        probe: &str,
        label: Option<&str>,
        online: bool,
        last_seen_ms: u64,
        now_ms: u64,
    ) -> Vec<Change> {
        let mut changes = Vec::new();
        let key = (probe.to_string(), AlertKind::ProbeOffline, String::new());
        let active = self.trackers.get(&key).and_then(|t| t.active);
        match (online, active) {
            (false, None) if rules.offline => {
                let id = self.raise(
                    Alert {
                        id: 0,
                        kind: AlertKind::ProbeOffline,
                        probe: probe.to_string(),
                        label: label.map(str::to_string),
                        stream: None,
                        group: None,
                        message: format!("{} stopped checking in", who(probe, label)),
                        raised_ms: last_seen_ms.max(1).min(now_ms),
                        resolved_ms: None,
                        worst_loss_percent: None,
                        unicast_clean: None,
                    },
                    &mut changes,
                );
                self.trackers.entry(key).or_default().active = Some(id);
            }
            (true, Some(id)) => {
                self.trackers.remove(&key);
                self.resolve(id, now_ms, Some("checking in again"), &mut changes);
            }
            _ => {}
        }
        changes
    }

    /// A report a probe delivered. `comparisons` are its multicast vs.
    /// unicast verdicts (see [`crate::probes::compare`]).
    pub fn report(
        &mut self,
        rules: &AlertRules,
        probe: &str,
        label: Option<&str>,
        report: &IntervalReport,
        comparisons: &[Comparison],
    ) -> Vec<Change> {
        let mut changes = Vec::new();
        let at = report.end_ms;

        // Multicast loss, per group.
        let mut seen: HashSet<String> = HashSet::new();
        for row in report
            .streams
            .iter()
            .filter(|r| r.group.ip().is_multicast())
        {
            // Per sender: a group with two senders is two streams.
            let stream = format!("{} from {}", row.group, row.source);
            if !seen.insert(stream.clone()) {
                continue;
            }
            let expected = row.packets + row.lost;
            let loss = if expected == 0 {
                0.0
            } else {
                row.lost as f64 * 100.0 / expected as f64
            };
            let bad = rules.multicast_loss && expected > 0 && loss >= rules.loss_percent;
            // Only when its unicast copy actually arrived: clean, or lossy too.
            let unicast_clean = row
                .ssrc
                .and_then(|s| comparisons.iter().find(|c| c.ssrc == s))
                .filter(|c| c.unicast.packets > 0)
                .map(|c| c.unicast.lost == 0);
            let name = stream_name(row.name.as_deref(), Some(row.group));
            self.step(
                (probe.to_string(), AlertKind::MulticastLoss, stream),
                bad,
                rules,
                at,
                &mut changes,
                |a: Option<&mut Alert>| match a {
                    // Still bad: keep the worst figure.
                    Some(a) => {
                        a.worst_loss_percent = Some(a.worst_loss_percent.unwrap_or(0.0).max(loss));
                        None
                    }
                    None => {
                        let mut message = format!(
                            "{}: {loss:.1}% multicast loss on {}",
                            who(probe, label),
                            name.as_deref().unwrap_or("a stream")
                        );
                        match unicast_clean {
                            Some(true) => message += "; its unicast copy is clean, so look at the multicast path (IGMP snooping/querier, PIM, Wi-Fi)",
                            Some(false) => message += "; its unicast copy is losing packets too, so look at the network or the sender",
                            None => {}
                        }
                        Some(Alert {
                            id: 0,
                            kind: AlertKind::MulticastLoss,
                            probe: probe.to_string(),
                            label: label.map(str::to_string),
                            stream: name.clone(),
                            group: Some(row.group),
                            message,
                            raised_ms: at,
                            resolved_ms: None,
                            worst_loss_percent: Some(loss),
                            unicast_clean,
                        })
                    }
                },
            );
        }

        // No multicast: the unicast copy arrives and the stream doesn't.
        for c in comparisons {
            let stream = format!("ssrc {:08x}", c.ssrc);
            seen.insert(stream.clone());
            let bad = rules.no_multicast && c.verdict == Verdict::NoMulticast;
            let name = stream_name(c.name.as_deref(), c.group);
            self.step(
                (probe.to_string(), AlertKind::NoMulticast, stream),
                bad,
                rules,
                at,
                &mut changes,
                |a: Option<&mut Alert>| {
                    a.is_none().then(|| Alert {
                        id: 0,
                        kind: AlertKind::NoMulticast,
                        probe: probe.to_string(),
                        label: label.map(str::to_string),
                        stream: name.clone(),
                        group: c.group,
                        message: format!(
                            "{}: gets the unicast copy of {} but not the multicast stream \
                             (not joined? IGMP snooping without a querier, routing, TTL)",
                            who(probe, label),
                            name.as_deref().unwrap_or("a stream")
                        ),
                        raised_ms: at,
                        resolved_ms: None,
                        worst_loss_percent: None,
                        unicast_clean: Some(true),
                    })
                },
            );
        }

        // Streams that weren't in this report count as clean intervals, so
        // an alert about a stream that stopped is resolved in time.
        let gone: Vec<Key> = self
            .trackers
            .keys()
            .filter(|(p, k, s)| p == probe && *k != AlertKind::ProbeOffline && !seen.contains(s))
            .cloned()
            .collect();
        for key in gone {
            self.step(key, false, rules, at, &mut changes, |_| None);
        }
        changes
    }

    /// One interval for one tracker. `make` builds the alert to raise
    /// (given `None`), or updates the active one (given `Some`).
    fn step(
        &mut self,
        key: Key,
        bad: bool,
        rules: &AlertRules,
        at: u64,
        changes: &mut Vec<Change>,
        make: impl FnOnce(Option<&mut Alert>) -> Option<Alert>,
    ) {
        // Clean and nothing pending: nothing to track.
        if !bad && !self.trackers.contains_key(&key) {
            return;
        }
        let (active, nbad, ngood) = {
            let t = self.trackers.entry(key.clone()).or_default();
            if bad {
                t.bad += 1;
                t.good = 0;
            } else {
                t.good += 1;
                t.bad = 0;
            }
            (t.active, t.bad, t.good)
        };
        match (active, bad) {
            (Some(id), true) => {
                make(self.active.get_mut(&id));
            }
            (Some(id), false) if ngood >= rules.clear_after => {
                self.trackers.remove(&key);
                self.resolve(id, at, None, changes);
            }
            (None, true) if nbad >= rules.raise_after => {
                if let Some(alert) = make(None) {
                    let id = self.raise(alert, changes);
                    if let Some(t) = self.trackers.get_mut(&key) {
                        t.active = Some(id);
                    }
                }
            }
            (None, false) if ngood >= rules.clear_after => {
                // Nothing pending: stop tracking it.
                self.trackers.remove(&key);
            }
            _ => {}
        }
    }

    /// The probe was forgotten: resolve its alerts.
    pub fn forget(&mut self, probe: &str, now_ms: u64) -> Vec<Change> {
        let mut changes = Vec::new();
        let ids: Vec<u64> = self
            .active
            .values()
            .filter(|a| a.probe == probe)
            .map(|a| a.id)
            .collect();
        for id in ids {
            self.resolve(id, now_ms, Some("probe forgotten"), &mut changes);
        }
        self.trackers.retain(|(p, _, _), _| p != probe);
        changes
    }

    /// The rules changed: resolve alerts of kinds now turned off.
    pub fn rules_changed(&mut self, rules: &AlertRules, now_ms: u64) -> Vec<Change> {
        let mut changes = Vec::new();
        let ids: Vec<u64> = self
            .active
            .values()
            .filter(|a| !rules.enabled(a.kind))
            .map(|a| a.id)
            .collect();
        for id in ids {
            self.resolve(id, now_ms, Some("alert turned off"), &mut changes);
        }
        self.trackers.retain(|(_, k, _), _| rules.enabled(*k));
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::compare;
    use crate::watch::StreamRow;

    fn row(group: &str, ssrc: u32, packets: u64, lost: u64) -> StreamRow {
        StreamRow {
            group: group.parse().unwrap(),
            name: Some("Zone 1".into()),
            source: "10.0.0.1:40000".parse().unwrap(),
            ssrc: Some(ssrc),
            payload_type: Some(0),
            codec: None,
            packets,
            bytes: 0,
            packets_per_sec: 0.0,
            kbit_per_sec: 0.0,
            lost,
            late: 0,
            loss_percent: 0.0,
            jitter_ms: None,
            max_gap_ms: 0.0,
            ptime_ms: None,
            ttl: None,
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
            end_ms: seq * 10_000 + 10_000,
            streams,
            groups: Vec::new(),
        }
    }

    /// Feed `loss` (lost packets per 100) as one report per entry; return
    /// what changed after each.
    fn run(a: &mut Alerts, rules: &AlertRules, losses: &[u64], unicast: bool) -> Vec<String> {
        losses
            .iter()
            .enumerate()
            .map(|(i, &lost)| {
                let mut streams = vec![row("239.1.1.1:5004", 7, 100 - lost, lost)];
                if unicast {
                    streams.push(row("0.0.0.0:5104", 7, 100, 0));
                }
                let r = report(i as u64 + 1, streams);
                let c = compare(&r);
                a.report(rules, "pi", Some("IDF"), &r, &c)
                    .iter()
                    .map(|c| match c {
                        Change::Raised(a) => format!("raise {:?}", a.kind),
                        Change::Resolved(a) => format!("resolve {:?}", a.kind),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect()
    }

    #[test]
    fn loss_raises_after_two_and_clears_after_three() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        // One bad interval alone doesn't raise; loss under 1% never counts.
        let got = run(&mut a, &rules, &[5, 0, 0, 2, 3, 4, 0, 0, 0, 0], false);
        assert_eq!(
            got,
            [
                "",
                "",
                "",
                "",
                "raise MulticastLoss",
                "",
                "",
                "",
                "resolve MulticastLoss",
                ""
            ]
        );
        let r = &a.recent()[0];
        assert_eq!(r.worst_loss_percent, Some(4.0));
        assert_eq!(r.unicast_clean, None);
        assert!(
            r.message
                .contains("pi (IDF): 3.0% multicast loss on Zone 1"),
            "{}",
            r.message
        );
        assert!(a.active().is_empty());
    }

    #[test]
    fn each_sender_on_a_group_counts() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        for seq in 1..=2 {
            let mut second = row("239.1.1.1:5004", 8, 90, 10);
            second.source = "10.0.0.2:40000".parse().unwrap();
            let r = report(seq, vec![row("239.1.1.1:5004", 7, 100, 0), second]);
            a.report(&rules, "pi", None, &r, &compare(&r));
        }
        assert_eq!(a.active().len(), 1, "the lossy second sender raises");
    }

    #[test]
    fn a_missing_copy_is_not_a_lossy_one() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        for seq in 1..=2 {
            let r = report(
                seq,
                vec![
                    row("239.1.1.1:5004", 7, 90, 10),
                    row("0.0.0.0:5104", 7, 0, 0),
                ],
            );
            a.report(&rules, "pi", None, &r, &compare(&r));
        }
        let alert = &a.active()[0];
        assert_eq!(alert.unicast_clean, None);
        assert!(
            !alert.message.contains("losing packets too"),
            "{}",
            alert.message
        );
    }

    #[test]
    fn says_when_the_unicast_copy_is_clean() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        run(&mut a, &rules, &[10, 10], true);
        let active = a.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].unicast_clean, Some(true));
        assert!(active[0].message.contains("look at the multicast path"));
    }

    #[test]
    fn no_multicast_and_a_stream_that_stops() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        let only_unicast = |seq| report(seq, vec![row("0.0.0.0:5104", 9, 100, 0)]);
        for seq in 1..=2 {
            let r = only_unicast(seq);
            a.report(&rules, "pi", None, &r, &compare(&r));
        }
        assert_eq!(a.active()[0].kind, AlertKind::NoMulticast);
        // The stream ends: three empty reports resolve it.
        let mut resolved = Vec::new();
        for seq in 3..=5 {
            resolved.extend(a.report(&rules, "pi", None, &report(seq, vec![]), &[]));
        }
        assert!(matches!(resolved.as_slice(), [Change::Resolved(_)]));
        assert!(a.trackers.is_empty(), "nothing left to track");
    }

    #[test]
    fn offline_and_back() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        let c = a.presence(&rules, "pi", None, false, 1000, 40_000);
        assert!(matches!(&c[..], [Change::Raised(x)] if x.raised_ms == 1000));
        assert!(a
            .presence(&rules, "pi", None, false, 1000, 41_000)
            .is_empty());
        let c = a.presence(&rules, "pi", None, true, 50_000, 50_000);
        assert!(
            matches!(&c[..], [Change::Resolved(x)] if x.message.ends_with("(checking in again)"))
        );
        // Turned off: nothing.
        let off = AlertRules {
            offline: false,
            ..AlertRules::default()
        };
        assert!(a.presence(&off, "pi", None, false, 1, 2).is_empty());
    }

    #[test]
    fn forgetting_or_turning_off_resolves() {
        let rules = AlertRules::default();
        let mut a = Alerts::default();
        run(&mut a, &rules, &[10, 10], false);
        a.presence(&rules, "pi", None, false, 1, 2);
        assert_eq!(a.active().len(), 2);
        let off = AlertRules {
            multicast_loss: false,
            ..AlertRules::default()
        };
        assert_eq!(a.rules_changed(&off, 5).len(), 1);
        assert_eq!(a.forget("pi", 6).len(), 1);
        assert!(a.active().is_empty());
        assert_eq!(a.recent().len(), 2);
    }

    #[test]
    fn validates_rules() {
        assert!(AlertRules::default().validate().is_ok());
        for bad in [
            AlertRules {
                loss_percent: 0.0,
                ..AlertRules::default()
            },
            AlertRules {
                raise_after: 0,
                ..AlertRules::default()
            },
        ] {
            assert!(bad.validate().is_err());
        }
    }
}
