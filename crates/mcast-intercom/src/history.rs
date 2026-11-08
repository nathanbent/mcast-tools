//! The play history: every run that ended, kept on disk so it outlives a
//! restart. One file a day (`history/2026-11-08.jsonl`, by UTC date, beside
//! `intercom.json`), one JSON line per run: `{"run": RUN, "logged_ms"}`.
//! Days older than the config's `history_days` are deleted (0 keeps them all).
//!
//! The runner tells [`History::observe`] about every change; a run is written
//! once, when it's over (missed schedules included).

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

    /// Hear about a change from the runner: write a run once it's over.
    pub fn observe(&self, c: &Change<'_>) {
        if let Change::Run(run) = c {
            if run.state.is_over() {
                self.write(run);
            }
        }
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

    /// Delete days older than the ones kept.
    fn prune(&self, today: jiff::civil::Date) {
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
                    q.from_ms.map_or(true, |f| started >= f)
                        && q.to_ms.map_or(true, |t| started < t)
                        && q.before.map_or(true, |b| key(r) < b)
                        && q.announcement
                            .as_ref()
                            .map_or(true, |a| s("announcement") == *a)
                        && q.state.as_ref().map_or(true, |st| s("state") == *st)
                        && by.as_ref().map_or(true, |b| {
                            r["trigger"]["by"]
                                .as_str()
                                .is_some_and(|x| x.to_lowercase().contains(b))
                        })
                        && text
                            .as_ref()
                            .map_or(true, |t| s("name").to_lowercase().contains(t))
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
