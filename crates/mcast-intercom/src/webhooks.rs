//! Outbound webhooks: the panel POSTs JSON to other systems when something
//! happens (`docs/intercom-triggers.md`).
//!
//! Hooks are kept in `webhooks.json` beside `intercom.json`, readable by its
//! owner only, since each may hold a signing secret. [`Webhooks`] turns the
//! runner's changes into events (`run_started`, `run_ended`, `run_failed`,
//! `schedule_missed`, `listener_silent`, `listener_offline`, `listener_online`,
//! `backend_down`, `backend_up`) and delivers each to the
//! hooks that want it, off the caller's thread: a short timeout, one retry,
//! and the outcome kept for the Settings page. A slow or dead receiver
//! never holds up the panel.

use crate::now_ms;
use crate::runner::{BackendStatus, Change, Run, RunId, RunState};
use crate::store::stamp;
use crate::tokens::{hex, random};
use anyhow::{bail, Context, Result};
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Every event a hook can ask for.
pub const EVENTS: [&str; 9] = [
    "run_started",
    "run_ended",
    "run_failed",
    "schedule_missed",
    "listener_silent",
    "listener_offline",
    "listener_online",
    "backend_down",
    "backend_up",
];

/// How long a receiver has to answer.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Before the one retry.
const RETRY_AFTER: Duration = Duration::from_secs(3);
/// Deliveries kept per hook, newest last.
pub const LOG_PER_HOOK: usize = 50;
/// The log file is cut back to the newest lines when it grows past this.
const LOG_FILE_MAX: usize = 5000;
/// Deliveries under way at once; more are dropped (and say so).
const MAX_IN_FLIGHT: usize = 32;

/// One hook, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Webhook {
    pub id: String,
    pub name: String,
    /// `http://` or `https://`.
    pub url: String,
    /// The events it gets; empty for all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Only runs and speakers at these sites (ids); empty for all. Events
    /// about the panel itself (`backend_down`, `backend_up`) always go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
    /// Key for the `X-Mcast-Signature` header; `None` for unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub created_ms: u64,
}

fn yes() -> bool {
    true
}

impl Webhook {
    pub fn wants(&self, event: &str) -> bool {
        self.enabled && (self.events.is_empty() || self.events.iter().any(|e| e == event))
    }

    /// Whether it wants something at `sites` (`None`: about the panel, not
    /// a site). Something at no site goes only to hooks for all sites.
    pub fn covers(&self, sites: Option<&[String]>) -> bool {
        match sites {
            None => true,
            Some(at) => self.sites.is_empty() || at.iter().any(|s| self.sites.contains(s)),
        }
    }
}

/// `webhooks.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WebhookFile {
    #[serde(default)]
    pub webhooks: Vec<Webhook>,
}

/// What a new hook, or a change to one, gives. `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct HookChange {
    pub name: Option<String>,
    pub url: Option<String>,
    pub events: Option<Vec<String>>,
    pub enabled: Option<bool>,
    /// Site ids; checked against the config by the caller.
    pub sites: Option<Vec<String>>,
}

fn check_sites(sites: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for s in sites {
        let s = s.trim();
        if s.is_empty()
            || s.len() > 64
            || !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("\"{s}\" isn't a site id");
        }
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    }
    Ok(out)
}

fn check_url(url: &str) -> Result<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .with_context(|| format!("\"{url}\" isn't an http:// or https:// URL"))?;
    if rest.is_empty() || rest.starts_with('/') || url.chars().any(char::is_whitespace) {
        bail!("\"{url}\" has no host");
    }
    Ok(url.to_string())
}

fn check_events(events: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for e in events {
        let e = e.trim();
        if !EVENTS.contains(&e) {
            bail!("no event \"{e}\": use {}", EVENTS.join(", "));
        }
        if !out.iter().any(|x| x == e) {
            out.push(e.to_string());
        }
    }
    Ok(out)
}

