//! `mcast intercom ...`: list, check and play from the command line.

use crate::backend::{Backend, HttpBackend};
use crate::model::{Audio, Config, Day, Target, When, NO_CHIME};
use crate::runner::{Run, RunState, Runner, TargetState, Trigger};
use crate::scheduler;
use crate::store;
use anyhow::{bail, Context, Result};
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
        let describe = |au: &Audio| match au {
            Audio::Media(m) => m.clone(),
            Audio::ChimeOnly => "chime only".into(),
            Audio::Live => "live (push to talk)".into(),
            Audio::Playlist(id) => format!(
                "playlist {}",
                cfg.playlist(id).map_or(id.as_str(), |p| p.name.as_str())
            ),
        };
        let audio = describe(&a.audio);
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
        for p in &a.parts {
            extras.push(format!(
                "and {} -> {}",
                describe(&p.audio),
                p.zones.join(", ")
            ));
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
    let fires = scheduler::upcoming(&cfg, jiff::Timestamp::now(), n);
    if fires.is_empty() {
        println!("Nothing scheduled.");
        return Ok(());
    }
    if cfg.sites.is_empty() {
        println!("Times in {}:", cfg.tz_name());
    }
    for f in fires {
        // With sites, each time is in its own site's time zone.
        let at = match f.site.as_deref().and_then(|id| cfg.site(id)) {
            Some(site) => format!(" at {} ({})", site.name, cfg.site_tz_name(Some(&site.id))),
            None if !cfg.sites.is_empty() => format!(" ({})", cfg.tz_name()),
            None => String::new(),
        };
        println!("  {}  {} ({}){at}", f.when(), f.name, f.schedule);
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
    // (what, its media files, its chimes)
    type Uses<'a> = Vec<(String, Vec<&'a str>, [&'a Option<Value>; 2])>;
    let mut uses: Uses = vec![("defaults".to_string(), Vec::new(), [&d.chime, &d.end_chime])];
    for a in &cfg.announcements {
        let what = format!("announcement \"{}\"", a.id);
        let media: Vec<&str> = a
            .sections()
            .into_iter()
            .filter_map(|(au, _)| match au {
                Audio::Media(m) => Some(m.as_str()),
                _ => None,
            })
            .collect();
        uses.push((what, media, [&a.chime, &a.end_chime]));
    }
    for p in &cfg.playlists {
        let what = format!("playlist \"{}\"", p.id);
        let media = p
            .items
            .iter()
            .filter_map(crate::model::PlaylistItem::media)
            .collect();
        uses.push((what, media, [&None, &None]));
    }
    for (what, audio, chime_values) in uses {
        let mut files: Vec<&str> = audio;
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

/// Write a backup of the config, with every sound on the mcast server if
/// `sounds`, to `out` (default: `intercom-DATE.json` or `.tar.gz` here).
pub fn backup(
    path: &Path,
    backend: &HttpBackend,
    sounds: bool,
    out: Option<std::path::PathBuf>,
) -> Result<()> {
    let cfg = store::load(path)?;
    let date = jiff::Zoned::now().date();
    let ext = if sounds { "tar.gz" } else { "json" };
    let out = out.unwrap_or_else(|| format!("intercom-{date}.{ext}").into());
    if sounds {
        let files = backend
            .media_files()
            .with_context(|| format!("listing the sounds on {}", backend.base()))?;
        let mut data = Vec::with_capacity(files.len());
        for f in &files {
            let (_, bytes) = backend
                .download_media(&f.name)
                .with_context(|| format!("downloading {}", f.name))?;
            data.push((f.name.as_str(), bytes));
        }
        let file =
            std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
        crate::backup::write_archive(
            &cfg,
            data.iter().map(|(n, d)| (*n, d.as_slice())),
            crate::now_ms(),
            std::io::BufWriter::new(file),
        )?;
        let n = files.len();
        println!(
            "{}: the config and {n} sound{}",
            out.display(),
            if n == 1 { "" } else { "s" }
        );
    } else {
        let mut json = serde_json::to_vec_pretty(&cfg)?;
        json.push(b'\n');
        std::fs::write(&out, json).with_context(|| format!("writing {}", out.display()))?;
        println!("{}: the config", out.display());
    }
    println!("(Access tokens, webhooks and the play history aren't in it.)");
    Ok(())
}

/// Restore a backup (a `.tar.gz` or an `intercom.json`): say what would
/// change, then, unless `dry_run`, upload its sounds the mcast server lacks
/// and replace the config, keeping the old one as `.bak`.
pub fn restore(path: &Path, backend: &HttpBackend, file: &Path, dry_run: bool) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let b = crate::backup::read(&bytes).with_context(|| format!("in {}", file.display()))?;
    let now = store::load(path)?;
    let on_server = backend.media_files();
    if let Err(e) = &on_server {
        if !b.sounds.is_empty() && !dry_run {
            bail!(
                "the backup has sounds, but {} can't be asked: {e}",
                backend.base()
            );
        }
        println!("Couldn't ask {} about its sounds: {e}", backend.base());
    }
    let plan = crate::backup::plan(&now, &b, on_server.as_deref().ok());
    println!("Restoring {} over {}:", file.display(), path.display());
    for line in plan.describe() {
        println!("  {line}");
    }
    if dry_run || plan.same {
        return Ok(());
    }
    let done = crate::backup::apply(path, &b, &plan, backend)?;
    if !done.uploaded.is_empty() {
        let n = done.uploaded.len();
        println!("Uploaded {n} sound{}.", if n == 1 { "" } else { "s" });
    }
    match done.kept {
        Some(k) => println!("Restored. The config before is in {}.", k.display()),
        None => println!("Restored."),
    }
    Ok(())
}

/// Play an announcement and follow it to the end. Setting `stop` (Ctrl-C)
/// stops it. An error if it played to no group.
pub fn play(path: &Path, backend: Arc<dyn Backend>, which: &str, stop: &AtomicBool) -> Result<()> {
    let cfg = store::load(path)?;
    if cfg
        .announcement(which)
        .is_some_and(|a| a.audio == Audio::Live)
    {
        bail!("\"{which}\" is live: talk on it from the panel's page");
    }
    // Notifications can arrive out of order, so they only say when to look.
    let (tx, rx) = mpsc::channel::<()>();
    let runner = Runner::new(backend, move |_| {
        let _ = tx.send(());
    });
    let run = runner.play(&cfg, which, Trigger::Manual { by: None })?;
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

// ---- access tokens ----

/// A moment in the panel's time zone: "2026-11-07 14:03".
fn local_time(cfg: &Config, ms: u64) -> String {
    let tz = cfg.tz().unwrap_or_else(|_| jiff::tz::TimeZone::system());
    jiff::Timestamp::from_millisecond(ms as i64)
        .map(|t| t.to_zoned(tz).strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| "?".into())
}

/// `mcast intercom token add`: make a token and print its secret, once.
pub fn token_add(config: &Path, tokens: &Path, new: crate::tokens::NewToken) -> Result<()> {
    let cfg = store::load(config)?;
    let mut f = crate::tokens::load(tokens)?;
    let (t, secret) = f.add(new, &cfg, crate::now_ms())?;
    crate::tokens::save(tokens, &f)?;
    let mut about = vec![format!("{} token", t.role)];
    if !t.announcements.is_empty() {
        about.push(format!("plays only {}", t.announcements.join(", ")));
    }
    if !t.categories.is_empty() {
        about.push(format!("only categories {}", t.categories.join(", ")));
    }
    if let Some(d) = &t.expires {
        about.push(format!("works until the end of {d}"));
    }
    eprintln!(
        "Made \"{}\" ({}) in {}. Its secret, shown only now:",
        t.name,
        about.join(", "),
        tokens.display()
    );
    println!("{secret}");
    Ok(())
}

/// `mcast intercom token list`.
pub fn token_list(config: &Path, tokens: &Path) -> Result<()> {
    let cfg = store::load(config)?;
    let f = crate::tokens::load(tokens)?;
    if f.tokens.is_empty() {
        println!("No access tokens in {}.", tokens.display());
        return Ok(());
    }
    let now = crate::now_ms();
    println!("Access tokens in {}:", tokens.display());
    for t in &f.tokens {
        let mut about = Vec::new();
        if !t.announcements.is_empty() {
            let names: Vec<&str> = t
                .announcements
                .iter()
                .map(|id| {
                    cfg.announcement(id)
                        .map_or(id.as_str(), |a| a.name.as_str())
                })
                .collect();
            about.push(format!("plays only {}", names.join(", ")));
        }
        if !t.categories.is_empty() {
            let names: Vec<&str> = t
                .categories
                .iter()
                .map(|id| {
                    cfg.categories
                        .iter()
                        .find(|c| c.id == *id)
                        .map_or(id.as_str(), |c| c.name.as_str())
                })
                .collect();
            about.push(format!("only {}", names.join(", ")));
        }
        match &t.expires {
            Some(d) if t.expired(now) => about.push(format!("EXPIRED after {d}")),
            Some(d) => about.push(format!("until {d}")),
            None => {}
        }
        about.push(match t.last_used_ms {
            Some(ms) => format!(
                "used {} time{}, last {}",
                t.uses,
                if t.uses == 1 { "" } else { "s" },
                local_time(&cfg, ms)
            ),
            None => "never used".into(),
        });
        println!(
            "  {:<10} {:<24} {:<9} {}",
            t.id,
            t.name,
            t.role.name(),
            about.join("; ")
        );
    }
    Ok(())
}

/// `mcast intercom token revoke`, by id or name.
pub fn token_revoke(tokens: &Path, which: &str) -> Result<()> {
    let mut f = crate::tokens::load(tokens)?;
    let t = f.remove(which)?;
    crate::tokens::save(tokens, &f)?;
    println!("Revoked \"{}\" ({}).", t.name, t.role);
    Ok(())
}
