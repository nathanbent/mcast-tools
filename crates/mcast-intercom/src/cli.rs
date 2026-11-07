//! `mcast intercom ...`: list, check and play from the command line.

use crate::backend::{Backend, HttpBackend};
use crate::model::{Audio, Config, Day, Target, When, NO_CHIME};
use crate::runner::{Run, RunState, Runner, TargetState, Trigger};
use crate::scheduler;
use crate::store;
use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// How long to wait for senders to stop after Ctrl-C.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// How often to ask the server directly, in case an event was missed.
const RECONCILE_EVERY: Duration = Duration::from_secs(5);

/// Print the zones and announcements.
pub fn list(path: &Path) -> Result<()> {
    let cfg = store::load(path)?;
    if cfg.zones.is_empty() && cfg.announcements.is_empty() && cfg.schedules.is_empty() {
        println!("Nothing set up yet in {}.", path.display());
        return Ok(());
    }
    println!("Zones:");
    for z in &cfg.zones {
        let targets: Vec<String> = z
            .targets
            .iter()
            .map(|t| describe_target(&cfg.defaults.target(t)))
            .collect();
        println!("  {:<16} {}: {}", z.id, z.name, targets.join(", "));
    }
    println!("Announcements:");
    for a in &cfg.announcements {
        let audio = match &a.audio {
            Audio::Media(m) => m.clone(),
            Audio::ChimeOnly => "chime only".into(),
        };
        let mut extras = Vec::new();
        // As it plays, with the defaults: say which chimes are those.
        let played = a.with_defaults(&cfg.defaults);
        for (label, own, c) in [
            ("chime", &a.chime, &played.chime),
            ("end chime", &a.end_chime, &played.end_chime),
        ] {
            if let Some(c) = c {
                let from = if own.is_none() { " (default)" } else { "" };
                extras.push(format!("{label} {}{from}", chime_name(c)));
            }
        }
        if a.repeat > 1 {
            extras.push(format!("{} times", a.repeat));
        }
        if a.priority != Default::default() {
            extras.push(format!("{} priority", a.priority));
        }
        let extras = if extras.is_empty() {
            String::new()
        } else {
            format!(" ({})", extras.join(", "))
        };
        println!(
            "  {:<16} {}: {audio}{extras} -> {}",
            a.id,
            a.name,
            a.zones.join(", ")
        );
    }
    if !cfg.schedules.is_empty() {
        println!(
            "Schedules (times in {}){}:",
            cfg.tz_name(),
            if cfg.schedules_paused {
                ", all paused"
            } else {
                ""
            }
        );
        for sch in &cfg.schedules {
            let what = cfg
                .announcement(&sch.announcement)
                .map_or(sch.announcement.as_str(), |a| a.name.as_str());
            println!(
                "  {:<16} {what}: {}{}",
                sch.id,
                describe_when(&sch.when),
                if sch.enabled { "" } else { " (off)" }
            );
        }
    }
    if !cfg.days_off.is_empty() {
        println!("Days off (no schedules play):");
        for d in &cfg.days_off {
            let when = match &d.to {
                Some(to) if *to != d.from => format!("{} to {to}", d.from),
                _ => d.from.clone(),
            };
            println!(
                "  {when}{}",
                if d.name.is_empty() {
                    String::new()
                } else {
                    format!(": {}", d.name)
                }
            );
        }
    }
    Ok(())
}