impl WebhookFile {
    fn check_name(&self, name: &str, except: Option<&str>) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
            bail!("a webhook's name is one line of 1 to 64 characters");
        }
        if self
            .webhooks
            .iter()
            .any(|w| Some(w.id.as_str()) != except && w.name.eq_ignore_ascii_case(name))
        {
            bail!("there's already a webhook called \"{name}\"");
        }
        Ok(name.to_string())
    }

    /// Add a hook; with `sign`, give it a secret. Returns it (with its secret).
    pub fn add(&mut self, c: HookChange, sign: bool, now_ms: u64) -> Result<Webhook> {
        let name = self.check_name(c.name.as_deref().unwrap_or(""), None)?;
        let url = check_url(c.url.as_deref().context("a webhook needs a url")?)?;
        let events = check_events(&c.events.unwrap_or_default())?;
        let sites = check_sites(&c.sites.unwrap_or_default())?;
        let secret = sign.then(new_secret).transpose()?;
        let id = loop {
            let id = format!("w_{}", hex(&random::<3>()?));
            if !self.webhooks.iter().any(|w| w.id == id) {
                break id;
            }
        };
        let w = Webhook {
            id,
            name,
            url,
            events,
            enabled: c.enabled.unwrap_or(true),
            sites,
            secret,
            created_ms: now_ms,
        };
        self.webhooks.push(w.clone());
        Ok(w)
    }

    pub fn change(&mut self, id: &str, c: HookChange) -> Result<Webhook> {
        let i = self
            .webhooks
            .iter()
            .position(|w| w.id == id)
            .with_context(|| format!("no webhook \"{id}\""))?;
        let name = c.name.map(|n| self.check_name(&n, Some(id))).transpose()?;
        let url = c.url.map(|u| check_url(&u)).transpose()?;
        let events = c.events.map(|e| check_events(&e)).transpose()?;
        let sites = c.sites.map(|s| check_sites(&s)).transpose()?;
        let w = &mut self.webhooks[i];
        if let Some(n) = name {
            w.name = n;
        }
        if let Some(u) = url {
            w.url = u;
        }
        if let Some(e) = events {
            w.events = e;
        }
        if let Some(on) = c.enabled {
            w.enabled = on;
        }
        if let Some(s) = sites {
            w.sites = s;
        }
        Ok(w.clone())
    }

    pub fn remove(&mut self, id: &str) -> Result<Webhook> {
        let i = self
            .webhooks
            .iter()
            .position(|w| w.id == id)
            .with_context(|| format!("no webhook \"{id}\""))?;
        Ok(self.webhooks.remove(i))
    }

    fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        for w in &self.webhooks {
            if !ids.insert(w.id.as_str()) {
                bail!("two webhooks have the id \"{}\"", w.id);
            }
            check_url(&w.url).with_context(|| format!("webhook \"{}\"", w.name))?;
            check_events(&w.events).with_context(|| format!("webhook \"{}\"", w.name))?;
            check_sites(&w.sites).with_context(|| format!("webhook \"{}\"", w.name))?;
        }
        Ok(())
    }
}

/// `whsec_` and 32 random bytes in hex.
fn new_secret() -> Result<String> {
    Ok(format!("whsec_{}", hex(&random::<32>()?)))
}

/// `sha256=` and the hex HMAC-SHA256 of `{timestamp}.{body}` with `secret`:
/// what the receiver checks the `X-Mcast-Signature` header against.
pub fn signature(secret: &str, timestamp_ms: u64, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(timestamp_ms.to_string().as_bytes());
    ctx.update(b".");
    ctx.update(body);
    format!("sha256={}", hex(ctx.sign().as_ref()))
}

/// `webhooks.json` beside the config file.
pub fn default_path(config: &Path) -> PathBuf {
    config.with_file_name("webhooks.json")
}

pub fn load(path: &Path) -> Result<WebhookFile> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WebhookFile::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let f: WebhookFile =
        serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    f.validate()
        .with_context(|| format!("in {}", path.display()))?;
    Ok(f)
}

pub fn save(path: &Path, f: &WebhookFile) -> Result<()> {
    f.validate()?;
    crate::store::save_private(path, f)
}

/// How a delivery went.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// The `X-Mcast-Delivery` number (they start again after a restart).
    #[serde(default)]
    pub delivery: u64,
    pub event: String,
    /// What it was about: the announcement, or the speaker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    pub at_ms: u64,
    pub ok: bool,
    /// The receiver's HTTP status, if it answered.
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub error: Option<String>,
    /// Including the retry.
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub took_ms: u64,
}

