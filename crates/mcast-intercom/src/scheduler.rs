//! Schedules: announcements that play by themselves, at wall-clock times in
//! the config's time zone.
//!
//! Daylight saving: a time that doesn't exist that day (the clocks went
//! forward over it) plays at the first moment after the gap, as if the
//! clocks hadn't changed yet; a time that happens twice (they went back)
//! plays once, the first time.
//!
//! A time is played if the panel gets to it within [`GRACE`] (it was busy,
//! or just started); later than that it is recorded as missed, so an outage
//! doesn't end in a burst of stale announcements. On starting, the panel
//! looks back [`LOOK_BACK`] for times it missed while it wasn't running.

use crate::model::{parse_datetime, parse_time, Config, Schedule, When};
use crate::runner::{RunState, Runner, Trigger};
use crate::store;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How late a schedule may still play.
pub const GRACE: Duration = Duration::from_secs(60);
/// How far back to look for missed times on starting.
pub const LOOK_BACK: Duration = Duration::from_secs(3600);
/// Look at the config at least this often, to see edits made by hand.
const MAX_SLEEP: Duration = Duration::from_secs(30);
/// Most times due for one schedule in one look (after a long outage).
const MAX_DUE: usize = 1000;

/// The next time a schedule is due, strictly after `after` (whether or not
/// it's enabled).
pub fn next_after(s: &Schedule, tz: &TimeZone, after: Timestamp) -> Option<Timestamp> {
    let at = |dt: jiff::civil::DateTime| {
        tz.to_ambiguous_zoned(dt)
            .compatible()
            .ok()
            .map(|z| z.timestamp())
    };
    match &s.when {
        When::Once(when) => at(parse_datetime(when).ok()?).filter(|t| *t > after),
        When::Weekly { days, times } => {
            let times: Vec<_> = times.iter().filter_map(|t| parse_time(t).ok()).collect();
            let mut date = after.to_zoned(tz.clone()).date();
            // A week and a day: today's times may all have passed.
            for _ in 0..=8 {
                if days.iter().any(|d| d.weekday() == date.weekday()) {
                    let best = times
                        .iter()
                        .filter_map(|t| at(date.to_datetime(*t)))
                        .filter(|t| *t > after)
                        .min();
                    if best.is_some() {
                        return best;
                    }
                }
                date = date.tomorrow().ok()?;
            }
            None
        }
    }
}

/// The next time a schedule plays, strictly after `after`: as
/// [`next_after`] in its site's time zone, passing over its site's days
/// off (whether or not it's on).
pub fn next_playing(cfg: &Config, s: &Schedule, after: Timestamp) -> Option<Timestamp> {
    let tz = &cfg.schedule_tz(s);
    let site = s.site.as_deref();
    let mut after = after;
    // Each turn passes a whole run of days off, so this is plenty.
    for _ in 0..MAX_DUE {
        let t = next_after(s, tz, after)?;
        let Some(last) = cfg.days_off_until(site, t.to_zoned(tz.clone()).date()) else {
            return Some(t);
        };
        // Look again from just before the first day back starts.
        let back = last
            .tomorrow()
            .ok()?
            .to_datetime(jiff::civil::Time::midnight());
        let start = tz.to_ambiguous_zoned(back).compatible().ok()?.timestamp();
        after = start
            .checked_sub(jiff::SignedDuration::from_nanos(1))
            .ok()?;
    }
    None
}

/// The schedules that play: those that are on, in groups and sites that
/// are on, unless everything is paused.
fn playing(cfg: &Config) -> impl Iterator<Item = &Schedule> {
    cfg.schedules.iter().filter(move |s| cfg.schedule_on(s))
}

