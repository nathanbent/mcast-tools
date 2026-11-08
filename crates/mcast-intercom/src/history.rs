//! The play history: every run that ended, kept on disk so it outlives a
//! restart. One file a day (`history/2026-11-08.jsonl`, by UTC date, beside
//! `intercom.json`), one JSON line per run: `{"run": RUN, "logged_ms"}`.
//! Days older than the config's `history_days` are deleted (0 keeps them all).
//!
//! The runner tells [`History::observe`] about every change; a run is written
//! once, when it's over (missed schedules included) and, if its zones expect
//! listeners, once it's known who heard it.

use crate::now_ms;
use crate::runner::{Change, Run};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// Days kept unless the config says otherwise.
pub const DEFAULT_DAYS: u32 = 90;
/// Most runs one query returns.
pub const MAX_LIMIT: usize = 1000;
/// Where connection changes go, beside the day files.
const CONNECTION_FILE: &str = "connection.jsonl";
/// Connection changes kept.
const MAX_CONNECTION_LINES: usize = 5000;
/// A scheduled play that starts this much after its time counts as late.
pub const LATE_MS: u64 = 5000;

/// `history/` beside the config file.
pub fn default_dir(config: &Path) -> PathBuf {
    config.with_file_name("history")
}

fn day_of(ms: u64) -> jiff::civil::Date {
    jiff::Timestamp::from_millisecond(ms as i64)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date()
}

/// The day a file holds, from its name.
fn file_day(path: &Path) -> Option<jiff::civil::Date> {
    let name = path.file_name()?.to_str()?;
    name.strip_suffix(".jsonl")?.parse().ok()
}

/// Which runs to return, newest first.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Started at or after (ms).
    pub from_ms: Option<u64>,
    /// Started before (ms).
    pub to_ms: Option<u64>,
    pub announcement: Option<String>,
    /// A run state: `done`, `stopped`, `failed`, `missed`.
    pub state: Option<String>,
    /// Part of the name of who started it (`trigger.by`), any case.
    pub by: Option<String>,
    /// Part of the announcement's name, any case.
    pub text: Option<String>,
    /// For the next page: runs that sort after this one, as
    /// (`started_ms`, `id`). Runs can start in the same millisecond.
    pub before: Option<(u64, u64)>,
    pub limit: usize,
}

/// Where a run sorts: newest first, then by id.
fn key(r: &Value) -> (u64, u64) {
    (
        r["started_ms"].as_u64().unwrap_or(0),
        r["id"].as_u64().unwrap_or(0),
    )
}

pub struct History {
    dir: PathBuf,
    keep_days: AtomicU32,
    st: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Runs written, as (id, started): ids start again after a restart.
    written: VecDeque<(u64, u64)>,
    /// The last day old files were looked at.
    pruned: Option<jiff::civil::Date>,
}