/// A delivery as the log file keeps it.
#[derive(Serialize, Deserialize)]
struct Logged {
    hook: String,
    #[serde(flatten)]
    d: Delivery,
}

/// The hooks' recent deliveries, in memory and in `webhook-log.jsonl`
/// beside `webhooks.json` (owner-only), so they outlive a restart.
struct Log {
    path: PathBuf,
    hooks: HashMap<String, VecDeque<Delivery>>,
    lines: usize,
}

impl Log {
    fn open(path: PathBuf) -> Self {
        let mut log = Log {
            path,
            hooks: HashMap::new(),
            lines: 0,
        };
        if let Ok(text) = std::fs::read_to_string(&log.path) {
            for line in text.lines() {
                log.lines += 1;
                if let Ok(l) = serde_json::from_str::<Logged>(line) {
                    log.keep(l.hook, l.d);
                }
            }
        }
        log
    }

    fn keep(&mut self, hook: String, d: Delivery) {
        let q = self.hooks.entry(hook).or_default();
        q.push_back(d);
        while q.len() > LOG_PER_HOOK {
            q.pop_front();
        }
    }

    /// Note a delivery, and append it to the file (cutting the file back to
    /// what's kept once it's long).
    fn add(&mut self, hook: &str, d: Delivery) {
        self.keep(hook.to_string(), d.clone());
        let line = serde_json::to_string(&Logged {
            hook: hook.to_string(),
            d,
        })
        .unwrap_or_default();
        let wrote = if self.lines + 1 > LOG_FILE_MAX {
            self.rewrite()
        } else {
            append_private(&self.path, &line).map(|_| self.lines += 1)
        };
        if let Err(e) = wrote {
            eprintln!("mcast intercom: writing {}: {e}", self.path.display());
        }
    }

    /// The file, from what's kept.
    fn rewrite(&mut self) -> std::io::Result<()> {
        let mut all: Vec<(&String, &Delivery)> = self
            .hooks
            .iter()
            .flat_map(|(h, q)| q.iter().map(move |d| (h, d)))
            .collect();
        all.sort_by_key(|(_, d)| d.at_ms);
        let mut text = String::new();
        for (hook, d) in &all {
            let l = Logged {
                hook: (*hook).clone(),
                d: (*d).clone(),
            };
            text.push_str(&serde_json::to_string(&l).unwrap_or_default());
            text.push('\n');
        }
        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        write_private(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &self.path)?;
        self.lines = all.len();
        Ok(())
    }

    /// Forget a deleted hook's deliveries.
    fn forget(&mut self, hook: &str) {
        if self.hooks.remove(hook).is_some() {
            if let Err(e) = self.rewrite() {
                eprintln!("mcast intercom: writing {}: {e}", self.path.display());
            }
        }
    }
}

fn private_options() -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o
}

fn append_private(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = private_options().create(true).append(true).open(path)?;
    f.write_all(format!("{line}\n").as_bytes())
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = private_options()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    f.write_all(data)
}

/// The log beside `webhooks.json`.
pub fn log_path(webhooks: &Path) -> PathBuf {
    webhooks.with_file_name("webhook-log.jsonl")
}

/// What a delivery is about, for the log.
fn about(data: &Value) -> Option<String> {
    data["run"]["name"]
        .as_str()
        .or(data["speaker"]["name"].as_str())
        .map(String::from)
}

/// The running panel's hooks.
pub struct Webhooks {
    path: PathBuf,
    agent: ureq::Agent,
    st: Mutex<State>,
    in_flight: Arc<AtomicUsize>,
    next_delivery: AtomicU64,
    log: Arc<Mutex<Log>>,
}

#[derive(Default)]
struct State {
    file: WebhookFile,
    stamp: Option<(SystemTime, u64)>,
    /// Runs seen while still going.
    going: HashSet<RunId>,
    /// Runs already reported over, so a late update doesn't report it twice.
    ended: VecDeque<RunId>,
    /// Runs whose listener check has been reported on.
    checked: VecDeque<RunId>,
    /// The connection, as last seen.
    connected: Option<bool>,
}