/// Playing schedules' times in `(from, to]`, earliest first, leaving out
/// days off.
pub fn due(cfg: &Config, from: Timestamp, to: Timestamp) -> Vec<(&Schedule, Timestamp)> {
    let mut out = Vec::new();
    for s in playing(cfg) {
        let mut t = from;
        for _ in 0..MAX_DUE {
            match next_playing(cfg, s, t) {
                Some(next) if next <= to => {
                    out.push((s, next));
                    t = next;
                }
                _ => break,
            }
        }
    }
    out.sort_by_key(|(_, t)| *t);
    out
}

/// A time a schedule will play.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fire {
    pub at_ms: u64,
    /// Wall-clock time in the schedule's time zone: "2026-11-09T12:00:00".
    pub local: String,
    /// "Mon".
    pub weekday: String,
    pub schedule: String,
    pub announcement: String,
    /// The announcement's name.
    pub name: String,
    /// The schedule's site, if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

impl Fire {
    fn new(cfg: &Config, s: &Schedule, at: Timestamp) -> Fire {
        let z = at.to_zoned(cfg.schedule_tz(s));
        Fire {
            at_ms: at.as_millisecond().max(0) as u64,
            local: z.datetime().to_string(),
            weekday: z.strftime("%a").to_string(),
            schedule: s.id.clone(),
            announcement: s.announcement.clone(),
            name: cfg
                .announcement(&s.announcement)
                .map_or_else(|| s.announcement.clone(), |a| a.name.clone()),
            site: s.site.clone(),
        }
    }

    /// "Mon 2026-11-09 12:00", for messages.
    pub fn when(&self) -> String {
        let hhmm = self
            .local
            .get(..16)
            .unwrap_or(&self.local)
            .replace('T', " ");
        format!("{} {hhmm}", self.weekday)
    }
}

/// The next `n` times anything is scheduled to play, after `after`
/// (leaving out days off, and schedules that are off or paused).
pub fn upcoming(cfg: &Config, after: Timestamp, n: usize) -> Vec<Fire> {
    let mut all = Vec::new();
    for s in playing(cfg) {
        let mut t = after;
        for _ in 0..n {
            match next_playing(cfg, s, t) {
                Some(next) => {
                    all.push((s, next));
                    t = next;
                }
                None => break,
            }
        }
    }
    all.sort_by_key(|(_, t)| *t);
    all.truncate(n);
    all.into_iter().map(|(s, t)| Fire::new(cfg, s, t)).collect()
}

/// Plays schedules as they come due, in a thread of its own, reading the
/// config file each time it looks. Stops when dropped.
pub struct Scheduler(Arc<Shared>);

struct Shared {
    path: PathBuf,
    runner: Runner,
    grace: Duration,
    poked: Mutex<bool>,
    wake: Condvar,
    stop: AtomicBool,
}