impl History {
    pub fn new(dir: PathBuf, keep_days: u32) -> Self {
        Self {
            dir,
            keep_days: AtomicU32::new(keep_days),
            st: Mutex::default(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many days to keep (0: all); old ones go at the next write.
    pub fn set_keep_days(&self, days: u32) {
        if self.keep_days.swap(days, Ordering::Relaxed) != days {
            self.lock().pruned = None;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Hear about a change from the runner: write a run once it's over,
    /// and note when the mcast server comes and goes.
    pub fn observe(&self, c: &Change<'_>) {
        match c {
            // Once it's known who heard it, too.
            Change::Run(run) if run.state.is_over() && !run.checking() => self.write(run),
            Change::Backend(status) => self.connection(Some(status.connected), status.since_ms),
            _ => {}
        }
    }

    /// The panel started or stopped: what the connection was until the next
    /// change isn't known.
    pub fn panel_up_or_down(&self) {
        self.connection(None, now_ms());
    }

    /// `connection.jsonl`: `{"at_ms", "connected"}` lines, `null` for unknown.
    fn connection(&self, connected: Option<bool>, at_ms: u64) {
        let _held = self.lock();
        let line = json!({ "at_ms": at_ms, "connected": connected }).to_string();
        let path = self.dir.join(CONNECTION_FILE);
        let wrote = std::fs::create_dir_all(&self.dir).and_then(|_| {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            f.write_all(format!("{line}\n").as_bytes())
        });
        if let Err(e) = wrote {
            eprintln!("mcast intercom: writing {}: {e}", path.display());
        }
    }

    /// The connection log, oldest first: (at_ms, connected or unknown).
    pub fn connection_log(&self) -> Vec<(u64, Option<bool>)> {
        let Ok(f) = std::fs::File::open(self.dir.join(CONNECTION_FILE)) else {
            return Vec::new();
        };
        let mut out: Vec<(u64, Option<bool>)> = std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .filter_map(|v| Some((v["at_ms"].as_u64()?, v["connected"].as_bool())))
            .collect();
        out.sort_by_key(|(at, _)| *at);
        out
    }

    fn write(&self, run: &Run) {
        let mut st = self.lock();
        let key = (run.id, run.started_ms);
        if st.written.contains(&key) {
            return;
        }
        st.written.push_back(key);
        if st.written.len() > 512 {
            st.written.pop_front();
        }
        let now = now_ms();
        let today = day_of(now);
        if st.pruned != Some(today) {
            st.pruned = Some(today);
            self.prune(today);
        }
        let line = json!({ "run": run, "logged_ms": now }).to_string();
        let path = self
            .dir
            .join(format!("{}.jsonl", day_of(run.started_ms.max(1))));
        let wrote = std::fs::create_dir_all(&self.dir).and_then(|_| {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            f.write_all(format!("{line}\n").as_bytes())
        });
        if let Err(e) = wrote {
            eprintln!(
                "mcast intercom: writing the history to {}: {e}",
                path.display()
            );
        }
    }

    /// Delete days older than the ones kept, and keep the connection log short.
    fn prune(&self, today: jiff::civil::Date) {
        let conn = self.dir.join(CONNECTION_FILE);
        if let Ok(text) = std::fs::read_to_string(&conn) {
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() > MAX_CONNECTION_LINES {
                let keep = lines[lines.len() - MAX_CONNECTION_LINES..].join("\n") + "\n";
                let _ = std::fs::write(&conn, keep);
            }
        }
        let days = self.keep_days.load(Ordering::Relaxed);
        if days == 0 {
            return;
        }
        let Ok(first) = today.checked_sub(jiff::Span::new().days(days as i64 - 1)) else {
            return;
        };
        for path in self.files() {
            if file_day(&path).is_some_and(|d| d < first) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// The day files, newest first.
    fn files(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| file_day(p).is_some())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files.reverse();
        files
    }

    /// Runs that match, newest first, and whether there are more (for
    /// another page: `before` = the last one's (`started_ms`, `id`)). `keep` says
    /// which runs the caller may see.
    pub fn query(&self, q: &Query, keep: impl Fn(&Value) -> bool) -> (Vec<Value>, bool) {
        let limit = q.limit.clamp(1, MAX_LIMIT);
        let lower = |s: &Option<String>| s.as_ref().map(|t| t.to_lowercase());
        let (by, text) = (lower(&q.by), lower(&q.text));
        let upper = match (q.to_ms, q.before) {
            (Some(t), Some(b)) => Some(t.min(b.0)),
            (t, b) => t.or(b.map(|b| b.0)),
        };
        let (from_day, to_day) = (q.from_ms.map(day_of), upper.map(day_of));
        let mut out = Vec::new();
        for path in self.files() {
            let Some(day) = file_day(&path) else { continue };
            if from_day.is_some_and(|f| day < f) {
                break;
            }
            if to_day.is_some_and(|t| day > t) {
                continue;
            }
            let Ok(f) = std::fs::File::open(&path) else {
                continue;
            };
            let mut runs: Vec<Value> = std::io::BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
                .filter_map(|v| v.get("run").cloned())
                .filter(|r| {
                    let started = r["started_ms"].as_u64().unwrap_or(0);
                    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
                    q.from_ms.is_none_or(|f| started >= f)
                        && q.to_ms.is_none_or(|t| started < t)
                        && q.before.is_none_or(|b| key(r) < b)
                        && q.announcement
                            .as_ref()
                            .is_none_or(|a| s("announcement") == *a)
                        && q.state.as_ref().is_none_or(|st| s("state") == *st)
                        && by.as_ref().is_none_or(|b| {
                            r["trigger"]["by"]
                                .as_str()
                                .is_some_and(|x| x.to_lowercase().contains(b))
                        })
                        && text
                            .as_ref()
                            .is_none_or(|t| s("name").to_lowercase().contains(t))
                        && keep(r)
                })
                .collect();
            runs.sort_by_key(|r| std::cmp::Reverse(key(r)));
            for r in runs {
                if out.len() == limit {
                    return (out, true);
                }
                out.push(r);
            }
        }
        (out, false)
    }
}

/// What the Panel's stats strip shows, counted from the log: see
/// [`History::stats`].
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Counts {
    /// Runs that ended.
    pub plays: u64,
    /// Played to every group they reached (`done`).
    pub played: u64,
    pub stopped: u64,
    pub failed: u64,
    pub missed: u64,
}

impl Counts {
    fn add(&mut self, r: &Value) {
        self.plays += 1;
        match r["state"].as_str() {
            Some("done") => self.played += 1,
            Some("stopped") => self.stopped += 1,
            Some("failed") => self.failed += 1,
            Some("missed") => self.missed += 1,
            _ => {}
        }
    }
}

impl History {
    /// Every logged run that started at or after `from_ms`, oldest day
    /// first (in no particular order within a day).
    pub fn scan(&self, from_ms: u64, mut f: impl FnMut(&Value)) {
        let first = day_of(from_ms);
        let mut files = self.files();
        files.reverse();
        for path in files {
            if file_day(&path).is_none_or(|d| d < first) {
                continue;
            }
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v["run"]["started_ms"].as_u64().unwrap_or(0) >= from_ms {
                    f(&v["run"]);
                }
            }
        }
    }

    /// Plays since `today_ms` and since `week_ms`, and groups busy or failed
    /// since `day_ago_ms`; only runs `keep` lets through.
    pub fn stats(
        &self,
        today_ms: u64,
        week_ms: u64,
        day_ago_ms: u64,
        keep: impl Fn(&Value) -> bool,
    ) -> Value {
        let (mut today, mut week) = (Counts::default(), Counts::default());
        let (mut busy, mut failed) = (0u64, 0u64);
        self.scan(week_ms.min(day_ago_ms), |r| {
            if !keep(r) {
                return;
            }
            let started = r["started_ms"].as_u64().unwrap_or(0);
            if started >= week_ms {
                week.add(r);
            }
            if started >= today_ms {
                today.add(r);
            }
            if started >= day_ago_ms {
                for t in r["targets"].as_array().into_iter().flatten() {
                    match t["state"].as_str() {
                        Some("busy") => busy += 1,
                        Some("failed") => failed += 1,
                        _ => {}
                    }
                }
            }
        });
        json!({
            "today": today,
            "week": week,
            "last_24h": { "busy_groups": busy, "failed_groups": failed },
        })
    }
}

/// Whether a logged run played at site `id`: by the sites it recorded, or,
/// for runs logged before runs recorded them, by the zones its groups are
/// in now.
pub fn at_site(run: &Value, id: &str, cfg: &crate::model::Config) -> bool {
    if let Some(sites) = run["sites"].as_array() {
        return sites.iter().any(|s| s == id);
    }
    if run.get("sites").is_some() {
        return false;
    }
    // No `sites`: an older run, or one at no site. Look at its groups.
    run["targets"].as_array().into_iter().flatten().any(|t| {
        let group = t["group"].as_str().unwrap_or("").trim();
        cfg.zones.iter().any(|z| {
            z.site.as_deref() == Some(id)
                && z.targets
                    .iter()
                    .any(|zt| zt.group.trim().eq_ignore_ascii_case(group))
        })
    })
}

/// What the Monitor tab shows, from `runs` (logged runs, any order):
/// totals; plays a day (`days`: (date, start ms, end ms), oldest first); by
/// announcement; by zone (`zones_of` names the zones, as (id, name), a
/// group is in); how schedules kept time; what started plays; and, for
/// zones that list them, which listeners heard plays and which missed them.
pub fn summarize(
    runs: &[Value],
    days: &[(String, u64, u64)],
    zones_of: &dyn Fn(&str) -> Vec<(String, String)>,
) -> Value {
    use std::collections::BTreeMap;
    let mut totals = Counts::default();
    let mut per_day: Vec<Counts> = vec![Counts::default(); days.len()];
    let mut anns: BTreeMap<String, (String, Counts)> = BTreeMap::new();
    #[derive(Default)]
    struct Zone {
        name: String,
        sends: u64,
        played: u64,
        busy: u64,
        failed: u64,
        taken_over: u64,
    }
    let mut zones: BTreeMap<String, Zone> = BTreeMap::new();
    let (mut on_time, mut late, mut missed, mut worst_late) = (0u64, 0u64, 0u64, 0u64);
    let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    #[derive(Default)]
    struct Ear {
        name: String,
        expected: u64,
        heard: u64,
        missed: u64,
        last_heard_ms: Option<u64>,
        worst_loss: Option<f64>,
    }
    // By name, any case.
    let mut ears: BTreeMap<String, Ear> = BTreeMap::new();
    let (mut checked, mut with_silent) = (0u64, 0u64);
    for r in runs {
        let started = r["started_ms"].as_u64().unwrap_or(0);
        totals.add(r);
        if let Some(i) = days
            .iter()
            .position(|(_, from, to)| started >= *from && started < *to)
        {
            per_day[i].add(r);
        }
        let id = r["announcement"].as_str().unwrap_or("").to_string();
        let name = r["name"].as_str().unwrap_or("").to_string();
        anns.entry(id)
            .or_insert_with(|| (name, Counts::default()))
            .1
            .add(r);
        for t in r["targets"].as_array().into_iter().flatten() {
            let group = t["group"].as_str().unwrap_or("");
            let mut found = zones_of(group);
            if found.is_empty() {
                found.push((String::new(), format!("{group} (in no zone now)")));
            }
            for (zid, zname) in found {
                let key = if zid.is_empty() { zname.clone() } else { zid };
                let z = zones.entry(key).or_insert_with(|| Zone {
                    name: zname,
                    ..Default::default()
                });
                z.sends += 1;
                if t["played"].as_u64().unwrap_or(0) > 0 {
                    z.played += 1;
                }
                match t["state"].as_str() {
                    Some("busy") => z.busy += 1,
                    Some("failed") => z.failed += 1,
                    Some("preempted") => z.taken_over += 1,
                    _ => {}
                }
            }
        }
        let kind = r["trigger"]["type"]
            .as_str()
            .unwrap_or("manual")
            .to_string();
        if kind == "schedule" {
            if r["state"] == "missed" {
                missed += 1;
            } else {
                let due = r["trigger"]["at_ms"].as_u64().unwrap_or(started);
                let behind = started.saturating_sub(due);
                worst_late = worst_late.max(behind);
                if behind > LATE_MS {
                    late += 1;
                } else {
                    on_time += 1;
                }
            }
        }
        *kinds.entry(kind).or_default() += 1;
        if let Some(b) = r["trigger"]["by"].as_str() {
            *by.entry(b.to_string()).or_default() += 1;
        }
        let l = &r["listeners"];
        // Only checks that finished and could tell.
        if l.is_object() && l["checking"] != true && l["error"].is_null() {
            checked += 1;
            if l["silent"].as_array().is_some_and(|s| !s.is_empty()) {
                with_silent += 1;
            }
            fn ear<'a>(ears: &'a mut BTreeMap<String, Ear>, name: &str) -> &'a mut Ear {
                ears.entry(name.to_lowercase()).or_insert_with(|| Ear {
                    name: name.to_string(),
                    ..Default::default()
                })
            }
            let heard: Vec<(&str, Option<f64>)> = l["heard"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| Some((h["name"].as_str()?, h["loss_percent"].as_f64())))
                .collect();
            for e in l["expected"].as_array().into_iter().flatten() {
                let Some(name) = e.as_str() else { continue };
                let e = ear(&mut ears, name);
                e.expected += 1;
                if !heard.iter().any(|(h, _)| h.eq_ignore_ascii_case(name)) {
                    e.missed += 1;
                }
            }
            for (name, loss) in heard {
                let e = ear(&mut ears, name);
                e.heard += 1;
                e.last_heard_ms = e.last_heard_ms.max(Some(started));
                if loss > e.worst_loss {
                    e.worst_loss = loss;
                }
            }
        }
    }
    let mut anns: Vec<Value> = anns
        .into_iter()
        .map(|(id, (name, c))| {
            let mut v = serde_json::to_value(c).unwrap_or_default();
            v["id"] = id.into();
            v["name"] = name.into();
            v
        })
        .collect();
    anns.sort_by_key(|v| std::cmp::Reverse(v["plays"].as_u64().unwrap_or(0)));
    let mut zones: Vec<Value> = zones
        .into_iter()
        .map(|(id, z)| {
            json!({ "id": id, "name": z.name, "sends": z.sends, "played": z.played,
                    "busy": z.busy, "failed": z.failed, "taken_over": z.taken_over })
        })
        .collect();
    zones.sort_by_key(|v| std::cmp::Reverse(v["sends"].as_u64().unwrap_or(0)));
    let mut ears: Vec<Ear> = ears.into_values().collect();
    // The ones missing the most first.
    ears.sort_by_key(|e| (std::cmp::Reverse(e.missed), e.name.to_lowercase()));
    let mut by: Vec<(String, u64)> = by.into_iter().collect();
    by.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    by.truncate(10);
    json!({
        "totals": totals,
        "days": days.iter().zip(per_day).map(|((date, _, _), c)| {
            let mut v = serde_json::to_value(c).unwrap_or_default();
            v["date"] = date.clone().into();
            v
        }).collect::<Vec<_>>(),
        "announcements": anns,
        "zones": zones,
        "schedules": { "on_time": on_time, "late": late, "missed": missed, "late_after_ms": LATE_MS, "most_late_ms": worst_late },
        "triggers": kinds,
        "by": by.into_iter().map(|(name, plays)| json!({ "name": name, "plays": plays })).collect::<Vec<_>>(),
        "listeners": {
            "checked": checked,
            "with_silent": with_silent,
            "list": ears.into_iter().map(|e| json!({
                "name": e.name, "expected": e.expected, "heard": e.heard, "missed": e.missed,
                "last_heard_ms": e.last_heard_ms, "worst_loss_percent": e.worst_loss,
            })).collect::<Vec<_>>(),
        },
    })
}

/// How long the mcast server was reachable between `from_ms` and `to_ms`,
/// from the connection log: `{"known_ms", "up_ms", "uptime", "outages":
/// [{"from_ms", "to_ms"}]}`. Time the panel wasn't running isn't known and
/// isn't counted; `uptime` is `null` when none of it is known.
pub fn uptime(log: &[(u64, Option<bool>)], from_ms: u64, to_ms: u64) -> Value {
    // The state at `from_ms`: the last change before it.
    let mut state = log
        .iter()
        .rev()
        .find(|(at, _)| *at <= from_ms)
        .and_then(|(_, c)| *c);
    let mut at = from_ms;
    let (mut known, mut up) = (0u64, 0u64);
    let mut outages: Vec<(u64, Option<u64>)> = Vec::new();
    if state == Some(false) {
        outages.push((from_ms, None));
    }
    let mut span = |state: Option<bool>, from: u64, to: u64| {
        if let Some(c) = state {
            known += to.saturating_sub(from);
            if c {
                up += to.saturating_sub(from);
            }
        }
    };
    for (t, c) in log.iter().filter(|(t, _)| *t > from_ms && *t < to_ms) {
        span(state, at, *t);
        if state == Some(false) && *c != Some(false) {
            if let Some(o) = outages.last_mut() {
                o.1 = Some(*t);
            }
        }
        if *c == Some(false) && state != Some(false) {
            outages.push((*t, None));
        }
        state = *c;
        at = *t;
    }
    span(state, at, to_ms);
    if state != Some(false) {
        // An outage the panel didn't see the end of (it stopped) ends unknown.
        if let Some(o) = outages.last_mut().filter(|o| o.1.is_none()) {
            o.1 = Some(at);
        }
    }
    let n = outages.len();
    json!({
        "known_ms": known,
        "up_ms": up,
        "uptime": (known > 0).then(|| up as f64 / known as f64),
        "outages": outages[n.saturating_sub(20)..].iter().rev()
            .map(|(f, t)| json!({ "from_ms": f, "to_ms": t })).collect::<Vec<_>>(),
    })
}

/// A run as a CSV row, times in `tz`: see [`CSV_HEADER`].
pub fn csv_row(r: &Value, tz: &jiff::tz::TimeZone) -> String {
    let time = |k: &str| {
        r[k].as_u64()
            .and_then(|ms| jiff::Timestamp::from_millisecond(ms as i64).ok())
            .map(|t| {
                t.to_zoned(tz.clone())
                    .strftime("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let targets = r["targets"].as_array().cloned().unwrap_or_default();
    let count = |st: &str| targets.iter().filter(|t| t["state"] == st).count();
    let played = targets
        .iter()
        .filter(|t| t["played"].as_u64().unwrap_or(0) > 0)
        .count();
    let groups: Vec<String> = targets
        .iter()
        .map(|t| t["group"].as_str().unwrap_or("").to_string())
        .collect();
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    [
        time("started_ms"),
        time("ended_ms"),
        s(&r["announcement"]),
        s(&r["name"]),
        s(&r["state"]),
        s(&r["trigger"]["type"]),
        s(&r["trigger"]["by"]),
        groups.join(" "),
        played.to_string(),
        count("busy").to_string(),
        count("failed").to_string(),
        s(&r["error"]),
    ]
    .iter()
    .map(|f| csv_field(f))
    .collect::<Vec<_>>()
    .join(",")
}

pub const CSV_HEADER: &str =
    "started,ended,announcement,name,state,trigger,by,groups,played,busy,failed,error";

/// Quote a field if it needs it; a leading `=+-@` is defused so a
/// spreadsheet doesn't run it as a formula.
fn csv_field(f: &str) -> String {
    let f = if f.starts_with(['=', '+', '-', '@']) {
        format!("'{f}")
    } else {
        f.to_string()
    };
    if f.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", f.replace('"', "\"\""))
    } else {
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RunState;

    fn dir() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "mcast-history-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn run(id: u64, started_ms: u64, state: RunState) -> Run {
        let mut r = Run::for_tests(id);
        r.started_ms = started_ms;
        r.state = state;
        r
    }

    #[test]
    fn waits_to_write_a_run_until_its_listeners_are_known() {
        use crate::runner::{HeardBy, Listeners};
        let d = dir();
        let h = History::new(d.clone(), 0);
        let t0 = 1_800_000_000_000;
        let mut r = run(1, t0, RunState::Done);
        r.listeners = Some(Box::new(Listeners {
            checking: true,
            expected: vec!["Lobby".into()],
            ..Listeners::default()
        }));
        h.observe(&Change::Run(&r));
        let all = |h: &History| {
            h.query(
                &Query {
                    limit: 10,
                    ..Default::default()
                },
                |_| true,
            )
            .0
        };
        assert!(all(&h).is_empty(), "not while still listening");
        let l = r.listeners.as_mut().unwrap();
        l.checking = false;
        l.heard = vec![HeardBy {
            name: "Lobby".into(),
            loss_percent: Some(0.5),
        }];
        h.observe(&Change::Run(&r));
        h.observe(&Change::Run(&r));
        let runs = all(&h);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["listeners"]["heard"][0]["name"], "Lobby");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn writes_ended_runs_once_and_finds_them() {
        let d = dir();
        let h = History::new(d.clone(), 0);
        let day = 86_400_000;
        let t0 = 1_800_000_000_000; // 2027-01-15
        h.observe(&Change::Run(&run(1, t0, RunState::Playing)));
        h.observe(&Change::Run(&run(1, t0, RunState::Done)));
        h.observe(&Change::Run(&run(1, t0, RunState::Done)));
        h.observe(&Change::Run(&run(2, t0 + 1000, RunState::Failed)));
        // After a restart, ids start again: a different run.
        h.observe(&Change::Run(&run(1, t0 + day, RunState::Stopped)));
        let all = h
            .query(
                &Query {
                    limit: 10,
                    ..Default::default()
                },
                |_| true,
            )
            .0;
        let ids: Vec<(u64, &str)> = all
            .iter()
            .map(|r| (r["id"].as_u64().unwrap(), r["state"].as_str().unwrap()))
            .collect();
        assert_eq!(ids, [(1, "stopped"), (2, "failed"), (1, "done")]);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2, "a file a day");

        let q = |q: Query| h.query(&q, |_| true);
        let (first, more) = q(Query {
            limit: 1,
            ..Default::default()
        });
        assert!(more && first.len() == 1);
        let (next, more) = q(Query {
            limit: 5,
            to_ms: Some(t0 + day),
            ..Default::default()
        });
        assert_eq!((next.len(), more), (2, false));
        let (failed, _) = q(Query {
            limit: 5,
            state: Some("failed".into()),
            ..Default::default()
        });
        assert_eq!(failed.len(), 1);
        let (day1, _) = q(Query {
            limit: 5,
            from_ms: Some(t0),
            to_ms: Some(t0 + day),
            ..Default::default()
        });
        assert_eq!(day1.len(), 2);
        let (none, _) = h.query(
            &Query {
                limit: 5,
                ..Default::default()
            },
            |r| r["state"] == "nope",
        );
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn pages_dont_lose_runs_that_started_together() {
        let d = dir();
        let h = History::new(d.clone(), 0);
        let t0 = 1_800_000_000_000;
        for id in 1..=3 {
            h.observe(&Change::Run(&run(id, t0, RunState::Done)));
        }
        let mut seen = Vec::new();
        let mut before = None;
        loop {
            let (page, more) = h.query(
                &Query {
                    limit: 1,
                    before,
                    ..Default::default()
                },
                |_| true,
            );
            for r in &page {
                seen.push(r["id"].as_u64().unwrap());
                before = Some((r["started_ms"].as_u64().unwrap(), r["id"].as_u64().unwrap()));
            }
            if !more {
                break;
            }
        }
        assert_eq!(seen, [3, 2, 1]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn counts_today_the_week_and_problems() {
        let d = dir();
        let h = History::new(d.clone(), 0);
        let day = 86_400_000;
        let now = 1_800_000_000_000;
        // A failed run with a busy group and a failed one, written as logged.
        let mut failed = serde_json::to_value(run(2, now - 1000, RunState::Failed)).unwrap();
        failed["targets"] =
            json!([{"group": "a", "state": "busy"}, {"group": "b", "state": "failed"}]);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(format!("{}.jsonl", day_of(now - 1000))),
            format!("{}\n", json!({"run": failed, "logged_ms": now})),
        )
        .unwrap();
        for r in [
            run(1, now - 2000, RunState::Done),
            run(3, now - 2 * day, RunState::Missed),
            run(4, now - 10 * day, RunState::Done),
        ] {
            h.observe(&Change::Run(&r));
        }
        let s = h.stats(now - 3_600_000, now - 6 * day, now - day, |_| true);
        assert_eq!(
            s["today"],
            json!({"plays": 2, "played": 1, "stopped": 0, "failed": 1, "missed": 0})
        );
        assert_eq!(s["week"]["plays"], 3);
        assert_eq!(s["week"]["missed"], 1);
        assert_eq!(s["last_24h"], json!({"busy_groups": 1, "failed_groups": 1}));
        let none = h.stats(now - 3_600_000, now - 6 * day, now - day, |_| false);
        assert_eq!(none["week"]["plays"], 0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn uptime_counts_only_what_was_seen() {
        let log = vec![
            (0, None),          // panel started
            (100, Some(true)),  // connected
            (500, Some(false)), // lost
            (700, Some(true)),  // back
            (900, None),        // panel stopped
            (1500, None),       // started again
            (1600, Some(true)),
        ];
        let u = uptime(&log, 0, 2000);
        // Known: 100-900 (800) and 1600-2000 (400); up: 400 + 200 + 400.
        assert_eq!(
            (u["known_ms"].clone(), u["up_ms"].clone()),
            (json!(1200), json!(1000))
        );
        assert_eq!(u["outages"], json!([{"from_ms": 500, "to_ms": 700}]));
        // Starting mid-outage.
        let u = uptime(&log, 600, 800);
        assert_eq!(
            (u["known_ms"].clone(), u["up_ms"].clone()),
            (json!(200), json!(100))
        );
        assert_eq!(u["outages"], json!([{"from_ms": 600, "to_ms": 700}]));
        assert_eq!(uptime(&[], 0, 10)["uptime"], Value::Null);
    }

    #[test]
    fn tells_which_site_a_run_played_at() {
        let cfg: crate::model::Config = serde_json::from_value(json!({
            "sites": [{"id": "north", "name": "North"}, {"id": "south", "name": "South"}],
            "zones": [
                {"id": "n", "name": "N", "site": "north", "targets": [{"group": "g-north"}]},
                {"id": "s", "name": "S", "site": "south", "targets": [{"group": "g-south"}]},
                {"id": "x", "name": "X", "targets": [{"group": "g-none"}]}
            ]
        }))
        .unwrap();
        // Recorded: as it says.
        let r = json!({"sites": ["north"], "targets": [{"group": "g-south"}]});
        assert!(at_site(&r, "north", &cfg));
        assert!(!at_site(&r, "south", &cfg));
        // Older, without `sites`: by its groups' zones now.
        let old = json!({"targets": [{"group": "G-SOUTH "}, {"group": "g-none"}]});
        assert!(at_site(&old, "south", &cfg));
        assert!(!at_site(&old, "north", &cfg));
        // At no site.
        let none = json!({"targets": [{"group": "g-none"}]});
        assert!(!at_site(&none, "north", &cfg) && !at_site(&none, "south", &cfg));
    }

    #[test]
    fn summarizes_who_heard_plays() {
        let r = |started: u64, listeners: Value| {
            json!({"id": started, "announcement": "a", "name": "A", "started_ms": started,
                   "state": "done", "trigger": {"type": "manual"}, "targets": [],
                   "listeners": listeners})
        };
        let runs = vec![
            r(
                10,
                json!({"checking": false, "expected": ["Lobby", "Hall"],
                       "heard": [{"name": "lobby", "loss_percent": 1.0}, {"name": "Kitchen"}],
                       "silent": ["Hall"]}),
            ),
            r(
                20,
                json!({"checking": false, "expected": ["Lobby", "Hall"],
                       "heard": [{"name": "Lobby", "loss_percent": 3.0}, {"name": "Hall"}],
                       "silent": []}),
            ),
            // Couldn't check, or still checking: not counted.
            r(
                30,
                json!({"checking": false, "expected": ["Lobby"], "heard": [], "silent": [],
                       "error": "the mcast server's monitor is off"}),
            ),
            r(
                40,
                json!({"checking": true, "expected": ["Lobby"], "heard": [], "silent": []}),
            ),
            // No listeners expected.
            r(50, Value::Null),
        ];
        let m = summarize(&runs, &[], &|_| vec![]);
        let l = &m["listeners"];
        assert_eq!(l["checked"], 2);
        assert_eq!(l["with_silent"], 1);
        let list = l["list"].as_array().unwrap();
        let names: Vec<_> = list.iter().map(|e| e["name"].as_str().unwrap()).collect();
        // Most missed first.
        assert_eq!(names, ["Hall", "Kitchen", "Lobby"]);
        assert_eq!(list[0]["expected"], 2);
        assert_eq!(list[0]["heard"], 1);
        assert_eq!(list[0]["missed"], 1);
        assert_eq!(list[0]["last_heard_ms"], 20);
        assert_eq!(list[1]["expected"], 0);
        assert_eq!(list[1]["heard"], 1);
        assert_eq!(list[2]["heard"], 2);
        assert_eq!(list[2]["missed"], 0);
        assert_eq!(list[2]["worst_loss_percent"], 3.0);
    }

    #[test]
    fn summarizes_by_day_announcement_zone_and_schedule() {
        let r = |id: u64, ann: &str, started: u64, state: &str, trigger: Value, targets: Value| {
            json!({"id": id, "announcement": ann, "name": ann.to_uppercase(), "started_ms": started,
                   "state": state, "trigger": trigger, "targets": targets})
        };
        let t = |group: &str, state: &str, played: u64| json!({"group": group, "state": state, "played": played});
        let runs = vec![
            r(
                1,
                "bell",
                10,
                "done",
                json!({"type": "schedule", "at_ms": 9}),
                json!([t("g1", "done", 1), t("g2", "done", 1)]),
            ),
            r(
                2,
                "bell",
                20_000,
                "done",
                json!({"type": "schedule", "at_ms": 10_000}),
                json!([t("g1", "done", 1)]),
            ),
            r(
                3,
                "bell",
                30,
                "missed",
                json!({"type": "schedule", "at_ms": 30}),
                json!([]),
            ),
            r(
                4,
                "fire",
                110,
                "failed",
                json!({"type": "api", "by": "Doorbell"}),
                json!([t("g1", "busy", 0), t("old", "failed", 0)]),
            ),
        ];
        let days = vec![("d1".to_string(), 0, 100), ("d2".to_string(), 100, 100_000)];
        let zones_of = |g: &str| match g {
            "g1" => vec![("hall".to_string(), "Hall".to_string())],
            "g2" => vec![
                ("hall".to_string(), "Hall".to_string()),
                ("yard".to_string(), "Yard".to_string()),
            ],
            _ => vec![],
        };
        let m = summarize(&runs, &days, &zones_of);
        assert_eq!(m["totals"]["plays"], 4);
        assert_eq!(m["days"][0]["plays"], 2);
        assert_eq!(m["days"][1]["failed"], 1);
        assert_eq!(m["announcements"][0]["id"], "bell");
        assert_eq!(m["announcements"][0]["missed"], 1);
        let hall = m["zones"]
            .as_array()
            .unwrap()
            .iter()
            .find(|z| z["id"] == "hall")
            .unwrap()
            .clone();
        assert_eq!(
            hall,
            json!({"id": "hall", "name": "Hall", "sends": 4, "played": 3, "busy": 1, "failed": 0, "taken_over": 0})
        );
        assert!(m["zones"]
            .as_array()
            .unwrap()
            .iter()
            .any(|z| z["name"] == "old (in no zone now)"));
        assert_eq!(m["schedules"]["on_time"], 1);
        assert_eq!(m["schedules"]["late"], 1);
        assert_eq!(m["schedules"]["missed"], 1);
        assert_eq!(m["triggers"], json!({"api": 1, "schedule": 3}));
        assert_eq!(m["by"], json!([{"name": "Doorbell", "plays": 1}]));
    }

    #[test]
    fn old_days_go() {
        let d = dir();
        std::fs::create_dir_all(&d).unwrap();
        for day in ["2001-01-01", "2001-01-02"] {
            std::fs::write(d.join(format!("{day}.jsonl")), "").unwrap();
        }
        std::fs::write(d.join("notes.txt"), "kept").unwrap();
        let h = History::new(d.clone(), 30);
        h.observe(&Change::Run(&run(1, now_ms(), RunState::Done)));
        let mut left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.contains(&"notes.txt".to_string()));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn csv_quotes_and_defuses_formulas() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=cmd()"), "'=cmd()");
        let r = serde_json::to_value(run(3, 1_800_000_000_000, RunState::Done)).unwrap();
        let row = csv_row(&r, &jiff::tz::TimeZone::UTC);
        assert!(
            row.starts_with("2027-01-15 08:00:00,,a,A,done,manual,"),
            "{row}"
        );
        assert_eq!(row.split(',').count(), CSV_HEADER.split(',').count());
    }
}
