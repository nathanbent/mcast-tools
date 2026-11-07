//! `mcast intercom ...`: list, check and play from the command line.

use crate::backend::{Backend, HttpBackend};
use crate::model::{Audio, Config, Target};
use crate::runner::{Run, RunState, Runner, TargetState, Trigger};
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
    if cfg.zones.is_empty() && cfg.announcements.is_empty() {
        println!("Nothing set up yet in {}.", path.display());
        return Ok(());
    }
    println!("Zones:");
    for z in &cfg.zones {
        let targets: Vec<String> = z.targets.iter().map(describe_target).collect();
        println!("  {:<16} {}: {}", z.id, z.name, targets.join(", "));
    }
    println!("Announcements:");
    for a in &cfg.announcements {
        let audio = match &a.audio {
            Audio::Media(m) => m.clone(),
            Audio::ChimeOnly => "chime only".into(),
        };
        let mut extras = Vec::new();
        if let Some(c) = &a.chime {
            extras.push(format!("chime {}", chime_name(c)));
        }
        if let Some(c) = &a.end_chime {
            extras.push(format!("end chime {}", chime_name(c)));
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
        "Media files and phonebook names all found on {}.",
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
    for a in &cfg.announcements {
        let mut files = Vec::new();
        if let Audio::Media(m) = &a.audio {
            files.push(m.as_str());
        }
        for c in [&a.chime, &a.end_chime].into_iter().flatten() {
            if let Some(f) = c["file"].as_str() {
                files.push(f);
            }
        }
        for f in files {
            if !media.iter().any(|m| m == f) {
                problems.push(format!("announcement \"{}\": no media file \"{f}\"", a.id));
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