/// Describe when a schedule plays: "Mon-Fri 08:30, 12:00" or "once, 2026-12-24 16:30".
pub fn describe_when(w: &When) -> String {
    match w {
        When::Once(at) => format!("once, {}", at.replace('T', " ")),
        When::Weekly { days, times } => {
            let all = [
                Day::Mon,
                Day::Tue,
                Day::Wed,
                Day::Thu,
                Day::Fri,
                Day::Sat,
                Day::Sun,
            ];
            let name = |d: &Day| {
                serde_json::to_value(d)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default()
            };
            let on = |d: &Day| days.contains(d);
            let days = if all.iter().all(on) {
                "every day".to_string()
            } else if all[..5].iter().all(on) && !all[5..].iter().any(on) {
                "Mon-Fri".to_string()
            } else {
                all.iter()
                    .filter(|d| on(d))
                    .map(|d| {
                        let n = name(d);
                        n[..1].to_uppercase() + &n[1..]
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("{days} {}", times.join(", "))
        }
    }
}

/// The next times anything is scheduled to play.
pub fn upcoming(path: &Path, n: usize) -> Result<()> {
    let cfg = store::load(path)?;
    let tz = cfg.tz()?;
    let fires = scheduler::upcoming(&cfg, &tz, jiff::Timestamp::now(), n);
    if fires.is_empty() {
        println!("Nothing scheduled.");
        return Ok(());
    }
    println!("Times in {}:", cfg.tz_name());
    for f in fires {
        println!("  {}  {} ({})", f.when(), f.name, f.schedule);
    }
    Ok(())
}

fn describe_target(t: &Target) -> String {
    let mut opts = Vec::new();
    if let Some(e) = &t.encoding {
        opts.push(e.clone());
    }
    if let Some(v) = t.volume_db {
        opts.push(format!("{v:+} dB"));
    }
    if opts.is_empty() {
        t.group.clone()
    } else {
        format!("{} ({})", t.group, opts.join(", "))
    }
}

fn chime_name(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        _ => c["file"].as_str().unwrap_or("?").to_string(),
    }
}

/// Check the config, and (if the server can be reached) that the media
/// files and phonebook names it uses exist there.
pub fn check(path: &Path, backend: &HttpBackend) -> Result<()> {
    let cfg = store::load(path)?;
    println!(
        "{}: {} zones, {} announcements",
        path.display(),
        cfg.zones.len(),
        cfg.announcements.len()
    );
    let problems = match server_problems(&cfg, backend) {
        Ok(p) => p,
        Err(e) if e.status == 0 => {
            println!("Couldn't check media files and phonebook names: {e}");
            return Ok(());
        }
        Err(e) => bail!("asking {}: {e}", backend.base()),
    };
    for p in &problems {
        println!("  {p}");
    }
    if !problems.is_empty() {
        bail!("{} problem(s) on {}", problems.len(), backend.base());
    }
    println!(
        "Media files, chimes and phonebook names all found on {}.",
        backend.base()
    );
    Ok(())
}

fn server_problems(
    cfg: &Config,
    backend: &dyn Backend,
) -> Result<Vec<String>, crate::BackendError> {
    let media = backend.media()?;
    let book = backend.phonebook()?;
    // An older server has no chime list: then only it can say.
    let chimes: Option<Vec<String>> = backend.chimes().ok().map(|list| {
        list.iter()
            .filter_map(|c| c["name"].as_str().map(str::to_string))
            .collect()
    });
    let mut problems = Vec::new();
    for z in &cfg.zones {
        for t in &z.targets {
            let g = t.group.trim();
            let known = g.parse::<std::net::SocketAddr>().is_ok()
                || book.iter().any(|e| e.name.eq_ignore_ascii_case(g));
            if !known {
                problems.push(format!("zone \"{}\": \"{g}\" isn't in the phonebook", z.id));
            }
        }
    }
    // The defaults, then each announcement's own (the defaults it uses are
    // checked once, above).
    let d = &cfg.defaults;
    let mut uses = vec![("defaults".to_string(), None, [&d.chime, &d.end_chime])];
    for a in &cfg.announcements {
        let what = format!("announcement \"{}\"", a.id);
        let media = match &a.audio {
            Audio::Media(m) => Some(m.as_str()),
            Audio::ChimeOnly => None,
        };
        uses.push((what, media, [&a.chime, &a.end_chime]));
    }
    for (what, audio, chime_values) in uses {
        let mut files: Vec<&str> = audio.into_iter().collect();
        for c in chime_values.into_iter().flatten() {
            if let Some(f) = c["file"].as_str() {
                files.push(f);
            }
            if let (Some(name), Some(chimes)) = (c.as_str(), &chimes) {
                let special = name == "default" || name == NO_CHIME;
                if !special && !chimes.iter().any(|k| k == name) {
                    problems.push(format!("{what}: no built-in chime \"{name}\""));
                }
            }
        }
        for f in files {
            if !media.iter().any(|m| m == f) {
                problems.push(format!("{what}: no media file \"{f}\""));
            }
        }
    }
    Ok(problems)
}

/// Play an announcement and follow it to the end. Setting `stop` (Ctrl-C)
/// stops it. An error if it played to no group.
pub fn play(path: &Path, backend: Arc<dyn Backend>, which: &str, stop: &AtomicBool) -> Result<()> {
    let cfg = store::load(path)?;
    // Notifications can arrive out of order, so they only say when to look.
    let (tx, rx) = mpsc::channel::<()>();
    let runner = Runner::new(backend, move |_| {
        let _ = tx.send(());
    });
    let run = runner.play(&cfg, which, Trigger::Manual)?;
    println!(
        "Playing \"{}\" to {} group{}{}",
        run.name,
        run.targets.len(),
        if run.targets.len() == 1 { "" } else { "s" },
        if run.rounds > 1 {
            format!(", {} times", run.rounds)
        } else {
            String::new()
        }
    );
    let mut shown = Shown::default();
    shown.print(&run);
    let mut stopping: Option<Instant> = None;
    let mut reconciled = Instant::now();
    let last = loop {
        let _ = rx.recv_timeout(Duration::from_millis(200));
        let now = runner.get(run.id).unwrap_or_else(|| run.clone());
        shown.print(&now);
        if now.state.is_over() {
            break now;
        }
        if stop.load(Ordering::Relaxed) && stopping.is_none() {
            println!("Stopping...");
            stopping = Some(Instant::now());
            let r = runner.clone();
            std::thread::spawn(move || r.stop(run.id));
        }
        if stopping.is_some_and(|t| t.elapsed() > STOP_WAIT) {
            bail!("gave up waiting for the senders to stop");
        }
        if reconciled.elapsed() > RECONCILE_EVERY {
            reconciled = Instant::now();
            runner.reconcile();
        }
    };
    match last.state {
        RunState::Done => {
            let played = last.targets.iter().filter(|t| t.played > 0).count();
            println!("Done: played to {played} of {} groups.", last.targets.len());
            Ok(())
        }
        RunState::Stopped => {
            println!("Stopped.");
            Ok(())
        }
        _ => bail!(last.error.unwrap_or_else(|| "failed".into())),
    }
}

/// Prints each group's state when it changes.
#[derive(Default)]
struct Shown {
    round: u32,
    targets: HashMap<usize, (TargetState, Option<u64>)>,
}

impl Shown {
    fn print(&mut self, run: &Run) {
        if run.rounds > 1 && run.round != self.round && run.state == RunState::Playing {
            self.round = run.round;
            self.targets.clear();
            println!("Round {} of {}", run.round, run.rounds);
        }
        for (i, t) in run.targets.iter().enumerate() {
            let now = (t.state, t.job);
            if self.targets.get(&i) == Some(&now) {
                continue;
            }
            self.targets.insert(i, now);
            let name = match &t.address {
                Some(a) if *a != t.group => format!("{} ({a})", t.group),
                _ => t.group.clone(),
            };
            let state = match t.state {
                TargetState::Starting => "starting".to_string(),
                TargetState::Playing => match t.job {
                    Some(j) => format!("playing (job {j})"),
                    None => "playing".into(),
                },
                TargetState::Done => "done".into(),
                TargetState::Busy => "busy".into(),
                TargetState::Preempted => "taken over".into(),
                TargetState::Stopped => "stopped".into(),
                TargetState::Failed => "failed".into(),
            };
            match &t.error {
                Some(e) => println!("  {name}: {state}: {e}"),
                None => println!("  {name}: {state}"),
            }
        }
    }
}