impl Scheduler {
    /// Start looking. Times in the last `grace` before now play straight
    /// away; those in the [`LOOK_BACK`] before that are recorded as missed.
    pub fn start(path: PathBuf, runner: Runner, grace: Duration) -> Self {
        let shared = Arc::new(Shared {
            path,
            runner,
            grace,
            poked: Mutex::new(false),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let me = shared.clone();
        std::thread::spawn(move || me.run());
        Scheduler(shared)
    }

    /// Look again now: the config changed.
    pub fn poke(&self) {
        *self.0.poked.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0.wake.notify_all();
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
        self.poke();
    }
}

impl Shared {
    fn run(&self) {
        let grace_ms = self.grace.as_millis() as i64;
        let back_ms = grace_ms.max(LOOK_BACK.as_millis() as i64);
        let mut mark = Timestamp::from_millisecond(Timestamp::now().as_millisecond() - back_ms)
            .unwrap_or_else(|_| Timestamp::now());
        let mut problem: Option<String> = None;
        while !self.stop.load(Ordering::Relaxed) {
            let now = Timestamp::now();
            // The clock went back: don't play anything twice.
            if now < mark {
                mark = now;
            }
            let mut sleep = MAX_SLEEP;
            match store::load(&self.path).and_then(|c| c.tz().map(|_| c)) {
                Err(e) => {
                    let e = format!("{e:#}");
                    if problem.as_ref() != Some(&e) {
                        eprintln!("schedules paused: {e}");
                        problem = Some(e);
                    }
                    // While it can't be read, times pass unplayed and are
                    // recorded as missed once it can.
                }
                Ok(cfg) => {
                    if problem.take().is_some() {
                        eprintln!("schedules running again");
                    }
                    self.play_due(&cfg, mark, now, grace_ms);
                    mark = now;
                    let next = playing(&cfg)
                        .filter_map(|s| next_playing(&cfg, s, now))
                        .min();
                    if let Some(next) = next {
                        let ms = (next.as_millisecond() - now.as_millisecond()).max(0) as u64;
                        sleep = sleep.min(Duration::from_millis(ms));
                    }
                }
            }
            let mut poked = self.poked.lock().unwrap_or_else(|e| e.into_inner());
            if !*poked {
                poked = self
                    .wake
                    .wait_timeout(poked, sleep)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            *poked = false;
        }
    }

    fn play_due(&self, cfg: &Config, from: Timestamp, now: Timestamp, grace_ms: i64) {
        // Only the latest missed time of each schedule is worth recording.
        let mut missed: HashMap<&str, (&Schedule, Timestamp)> = HashMap::new();
        for (s, at) in due(cfg, from, now) {
            if now.as_millisecond() - at.as_millisecond() <= grace_ms {
                self.play(cfg, s, at);
            } else {
                missed.insert(s.id.as_str(), (s, at));
            }
        }
        for (s, at) in missed.into_values() {
            let Some(a) = cfg.announcement(&s.announcement) else {
                continue;
            };
            let f = Fire::new(cfg, s, at);
            self.runner.record(
                a,
                Trigger::Schedule {
                    id: s.id.clone(),
                    at_ms: f.at_ms,
                },
                RunState::Missed,
                format!(
                    "missed: due {} but the panel couldn't play it then",
                    f.when()
                ),
            );
        }
    }

    fn play(&self, cfg: &Config, s: &Schedule, at: Timestamp) {
        let trigger = Trigger::Schedule {
            id: s.id.clone(),
            at_ms: Fire::new(cfg, s, at).at_ms,
        };
        let (runner, cfg, id) = (self.runner.clone(), cfg.clone(), s.announcement.clone());
        let site = s.site.clone();
        // Starting can take a moment; other schedules due now shouldn't wait.
        std::thread::spawn(move || {
            if let Err(e) = runner.play_at(&cfg, &id, trigger.clone(), site.as_deref()) {
                if let Some(a) = cfg.announcement(&id) {
                    runner.record(a, trigger, RunState::Failed, e.to_string());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::Fake;
    use crate::model::{Announcement, Audio, Day, Priority, Target, Zone};
    use crate::runner::Run;

    fn london() -> TimeZone {
        TimeZone::get("Europe/London").unwrap()
    }

    /// A London wall-clock time as an instant.
    fn local(s: &str) -> Timestamp {
        london()
            .to_ambiguous_zoned(s.parse().unwrap())
            .compatible()
            .unwrap()
            .timestamp()
    }

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn weekly(days: &[Day], times: &[&str]) -> Schedule {
        Schedule {
            id: "s".into(),
            name: String::new(),
            announcement: "a".into(),
            enabled: true,
            site: None,
            group: None,
            when: When::Weekly {
                days: days.to_vec(),
                times: times.iter().map(|t| t.to_string()).collect(),
            },
        }
    }

    fn once(at: &str) -> Schedule {
        Schedule {
            when: When::Once(at.into()),
            ..weekly(&[Day::Mon], &["00:00"])
        }
    }

    #[test]
    fn weekly_times_come_round() {
        let s = weekly(&[Day::Mon, Day::Fri], &["12:00", "08:30"]);
        let tz = london();
        // 2026-11-09 is a Monday.
        let next = |after: &str| next_after(&s, &tz, local(after)).unwrap();
        assert_eq!(next("2026-11-09T09:00"), local("2026-11-09T12:00"));
        assert_eq!(next("2026-11-09T07:00"), local("2026-11-09T08:30"));
        // Strictly after: the time itself has had its turn.
        assert_eq!(next("2026-11-09T12:00"), local("2026-11-13T08:30"));
        assert_eq!(next("2026-11-13T13:00"), local("2026-11-16T08:30"));
    }

    #[test]
    fn a_time_skipped_by_the_clocks_plays_just_after() {
        // 2026-03-29 01:00 GMT the clocks went to 02:00 BST: 01:30 never happens.
        let s = weekly(&[Day::Sun], &["01:30"]);
        let got = next_after(&s, &london(), local("2026-03-29T00:00")).unwrap();
        assert_eq!(got, ts("2026-03-29T01:30:00Z"), "02:30 BST");
    }

    #[test]
    fn a_time_that_happens_twice_plays_once() {
        // 2026-10-25 02:00 BST the clocks went back to 01:00 GMT: 01:30 happens twice.
        let s = weekly(&[Day::Sun], &["01:30"]);
        let tz = london();
        let first = next_after(&s, &tz, local("2026-10-25T00:00")).unwrap();
        assert_eq!(first, ts("2026-10-25T00:30:00Z"), "the first, in BST");
        let after = next_after(&s, &tz, first).unwrap();
        assert_eq!(
            after,
            ts("2026-11-01T01:30:00Z"),
            "then next week, not an hour later"
        );
    }

    #[test]
    fn once_plays_once() {
        let s = once("2026-12-24T16:30");
        let tz = london();
        assert_eq!(
            next_after(&s, &tz, local("2026-12-01T00:00")),
            Some(local("2026-12-24T16:30"))
        );
        assert_eq!(next_after(&s, &tz, local("2026-12-24T16:30")), None);
    }

    fn config(schedules: Vec<Schedule>) -> Config {
        Config {
            zones: vec![Zone {
                id: "z".into(),
                name: "Z".into(),
                targets: vec![Target {
                    group: "239.1.1.1:5000".into(),
                    ..Default::default()
                }],
                site: None,
            }],
            announcements: ["a", "b", "c", "d"]
                .iter()
                .map(|id| Announcement {
                    id: id.to_string(),
                    name: format!("{id} name"),
                    audio: Audio::ChimeOnly,
                    chime: None,
                    end_chime: None,
                    mode: None,
                    volume_db: None,
                    lead_in_ms: None,
                    repeat: 1,
                    repeat_gap_s: 0.0,
                    zones: vec!["z".into()],
                    priority: Priority::Normal,
                    parts: Vec::new(),
                    category: None,
                    tags: Vec::new(),
                })
                .collect(),
            schedules,
            time_zone: Some("Europe/London".into()),
            ..Default::default()
        }
    }

    #[test]
    fn lists_what_is_due_and_coming_up() {
        let mut every_day = weekly(
            &[
                Day::Mon,
                Day::Tue,
                Day::Wed,
                Day::Thu,
                Day::Fri,
                Day::Sat,
                Day::Sun,
            ],
            &["09:00", "12:00", "15:00"],
        );
        every_day.id = "daily".into();
        let mut xmas = once("2026-11-10T10:00");
        xmas.id = "once".into();
        xmas.announcement = "b".into();
        let mut off = weekly(&[Day::Mon], &["09:30"]);
        off.id = "off".into();
        off.enabled = false;
        let cfg = config(vec![every_day, xmas, off]);
        cfg.validate().unwrap();

        let got: Vec<_> = due(&cfg, local("2026-11-09T09:00"), local("2026-11-10T12:00"))
            .into_iter()
            .map(|(s, t)| (s.id.as_str(), t))
            .collect();
        assert_eq!(
            got,
            [
                ("daily", local("2026-11-09T12:00")),
                ("daily", local("2026-11-09T15:00")),
                ("daily", local("2026-11-10T09:00")),
                ("once", local("2026-11-10T10:00")),
                ("daily", local("2026-11-10T12:00")),
            ]
        );

        let next = upcoming(&cfg, local("2026-11-10T09:30"), 3);
        let when: Vec<_> = next.iter().map(Fire::when).collect();
        assert_eq!(
            when,
            [
                "Tue 2026-11-10 10:00",
                "Tue 2026-11-10 12:00",
                "Tue 2026-11-10 15:00"
            ]
        );
        assert_eq!(next[0].name, "b name");
        assert_eq!(next[0].schedule, "once");
    }

    #[test]
    fn days_off_and_pausing() {
        let all = [
            Day::Mon,
            Day::Tue,
            Day::Wed,
            Day::Thu,
            Day::Fri,
            Day::Sat,
            Day::Sun,
        ];
        let mut cfg = config(vec![weekly(&all, &["09:00"])]);
        let off = |from: &str, to: Option<&str>| crate::model::DayOff {
            name: String::new(),
            from: from.into(),
            to: to.map(Into::into),
        };
        // One day, then a run made of two that touch.
        cfg.days_off = vec![
            off("2026-11-10", None),
            off("2026-11-12", Some("2026-11-13")),
            off("2026-11-14", None),
        ];
        cfg.validate().unwrap();
        let got: Vec<_> = due(&cfg, local("2026-11-09T08:00"), local("2026-11-16T10:00"))
            .into_iter()
            .map(|(_, t)| t)
            .collect();
        assert_eq!(
            got,
            [
                local("2026-11-09T09:00"),
                local("2026-11-11T09:00"),
                local("2026-11-15T09:00"),
                local("2026-11-16T09:00"),
            ]
        );
        let s = &cfg.schedules[0];
        assert_eq!(
            next_playing(&cfg, s, local("2026-11-11T10:00")),
            Some(local("2026-11-15T09:00"))
        );

        // Paused: nothing is due or coming up, but each still knows its next time.
        cfg.schedules_paused = true;
        let (from, to) = (local("2026-11-09T08:00"), local("2026-11-16T10:00"));
        assert!(due(&cfg, from, to).is_empty());
        assert!(upcoming(&cfg, from, 5).is_empty());
        assert_eq!(
            next_playing(&cfg, &cfg.schedules[0], from),
            Some(local("2026-11-09T09:00"))
        );

        // Mistakes.
        cfg.days_off = vec![off("2026-11-14", Some("2026-11-12"))];
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("ends before it starts"), "{e}");
        cfg.days_off = vec![off("14/11/2026", None)];
        let e = cfg.validate().unwrap_err().to_string();
        assert!(e.contains("isn't a date"), "{e}");
    }

    #[test]
    fn sites_have_their_own_time_zones_days_off_and_switches() {
        use crate::model::{DayOff, ScheduleGroup, Site};
        let mut cfg = config(Vec::new());
        // Zone z is at London; zone ny at New York. Announcement "a" goes to both.
        cfg.zones[0].site = Some("ldn".into());
        cfg.zones.push(Zone {
            id: "ny".into(),
            name: "NY".into(),
            targets: vec![Target {
                group: "239.1.1.2:5000".into(),
                ..Default::default()
            }],
            site: Some("nyc".into()),
        });
        cfg.announcements[0].zones.push("ny".into());
        cfg.sites = vec![
            Site {
                id: "ldn".into(),
                name: "London".into(),
                time_zone: None,
                days_off: Vec::new(),
                paused: false,
            },
            Site {
                id: "nyc".into(),
                name: "New York".into(),
                time_zone: Some("America/New_York".into()),
                days_off: vec![DayOff {
                    name: "Thanksgiving".into(),
                    from: "2026-11-26".into(),
                    to: None,
                }],
                paused: false,
            },
        ];
        cfg.schedule_groups = vec![ScheduleGroup {
            id: "exams".into(),
            name: "Exam week".into(),
            enabled: false,
        }];
        let at = |id: &str, site: &str, group: Option<&str>| Schedule {
            id: id.into(),
            site: Some(site.into()),
            group: group.map(Into::into),
            ..weekly(&[Day::Wed, Day::Thu], &["09:00"])
        };
        cfg.schedules = vec![
            at("ldn-bell", "ldn", None),
            at("nyc-bell", "nyc", None),
            at("exam-bell", "ldn", Some("exams")),
        ];
        cfg.validate().unwrap();
        let ny = TimeZone::get("America/New_York").unwrap();
        let ny_local = |s: &str| {
            ny.to_ambiguous_zoned(s.parse().unwrap())
                .compatible()
                .unwrap()
                .timestamp()
        };
        let due_ids = |cfg: &Config| -> Vec<(String, Timestamp)> {
            due(cfg, ts("2026-11-25T00:00:00Z"), ts("2026-11-27T00:00:00Z"))
                .into_iter()
                .map(|(s, t)| (s.id.clone(), t))
                .collect()
        };
        // 09:00 at each site in its own time zone; New York's day off is its
        // own; the exam group is off.
        assert_eq!(
            due_ids(&cfg),
            [
                ("ldn-bell".to_string(), local("2026-11-25T09:00")),
                ("nyc-bell".to_string(), ny_local("2026-11-25T09:00")),
                ("ldn-bell".to_string(), local("2026-11-26T09:00")),
            ]
        );
        let fires = upcoming(&cfg, ts("2026-11-25T00:00:00Z"), 2);
        assert_eq!(fires[1].site.as_deref(), Some("nyc"));
        assert_eq!(fires[1].local, "2026-11-25T09:00:00");

        // The group on: exam bells too. A paused site: none of its own.
        cfg.schedule_groups[0].enabled = true;
        cfg.sites[1].paused = true;
        let ids: Vec<String> = due_ids(&cfg).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["ldn-bell", "exam-bell", "ldn-bell", "exam-bell"]);

        // A site's schedule plays only to that site's zones.
        let fake = std::sync::Arc::new(Fake::default());
        let r = Runner::new(fake.clone(), |_| {});
        r.play_at(&cfg, "a", Trigger::Manual { by: None }, Some("nyc"))
            .unwrap();
        let groups: Vec<_> = fake.requests().iter().map(|q| q["group"].clone()).collect();
        assert_eq!(groups, [serde_json::json!("239.1.1.2:5000")]);
        fake.finish_all();

        // A site's schedule must have somewhere to play there.
        let mut bad = cfg.clone();
        bad.schedules[1].announcement = "b".into();
        let e = bad.validate().unwrap_err().to_string();
        assert!(e.contains("has no zones at New York"), "{e}");
        let mut bad = cfg.clone();
        bad.schedules[0].group = Some("nope".into());
        let e = bad.validate().unwrap_err().to_string();
        assert!(e.contains("no schedule group \"nope\""), "{e}");
    }

    #[test]
    fn validates_schedules() {
        let bad = |s: Schedule, tz: Option<&str>, msg: &str| {
            let mut c = config(vec![s]);
            c.time_zone = tz.map(Into::into);
            let e = c.validate().unwrap_err().to_string();
            assert!(e.contains(msg), "{e:?} should mention {msg:?}");
        };
        bad(weekly(&[], &["09:00"]), None, "has no days");
        bad(
            weekly(&[Day::Mon, Day::Mon], &["09:00"]),
            None,
            "a day twice",
        );
        bad(weekly(&[Day::Mon], &[]), None, "has no times");
        bad(weekly(&[Day::Mon], &["9am"]), None, "isn't a time of day");
        bad(weekly(&[Day::Mon], &["25:00"]), None, "isn't a time of day");
        bad(once("2026-12-24"), None, "isn't a date and time");
        bad(
            weekly(&[Day::Mon], &["09:00"]),
            Some("Mars/Olympus"),
            "unknown time zone",
        );
        let mut s = weekly(&[Day::Mon], &["09:00"]);
        s.announcement = "nope".into();
        bad(s, None, "no announcement \"nope\"");
        config(vec![weekly(&[Day::Mon], &["09:00:30"])])
            .validate()
            .unwrap();
    }

    /// `secs` from now, as a UTC wall-clock time with seconds.
    fn utc_in(secs: i64) -> String {
        let t = Timestamp::from_second(Timestamp::now().as_second() + secs).unwrap();
        t.to_zoned(TimeZone::UTC)
            .strftime("%Y-%m-%dT%H:%M:%S")
            .to_string()
    }

    fn wait_for(r: &Runner, what: &str, done: impl Fn(&[Run]) -> bool) -> Vec<Run> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let runs = r.list();
            if done(&runs) {
                return runs;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}: {runs:#?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn plays_on_time_catches_up_and_records_misses() {
        let dir = std::env::temp_dir().join(format!("mcast-intercom-sched-{}", std::process::id()));
        let path = dir.join("intercom.json");
        let sched = |id: &str, ann: &str, at: String| Schedule {
            id: id.into(),
            announcement: ann.into(),
            ..once(&at)
        };
        let mut cfg = config(vec![
            sched("soon", "a", utc_in(2)),
            sched("just-gone", "b", utc_in(-10)),
            sched("long-gone", "c", utc_in(-300)),
        ]);
        cfg.time_zone = Some("UTC".into());
        // Each to its own group, so they don't find each other busy.
        for (i, a) in cfg.announcements.iter_mut().enumerate() {
            let z = format!("z{i}");
            a.zones = vec![z.clone()];
            cfg.zones.push(Zone {
                site: None,
                id: z.clone(),
                name: z,
                targets: vec![Target {
                    group: format!("239.1.1.{}:5000", 10 + i),
                    ..Default::default()
                }],
            });
        }
        store::save(&path, &cfg).unwrap();

        let fake = Arc::new(Fake::default());
        let runner = Runner::new(fake.clone(), |_| {});
        let sch = Scheduler::start(path.clone(), runner.clone(), GRACE);

        // Within the grace period at start: plays now. Long before: missed.
        let runs = wait_for(&runner, "the catch-up", |r| r.len() >= 2);
        let by = |ann: &str| runs.iter().find(|r| r.announcement == ann).cloned();
        assert_eq!(by("b").unwrap().state, RunState::Playing);
        let missed = by("c").unwrap();
        assert_eq!(missed.state, RunState::Missed);
        assert!(missed.error.unwrap().starts_with("missed: due "));
        assert!(matches!(missed.trigger, Trigger::Schedule { ref id, .. } if id == "long-gone"));
        assert!(by("a").is_none(), "not yet");

        // On time.
        let runs = wait_for(&runner, "the next one", |r| {
            r.iter().any(|r| r.announcement == "a")
        });
        let a = runs.iter().find(|r| r.announcement == "a").unwrap();
        let Trigger::Schedule { at_ms, .. } = a.trigger else {
            panic!("{a:?}")
        };
        let late = a.started_ms as i64 - at_ms as i64;
        assert!(
            (0..1500).contains(&late),
            "started {late} ms after its time"
        );

        // A schedule added while running plays when poked, not 30 s later;
        // with the server away it's recorded as failed.
        fake.st.lock().unwrap().down = true;
        let d_zone = cfg.zones.iter_mut().find(|z| z.id == "z3").unwrap();
        d_zone.targets[0].group = "zone-1".into();
        cfg.schedules.push(sched("added", "d", utc_in(1)));
        store::save(&path, &cfg).unwrap();
        sch.poke();
        let runs = wait_for(&runner, "the added one", |r| {
            r.iter().any(|r| r.announcement == "d")
        });
        let d = runs.iter().find(|r| r.announcement == "d").unwrap();
        assert_eq!(d.state, RunState::Failed);
        assert!(d.error.as_ref().unwrap().contains("phonebook"), "{d:?}");

        drop(sch);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