impl Webhooks {
    /// Read the file. A broken one is logged and treated as empty until it's
    /// fixed (`serve` refuses to start with one).
    pub fn open(path: PathBuf) -> Self {
        let file = load(&path).unwrap_or_else(|e| {
            eprintln!("mcast intercom: {e:#}; no webhooks until it's fixed");
            WebhookFile::default()
        });
        let st = State {
            file,
            stamp: stamp(&path),
            ..Default::default()
        };
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(TIMEOUT)
                .redirects(0)
                .user_agent(&format!("mcast-intercom/{}", env!("CARGO_PKG_VERSION")))
                .build(),
            st: Mutex::new(st),
            in_flight: Arc::new(AtomicUsize::new(0)),
            next_delivery: AtomicU64::new(1),
            log: Arc::new(Mutex::new(Log::open(log_path(&path)))),
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn refresh(&self, st: &mut State) {
        let now = stamp(&self.path);
        if now == st.stamp {
            return;
        }
        st.stamp = now;
        match load(&self.path) {
            Ok(f) => st.file = f,
            Err(e) => eprintln!("mcast intercom: {e:#}; keeping the webhooks read before"),
        }
    }

    /// The hooks, with how each one's last delivery went.
    pub fn list(&self) -> Vec<(Webhook, Option<Delivery>)> {
        let hooks = {
            let mut st = self.lock();
            self.refresh(&mut st);
            st.file.webhooks.clone()
        };
        let log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        hooks
            .into_iter()
            .map(|w| {
                let d = log.hooks.get(&w.id).and_then(|q| q.back().cloned());
                (w, d)
            })
            .collect()
    }

    /// A hook's recent deliveries, newest first (at most [`LOG_PER_HOOK`]).
    pub fn deliveries(&self, id: &str) -> Vec<Delivery> {
        let log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        log.hooks
            .get(id)
            .map(|q| q.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// A hook was deleted: forget what it was sent.
    pub fn forget(&self, id: &str) {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget(id);
    }

    /// Change the file (read afresh first). Blocks on the disk.
    pub fn modify<R>(&self, f: impl FnOnce(&mut WebhookFile) -> Result<R>) -> Result<R> {
        let mut st = self.lock();
        let mut file = load(&self.path)?;
        let r = f(&mut file)?;
        save(&self.path, &file)?;
        st.file = file;
        st.stamp = stamp(&self.path);
        Ok(r)
    }

    /// Hear about a change from the runner; never blocks on the network.
    pub fn observe(&self, c: &Change<'_>) {
        let (events, sites): (Vec<(&'static str, Value)>, Option<&[String]>) = {
            let mut st = self.lock();
            match c {
                Change::Run(run) => (run_events(&mut st, run), Some(&run.sites)),
                Change::Backend(status) => (backend_events(&mut st, status), None),
                Change::Media | Change::Shown(_) => (Vec::new(), None),
            }
        };
        for (event, data) in events {
            self.send(event, data, sites);
        }
    }

    /// Speakers that went offline or came back between plays:
    /// `listener_offline` / `listener_online`, `{"speaker"}`.
    pub fn speakers(&self, changes: &[crate::presence::SpeakerChange]) {
        for c in changes {
            let event = if c.online {
                "listener_online"
            } else {
                "listener_offline"
            };
            self.send(
                event,
                json!({ "speaker": c.speaker }),
                Some(&c.speaker.sites),
            );
        }
    }

    /// Deliver `event` to every hook that wants it, and covers `sites`
    /// (`None`: not about a site).
    fn send(&self, event: &str, data: Value, sites: Option<&[String]>) {
        let hooks: Vec<Webhook> = {
            let mut st = self.lock();
            self.refresh(&mut st);
            st.file
                .webhooks
                .iter()
                .filter(|w| w.wants(event) && w.covers(sites))
                .cloned()
                .collect()
        };
        for w in hooks {
            self.deliver(w, event, data.clone(), None);
        }
    }

    /// A `test` event to one hook, waiting for the answer.
    pub fn test(&self, id: &str) -> Result<Delivery> {
        let w = {
            let mut st = self.lock();
            self.refresh(&mut st);
            st.file.webhooks.iter().find(|w| w.id == id).cloned()
        }
        .with_context(|| format!("no webhook \"{id}\""))?;
        let (tx, rx) = std::sync::mpsc::channel();
        let data =
            json!({ "message": format!("A test from the intercom panel to \"{}\"", w.name) });
        self.deliver(w, "test", data, Some(tx));
        rx.recv_timeout(TIMEOUT * 3 + RETRY_AFTER)
            .context("the delivery didn't finish")
    }

    fn deliver(
        &self,
        w: Webhook,
        event: &str,
        data: Value,
        done: Option<std::sync::mpsc::Sender<Delivery>>,
    ) {
        let at_ms = now_ms();
        let id = self.next_delivery.fetch_add(1, Ordering::Relaxed);
        let mut body = json!({ "event": event, "at_ms": at_ms, "delivery": id });
        if let (Some(b), Some(d)) = (body.as_object_mut(), data.as_object()) {
            b.extend(d.clone());
        }
        let body = body.to_string().into_bytes();
        let event = event.to_string();
        let about = about(&data);
        let log = self.log.clone();
        let hook = w.id.clone();
        let record = move |d: Delivery| {
            log.lock()
                .unwrap_or_else(|e| e.into_inner())
                .add(&hook, d.clone());
            if let Some(tx) = &done {
                let _ = tx.send(d);
            }
        };
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            record(Delivery {
                delivery: id,
                event,
                about,
                at_ms,
                error: Some("dropped: too many deliveries under way".into()),
                ..Default::default()
            });
            return;
        }
        let in_flight = self.in_flight.clone();
        let agent = self.agent.clone();
        let spawned = std::thread::Builder::new()
            .name("webhook".into())
            .spawn(move || {
                let started = std::time::Instant::now();
                let mut d = Delivery {
                    delivery: id,
                    event: event.clone(),
                    about,
                    at_ms,
                    ..Default::default()
                };
                for attempt in 1..=2 {
                    d.attempts = attempt;
                    let ts = now_ms();
                    let mut req = agent
                        .post(&w.url)
                        .set("Content-Type", "application/json")
                        .set("X-Mcast-Event", &event)
                        .set("X-Mcast-Delivery", &id.to_string())
                        .set("X-Mcast-Timestamp", &ts.to_string());
                    if let Some(secret) = &w.secret {
                        req = req.set("X-Mcast-Signature", &signature(secret, ts, &body));
                    }
                    let retry = match req.send_bytes(&body) {
                        Ok(resp) => {
                            d.ok = true;
                            d.status = Some(resp.status());
                            d.error = None;
                            false
                        }
                        Err(ureq::Error::Status(code, _)) => {
                            d.status = Some(code);
                            d.error = Some(format!("the receiver answered HTTP {code}"));
                            code >= 500 || code == 429
                        }
                        Err(ureq::Error::Transport(t)) => {
                            d.status = None;
                            d.error = Some(t.to_string());
                            true
                        }
                    };
                    if !retry || attempt == 2 {
                        break;
                    }
                    std::thread::sleep(RETRY_AFTER);
                }
                d.took_ms = started.elapsed().as_millis() as u64;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                record(d);
            });
        if spawned.is_err() {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// What a run's change means for hooks: started, and over (how).
fn run_events(st: &mut State, run: &Run) -> Vec<(&'static str, Value)> {
    let data = || json!({ "run": run });
    let mut out = Vec::new();
    if st.ended.contains(&run.id) {
        listener_events(st, run, &mut out);
        return out;
    }
    let over = run.state.is_over();
    let first = !st.going.contains(&run.id);
    if first && !over {
        st.going.insert(run.id);
        out.push(("run_started", data()));
    }
    if over {
        st.going.remove(&run.id);
        st.ended.push_back(run.id);
        if st.ended.len() > 256 {
            st.ended.pop_front();
        }
        let event = match run.state {
            RunState::Missed => "schedule_missed",
            RunState::Failed => "run_failed",
            _ => "run_ended",
        };
        out.push((event, data()));
        listener_events(st, run, &mut out);
    }
    out
}

/// Once a run's listener check is over: `listener_silent` if anyone
/// expected didn't hear it. Its `run.listeners.silent` names them.
fn listener_events(st: &mut State, run: &Run, out: &mut Vec<(&'static str, Value)>) {
    let Some(l) = &run.listeners else {
        return;
    };
    if l.checking || st.checked.contains(&run.id) {
        return;
    }
    st.checked.push_back(run.id);
    if st.checked.len() > 256 {
        st.checked.pop_front();
    }
    if !l.silent.is_empty() {
        out.push(("listener_silent", json!({ "run": run })));
    }
}

/// The connection going down (or not coming up at the start), and back.
fn backend_events(st: &mut State, status: &BackendStatus) -> Vec<(&'static str, Value)> {
    let before = st.connected.replace(status.connected);
    let event = match (before, status.connected) {
        (Some(true) | None, false) => "backend_down",
        (Some(false), true) => "backend_up",
        _ => return Vec::new(),
    };
    vec![(event, json!({ "status": status }))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_events_and_names() {
        let mut f = WebhookFile::default();
        let c = |name: &str, url: &str| HookChange {
            name: Some(name.into()),
            url: Some(url.into()),
            ..Default::default()
        };
        let w = f
            .add(c("Slack", "https://hooks.example.com/x"), true, 1)
            .unwrap();
        assert!(w.secret.as_deref().unwrap().starts_with("whsec_"));
        assert!(w.wants("run_started"));
        assert!(f.add(c("slack", "https://a.example"), false, 1).is_err());
        for bad in [
            "ftp://x",
            "https://",
            "http:///x",
            "example.com",
            "https://a b",
        ] {
            assert!(f.add(c("Other", bad), false, 1).is_err(), "{bad}");
        }
        let mut only = c("Alarms", "http://10.0.0.5:8080/hook");
        only.events = Some(vec!["run_failed".into(), "backend_down".into()]);
        let w = f.add(only, false, 1).unwrap();
        assert!(w.secret.is_none() && w.wants("backend_down") && !w.wants("run_started"));
        let mut wrong = c("Wrong", "http://x");
        wrong.events = Some(vec!["everything".into()]);
        assert!(f.add(wrong, false, 1).is_err());

        let off = HookChange {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!f.change(&w.id, off).unwrap().wants("backend_down"));
        assert!(f.remove(&w.id).is_ok() && f.remove(&w.id).is_err());
    }

    fn run(id: RunId, state: RunState) -> Run {
        let mut r = Run::for_tests(id);
        r.state = state;
        r
    }

    #[test]
    fn a_silent_listener_is_reported_once_its_check_ends() {
        use crate::runner::Listeners;
        let mut st = State::default();
        let names =
            |v: Vec<(&'static str, Value)>| v.into_iter().map(|(e, _)| e).collect::<Vec<_>>();
        let with = |checking: bool, silent: &[&str]| {
            let mut r = run(1, RunState::Done);
            r.listeners = Some(Box::new(Listeners {
                checking,
                expected: vec!["Lobby".into(), "Hall".into()],
                silent: silent.iter().map(|s| s.to_string()).collect(),
                ..Listeners::default()
            }));
            r
        };
        // Over, still listening: only that it ended.
        assert_eq!(names(run_events(&mut st, &with(true, &[]))), ["run_ended"]);
        assert!(run_events(&mut st, &with(true, &[])).is_empty());
        let events = run_events(&mut st, &with(false, &["Hall"]));
        assert_eq!(names(events.clone()), ["listener_silent"]);
        assert_eq!(events[0].1["run"]["listeners"]["silent"], json!(["Hall"]));
        assert!(
            run_events(&mut st, &with(false, &["Hall"])).is_empty(),
            "once only"
        );

        // Everyone heard it: nothing more. Checked as it ends: both at once.
        let mut st = State::default();
        let mut quiet = with(false, &[]);
        quiet.id = 2;
        assert_eq!(names(run_events(&mut st, &quiet)), ["run_ended"]);
        let mut both = with(false, &["Hall"]);
        both.id = 3;
        assert_eq!(
            names(run_events(&mut st, &both)),
            ["run_ended", "listener_silent"]
        );
    }

    #[test]
    fn the_delivery_log_keeps_the_newest_and_outlives_a_restart() {
        let dir = std::env::temp_dir().join(format!(
            "mcast-hook-log-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("webhook-log.jsonl");
        let d = |n: u64, event: &str| Delivery {
            delivery: n,
            event: event.into(),
            about: Some(format!("Bell {n}")),
            at_ms: n,
            ok: n % 2 == 0,
            status: Some(204),
            ..Default::default()
        };
        let mut log = Log::open(path.clone());
        for n in 1..=60 {
            log.add("w_a", d(n, "run_ended"));
        }
        log.add("w_b", d(100, "test"));
        assert_eq!(log.hooks["w_a"].len(), LOG_PER_HOOK);
        assert_eq!(log.hooks["w_a"].front().unwrap().delivery, 11);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Read back after a restart: the same.
        let again = Log::open(path.clone());
        assert_eq!(again.hooks, log.hooks);
        assert_eq!(again.lines, 61);
        // Cut back to what's kept; a deleted hook's go.
        log.rewrite().unwrap();
        assert_eq!(Log::open(path.clone()).lines, LOG_PER_HOOK + 1);
        log.forget("w_b");
        let after = Log::open(path.clone());
        assert!(!after.hooks.contains_key("w_b"));
        assert_eq!(
            after.hooks["w_a"].back().unwrap().about.as_deref(),
            Some("Bell 60")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hooks_limited_to_sites() {
        let mut f = WebhookFile::default();
        let c = |name: &str, sites: Option<Vec<&str>>| HookChange {
            name: Some(name.into()),
            url: Some("http://x".into()),
            sites: sites.map(|s| s.into_iter().map(String::from).collect()),
            ..Default::default()
        };
        let all = f.add(c("All", None), false, 1).unwrap();
        let north = f
            .add(c("North", Some(vec![" north", "north"])), false, 1)
            .unwrap();
        assert_eq!(north.sites, ["north"]);
        assert!(f.add(c("Bad", Some(vec!["a b"])), false, 1).is_err());
        let at = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // About the panel: everyone. At no site: only hooks for all sites.
        for (sites, all_gets, north_gets) in [
            (None, true, true),
            (Some(at(&[])), true, false),
            (Some(at(&["south"])), true, false),
            (Some(at(&["south", "north"])), true, true),
        ] {
            assert_eq!(all.covers(sites.as_deref()), all_gets, "{sites:?}");
            assert_eq!(north.covers(sites.as_deref()), north_gets, "{sites:?}");
        }
        let changed = f
            .change(
                &north.id,
                HookChange {
                    sites: Some(Vec::new()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(changed.sites.is_empty());
    }

    #[test]
    fn runs_and_the_connection_become_events() {
        let mut st = State::default();
        let names =
            |v: Vec<(&'static str, Value)>| v.into_iter().map(|(e, _)| e).collect::<Vec<_>>();
        assert_eq!(
            names(run_events(&mut st, &run(1, RunState::Playing))),
            ["run_started"]
        );
        assert!(run_events(&mut st, &run(1, RunState::Waiting)).is_empty());
        assert_eq!(
            names(run_events(&mut st, &run(1, RunState::Done))),
            ["run_ended"]
        );
        assert!(
            run_events(&mut st, &run(1, RunState::Done)).is_empty(),
            "once only"
        );
        assert_eq!(
            names(run_events(&mut st, &run(2, RunState::Failed))),
            ["run_failed"]
        );
        assert_eq!(
            names(run_events(&mut st, &run(3, RunState::Missed))),
            ["schedule_missed"]
        );
        assert_eq!(
            names(run_events(&mut st, &run(4, RunState::Playing))),
            ["run_started"]
        );
        assert_eq!(
            names(run_events(&mut st, &run(4, RunState::Stopped))),
            ["run_ended"]
        );

        let status = |connected| BackendStatus {
            connected,
            error: None,
            since_ms: 0,
        };
        assert!(
            backend_events(&mut st, &status(true)).is_empty(),
            "up at the start: nothing"
        );
        assert_eq!(
            names(backend_events(&mut st, &status(false))),
            ["backend_down"]
        );
        assert!(backend_events(&mut st, &status(false)).is_empty());
        assert_eq!(
            names(backend_events(&mut st, &status(true))),
            ["backend_up"]
        );
        let mut fresh = State::default();
        assert_eq!(
            names(backend_events(&mut fresh, &status(false))),
            ["backend_down"],
            "down at the start"
        );
    }

    #[test]
    fn signatures_are_hmac_sha256_of_timestamp_and_body() {
        // echo -n '1700000000000.{"a":1}' | openssl dgst -sha256 -hmac whsec_k
        assert_eq!(
            signature("whsec_k", 1_700_000_000_000, br#"{"a":1}"#),
            "sha256=0fef45d0a97ab787cdfcae1508ba4337d8ce07ad39a9e32dfa302e154d10a20c"
        );
    }
}
