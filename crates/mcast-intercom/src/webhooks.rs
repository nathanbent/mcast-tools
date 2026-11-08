//! Outbound webhooks: the panel POSTs JSON to other systems when something
//! happens (`docs/intercom-triggers.md`).
//!
//! Hooks are kept in `webhooks.json` beside `intercom.json`, readable by its
//! owner only, since each may hold a signing secret. [`Webhooks`] turns the
//! runner's changes into events (`run_started`, `run_ended`, `run_failed`,
//! `schedule_missed`, `backend_down`, `backend_up`) and delivers each to the
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
pub const EVENTS: [&str; 6] = [
    "run_started",
    "run_ended",
    "run_failed",
    "schedule_missed",
    "backend_down",
    "backend_up",
];

/// How long a receiver has to answer.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Before the one retry.
const RETRY_AFTER: Duration = Duration::from_secs(3);
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

/// How a hook's last delivery went.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Delivery {
    pub event: String,
    pub at_ms: u64,
    pub ok: bool,
    /// The receiver's HTTP status, if it answered.
    pub status: Option<u16>,
    pub error: Option<String>,
    /// Including the retry.
    pub attempts: u32,
    pub took_ms: u64,
}

/// The running panel's hooks.
pub struct Webhooks {
    path: PathBuf,
    agent: ureq::Agent,
    st: Mutex<State>,
    in_flight: Arc<AtomicUsize>,
    next_delivery: AtomicU64,
    last: Arc<Mutex<HashMap<String, Delivery>>>,
}

#[derive(Default)]
struct State {
    file: WebhookFile,
    stamp: Option<(SystemTime, u64)>,
    /// Runs seen while still going.
    going: HashSet<RunId>,
    /// Runs already reported over, so a late update doesn't report it twice.
    ended: VecDeque<RunId>,
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
            path,
            agent: ureq::AgentBuilder::new()
                .timeout(TIMEOUT)
                .redirects(0)
                .user_agent(&format!("mcast-intercom/{}", env!("CARGO_PKG_VERSION")))
                .build(),
            st: Mutex::new(st),
            in_flight: Arc::new(AtomicUsize::new(0)),
            next_delivery: AtomicU64::new(1),
            last: Arc::default(),
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
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        hooks
            .into_iter()
            .map(|w| {
                let d = last.get(&w.id).cloned();
                (w, d)
            })
            .collect()
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
        let events: Vec<(&'static str, Value)> = {
            let mut st = self.lock();
            match c {
                Change::Run(run) => run_events(&mut st, run),
                Change::Backend(status) => backend_events(&mut st, status),
                Change::Media => Vec::new(),
            }
        };
        for (event, data) in events {
            self.send(event, data);
        }
    }

    /// Deliver `event` to every hook that wants it.
    fn send(&self, event: &str, data: Value) {
        let hooks: Vec<Webhook> = {
            let mut st = self.lock();
            self.refresh(&mut st);
            st.file
                .webhooks
                .iter()
                .filter(|w| w.wants(event))
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
        let last = self.last.clone();
        let record = move |d: Delivery| {
            last.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(w.id.clone(), d.clone());
            if let Some(tx) = &done {
                let _ = tx.send(d);
            }
        };
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            record(Delivery {
                event,
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
                    event: event.clone(),
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
    if st.ended.contains(&run.id) {
        return Vec::new();
    }
    let data = || json!({ "run": run });
    let mut out = Vec::new();
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
    }
    out
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
