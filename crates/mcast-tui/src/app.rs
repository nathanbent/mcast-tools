//! TUI state, and how API results and server events change it. Keys are
//! handled in `keys.rs`, drawing in `ui/`.

use crate::api::{ApiError, Client, Feed};
use crate::form::PageForm;
use crate::model::{Device, Job, MediaFile, Probe, Report, Snapshot, Stream};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Ended jobs listed under the running ones.
const RECENT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Jobs,
    Streams,
    Monitor,
    Media,
    Probes,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Jobs,
        Tab::Streams,
        Tab::Monitor,
        Tab::Media,
        Tab::Probes,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Jobs => "Now playing",
            Tab::Streams => "Streams",
            Tab::Monitor => "Monitor",
            Tab::Media => "Media",
            Tab::Probes => "Probes",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|&t| t == self).expect("listed")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    /// The page form is open.
    Page,
    /// Typing a local file path to upload.
    Upload(String),
    /// Waiting for y/n to delete a media file.
    ConfirmDelete(String),
    /// Editing one setting of a probe.
    ProbeInput {
        id: String,
        field: ProbeField,
        text: String,
    },
    /// Waiting for y/n to forget a probe.
    ConfirmForget(String),
    Help,
}

/// Probe settings typed in a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeField {
    Label,
    Groups,
    Interval,
}

impl ProbeField {
    pub fn prompt(self) -> &'static str {
        match self {
            ProbeField::Label => "Label (empty for none):",
            ProbeField::Groups => "Groups to watch besides announced ones, e.g. 239.192.2.1:10000 (space or comma between):",
            ProbeField::Interval => "Seconds per report and check-in (1-3600):",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Error,
}

/// Results from background threads.
pub enum Msg {
    Feed(Feed),
    Jobs(Result<Vec<Job>, ApiError>),
    Streams(Result<Vec<Stream>, ApiError>),
    Media(Result<Vec<MediaFile>, ApiError>),
    Devices(Result<Vec<Device>, ApiError>),
    Monitor(Result<Snapshot, ApiError>),
    Probes(Result<Vec<Probe>, ApiError>),
    /// A probe's stored reports, for its timeline.
    Reports(String, Result<Vec<Report>, ApiError>),
    /// A probe after a change to it.
    ProbeChanged(Result<Box<Probe>, ApiError>, String),
    /// A job was started (or failed to).
    Started(Result<Value, ApiError>),
    /// Some other action finished: the message to show, and what to refetch.
    Done(Result<String, ApiError>, Option<&'static str>),
}

pub struct App {
    pub client: Client,
    tx: Sender<Msg>,
    pub tab: Tab,
    pub mode: Mode,
    pub jobs: BTreeMap<u64, Job>,
    pub streams: Vec<Stream>,
    pub media: Vec<MediaFile>,
    pub devices: Vec<Device>,
    /// `None` until loaded; `Some(None)` when the server's monitor is off.
    pub monitor: Option<Option<Snapshot>>,
    /// Sorted by ID.
    pub probes: Vec<Probe>,
    /// The selected probe's reports (its ID, oldest first).
    pub reports: Option<(String, Vec<Report>)>,
    /// A probe whose reports are being fetched.
    reports_loading: Option<String>,
    /// Selected row per tab.
    pub selected: [usize; 5],
    pub form: PageForm,
    /// Output device for Listen ("" = default).
    pub output: String,
    pub status: Option<(String, Level, Instant)>,
    pub connection: Result<(), String>,
    /// The job that refused the last page (409), offered for stopping.
    pub conflict: Option<u64>,
    pub busy: bool,
    pub quit: bool,
}

impl App {
    pub fn new(client: Client, tx: Sender<Msg>) -> Self {
        Self {
            client,
            tx,
            tab: Tab::Jobs,
            mode: Mode::Normal,
            jobs: BTreeMap::new(),
            streams: Vec::new(),
            media: Vec::new(),
            devices: Vec::new(),
            monitor: None,
            probes: Vec::new(),
            reports: None,
            reports_loading: None,
            selected: [0; 5],
            form: PageForm::new(),
            output: String::new(),
            status: None,
            connection: Err("connecting".into()),
            conflict: None,
            busy: false,
            quit: false,
        }
    }

    /// Run `f` against the API in a background thread.
    pub fn spawn(&self, f: impl FnOnce(&Client) -> Msg + Send + 'static) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(f(&client));
        });
    }

    pub fn refresh(&self, what: &str) {
        match what {
            "jobs" => self.spawn(|c| Msg::Jobs(c.get("/jobs"))),
            "streams" => self.spawn(|c| Msg::Streams(c.get("/streams"))),
            "media" => self.spawn(|c| Msg::Media(c.get("/media"))),
            "devices" => self.spawn(|c| {
                Msg::Devices(c.get::<Value>("/devices").and_then(|v| {
                    serde_json::from_value(v["devices"].clone()).map_err(|e| ApiError {
                        status: 0,
                        message: e.to_string(),
                        job: None,
                    })
                }))
            }),
            "monitor" => self.spawn(|c| Msg::Monitor(c.get("/monitor"))),
            "probes" => self.spawn(|c| Msg::Probes(c.get("/probes"))),
            _ => {}
        }
    }

    pub fn refresh_all(&self) {
        for w in ["jobs", "streams", "media", "devices", "monitor", "probes"] {
            self.refresh(w);
        }
    }

    pub fn say(&mut self, text: impl Into<String>, level: Level) {
        self.status = Some((text.into(), level, Instant::now()));
    }

    /// The status message, until it's old.
    pub fn status(&self) -> Option<(&str, Level)> {
        let (text, level, at) = self.status.as_ref()?;
        let ttl = if *level == Level::Error { 12 } else { 5 };
        (at.elapsed() < Duration::from_secs(ttl)).then_some((text.as_str(), *level))
    }

    /// Jobs as listed: running ones newest first, then recent ones.
    pub fn job_rows(&self) -> Vec<&Job> {
        let mut active: Vec<&Job> = self.jobs.values().rev().filter(|j| j.active()).collect();
        active.extend(
            self.jobs
                .values()
                .rev()
                .filter(|j| !j.active())
                .take(RECENT),
        );
        active
    }

    /// Number of rows on the current tab.
    pub fn rows(&self) -> usize {
        match self.tab {
            Tab::Jobs => self.job_rows().len(),
            Tab::Streams => self.streams.len(),
            Tab::Monitor => self
                .monitor
                .as_ref()
                .and_then(|m| m.as_ref())
                .map_or(0, |m| m.streams.len()),
            Tab::Media => self.media.len(),
            Tab::Probes => self.probes.len(),
        }
    }

    pub fn selected(&self) -> usize {
        self.selected[self.tab.index()].min(self.rows().saturating_sub(1))
    }

    fn sync_form(&mut self) {
        let media: Vec<String> = self.media.iter().map(|m| m.name.clone()).collect();
        let inputs: Vec<String> = self
            .devices
            .iter()
            .filter(|d| d.kind == "input")
            .map(|d| d.name.clone())
            .collect();
        self.form.set_options(&media, &inputs);
    }

    pub fn handle(&mut self, msg: Msg) {
        let fail = |app: &mut App, e: ApiError| app.say(e.message, Level::Error);
        match msg {
            Msg::Feed(Feed::Connected) => {
                self.connection = Ok(());
                // Events may have been missed while disconnected.
                self.refresh_all();
            }
            Msg::Feed(Feed::Disconnected(why)) => self.connection = Err(why),
            Msg::Feed(Feed::Event(e)) => self.event(e),
            Msg::Jobs(r) => match r {
                Ok(jobs) => self.jobs = jobs.into_iter().map(|j| (j.id, j)).collect(),
                Err(e) => fail(self, e),
            },
            Msg::Streams(r) => match r {
                Ok(s) => self.streams = s,
                Err(e) => fail(self, e),
            },
            Msg::Media(r) => match r {
                Ok(m) => {
                    self.media = m;
                    self.sync_form();
                }
                Err(e) => fail(self, e),
            },
            Msg::Devices(r) => match r {
                Ok(d) => {
                    self.devices = d;
                    self.sync_form();
                }
                Err(e) => fail(self, e),
            },
            Msg::Monitor(r) => match r {
                Ok(m) => self.monitor = Some(Some(m)),
                Err(e) if e.status == 404 => self.monitor = Some(None),
                Err(e) => fail(self, e),
            },
            Msg::Probes(r) => match r {
                Ok(mut p) => {
                    p.sort_by(|a, b| a.id.cmp(&b.id));
                    self.probes = p;
                    self.want_reports();
                }
                Err(e) => fail(self, e),
            },
            Msg::Reports(id, r) => {
                if self.reports_loading.as_deref() == Some(id.as_str()) {
                    self.reports_loading = None;
                }
                match r {
                    Ok(more) => {
                        let mut all = match self.reports.take() {
                            Some((have, old)) if have == id => old,
                            _ => Vec::new(),
                        };
                        let last = all.last().map(|r| r.end_ms);
                        all.extend(
                            more.into_iter()
                                .filter(|r| last.map_or(true, |l| r.end_ms > l)),
                        );
                        let excess = all.len().saturating_sub(360);
                        all.drain(..excess);
                        self.reports = Some((id, all));
                    }
                    Err(e) if e.status == 404 => {}
                    Err(e) => fail(self, e),
                }
            }
            Msg::ProbeChanged(r, what) => {
                self.busy = false;
                match r {
                    Ok(p) => {
                        self.say(format!("{}: {what}", p.id), Level::Ok);
                        self.put_probe(*p);
                    }
                    Err(e) => fail(self, e),
                }
            }
            Msg::Started(r) => {
                self.busy = false;
                match r {
                    Ok(job) => {
                        if let Ok(j) = serde_json::from_value::<Job>(job) {
                            self.say(format!("{} started", j.title()), Level::Ok);
                            self.jobs.insert(j.id, j);
                        }
                        self.conflict = None;
                        self.mode = Mode::Normal;
                        self.tab = Tab::Jobs;
                        self.selected[Tab::Jobs.index()] = 0;
                    }
                    Err(e) if e.status == 409 => {
                        // Show what's in the way; the form keeps its values.
                        self.conflict = e.job;
                        self.mode = Mode::Normal;
                        self.tab = Tab::Jobs;
                        self.say(
                            format!("{} · x stops it, then p to page again", e.message),
                            Level::Error,
                        );
                    }
                    Err(e) => fail(self, e),
                }
            }
            Msg::Done(r, then) => {
                self.busy = false;
                match r {
                    Ok(text) => self.say(text, Level::Ok),
                    Err(e) => fail(self, e),
                }
                if let Some(w) = then {
                    self.refresh(w);
                }
            }
        }
    }

    fn event(&mut self, e: Value) {
        match e["type"].as_str().unwrap_or("") {
            "job_started" | "job_updated" | "job_ended" => {
                if let Ok(j) = serde_json::from_value::<Job>(e["job"].clone()) {
                    self.jobs.insert(j.id, j);
                }
            }
            "stream_added" | "stream_changed" | "stream_removed" => self.refresh("streams"),
            "monitor" => {
                if let Ok(s) = serde_json::from_value(e["snapshot"].clone()) {
                    self.monitor = Some(Some(s));
                }
            }
            "probe_updated" => {
                if let Ok(p) = serde_json::from_value::<Probe>(e["probe"].clone()) {
                    let id = p.id.clone();
                    self.put_probe(p);
                    // Keep the selected probe's timeline current.
                    if self.selected_probe().is_some_and(|p| p.id == id) {
                        self.load_reports(id);
                    }
                }
            }
            "probe_removed" => {
                if let Some(id) = e["id"].as_str() {
                    self.probes.retain(|p| p.id != id);
                    self.want_reports();
                }
            }
            "lagged" => self.refresh_all(),
            _ => {}
        }
    }

    // ---- probes ----

    fn put_probe(&mut self, p: Probe) {
        match self.probes.binary_search_by(|q| q.id.cmp(&p.id)) {
            Ok(i) => self.probes[i] = p,
            Err(i) => self.probes.insert(i, p),
        }
        self.want_reports();
    }

    pub fn selected_probe(&self) -> Option<&Probe> {
        self.probes
            .get(self.selected[Tab::Probes.index()].min(self.probes.len().saturating_sub(1)))
    }

    /// The reports shown belong to the selected probe.
    pub fn probe_reports(&self) -> Option<&[Report]> {
        let p = self.selected_probe()?;
        match &self.reports {
            Some((id, r)) if *id == p.id => Some(r),
            _ => None,
        }
    }

    /// Fetch the selected probe's reports if they aren't here yet.
    pub fn want_reports(&mut self) {
        let Some(id) = self.selected_probe().map(|p| p.id.clone()) else {
            return;
        };
        if self.reports.as_ref().is_some_and(|(have, _)| *have == id) {
            return;
        }
        self.load_reports(id);
    }

    /// Fetch reports newer than those held (all of them for another probe).
    fn load_reports(&mut self, id: String) {
        if self.reports_loading.as_deref() == Some(id.as_str()) {
            return;
        }
        let after = match &self.reports {
            Some((have, r)) if *have == id => r.last().map(|r| r.end_ms),
            _ => None,
        };
        self.reports_loading = Some(id.clone());
        let path = format!(
            "/probes/{}/reports{}",
            crate::api::encode(&id),
            after.map_or(String::new(), |a| format!("?after_ms={a}"))
        );
        self.spawn(move |c| Msg::Reports(id, c.get(&path)));
    }

    /// Change the selected probe's configuration with `f`, and save it.
    pub fn configure_probe(
        &mut self,
        what: &str,
        f: impl FnOnce(&mut Value) -> Result<(), String>,
    ) {
        let Some(p) = self.selected_probe() else {
            return;
        };
        let (id, mut config) = (p.id.clone(), p.config.clone());
        if let Err(e) = f(&mut config) {
            return self.say(e, Level::Error);
        }
        let what = what.to_string();
        self.busy = true;
        self.spawn(move |c| {
            let path = format!("/probes/{}/config", crate::api::encode(&id));
            let r = c.put(&path, &config).and_then(|v| {
                serde_json::from_value(v).map_err(|e| ApiError::from(std::io::Error::other(e)))
            });
            Msg::ProbeChanged(r, what)
        });
    }

    /// Apply a typed setting.
    pub fn set_probe_field(&mut self, field: ProbeField, text: String) {
        let text = text.trim().to_string();
        match field {
            ProbeField::Label => self.configure_probe("label saved", |c| {
                c["label"] = if text.is_empty() {
                    Value::Null
                } else {
                    text.into()
                };
                Ok(())
            }),
            ProbeField::Groups => self.configure_probe("groups saved", |c| {
                let groups: Vec<&str> = text.split([' ', ',']).filter(|g| !g.is_empty()).collect();
                if let Some(bad) = groups.iter().find(|g| !crate::model::is_multicast(g)) {
                    return Err(format!("{bad} isn't a multicast group and port"));
                }
                c["groups"] = json!(groups);
                Ok(())
            }),
            ProbeField::Interval => self.configure_probe("report interval saved", |c| {
                let s: f64 = text
                    .parse()
                    .map_err(|_| "that isn't a number of seconds".to_string())?;
                c["report_interval_s"] = json!(s);
                Ok(())
            }),
        }
    }

    pub fn toggle_probe(&mut self, key: &'static str, on: &'static str, off: &'static str) {
        let now = self.selected_probe().is_some_and(|p| p.flag(key));
        self.configure_probe(if now { off } else { on }, |c| {
            c[key] = json!(!now);
            Ok(())
        });
    }

    pub fn reset_probe(&mut self) {
        let Some(id) = self.selected_probe().map(|p| p.id.clone()) else {
            return;
        };
        self.busy = true;
        self.spawn(move |c| {
            let path = format!("/probes/{}/config", crate::api::encode(&id));
            let r = c.request_delete_json(&path).and_then(|v| {
                serde_json::from_value(v).map_err(|e| ApiError::from(std::io::Error::other(e)))
            });
            Msg::ProbeChanged(r, "back to the defaults".into())
        });
    }

    pub fn forget_probe(&mut self, id: String) {
        self.spawn(move |c| {
            let path = format!("/probes/{}", crate::api::encode(&id));
            Msg::Done(
                c.delete(&path).map(|_| format!("Forgot {id}")),
                Some("probes"),
            )
        });
    }

    // ---- actions ----

    pub fn submit_page(&mut self) {
        match self.form.request() {
            Ok(body) => {
                self.busy = true;
                self.say("Starting page…", Level::Info);
                self.spawn(move |c| Msg::Started(c.post("/senders", &body)));
            }
            Err(e) => self.say(e, Level::Error),
        }
    }

    pub fn stop(&mut self, id: u64) {
        self.spawn(move |c| {
            Msg::Done(
                c.delete(&format!("/jobs/{id}"))
                    .map(|_| format!("Stopped job {id}")),
                Some("jobs"),
            )
        });
    }

    pub fn listen(&mut self, s: &Stream) {
        let Some(a) = s.audio.clone().filter(|a| a.playable) else {
            return self.say("can't decode this stream", Level::Error);
        };
        let output = (!self.output.is_empty()).then(|| self.output.clone());
        let body = json!({
            "group": a.group, "encoding": a.encoding, "rate": a.sample_rate, "channels": a.channels,
            "sink": {"type": "play", "device": output},
        });
        self.busy = true;
        self.spawn(move |c| Msg::Started(c.post("/receivers", &body)));
    }

    /// Cycle the output device used by Listen.
    pub fn next_output(&mut self) {
        let mut outs: Vec<String> = vec![String::new()];
        outs.extend(
            self.devices
                .iter()
                .filter(|d| d.kind == "output")
                .map(|d| d.name.clone()),
        );
        let i = outs.iter().position(|o| *o == self.output).unwrap_or(0);
        self.output = outs[(i + 1) % outs.len()].clone();
    }

    pub fn upload(&mut self, path: String) {
        let path = std::path::PathBuf::from(path.trim());
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_start_matches('.').to_string())
        else {
            return self.say("that isn't a file path", Level::Error);
        };
        self.busy = true;
        self.say(format!("Uploading {name}…"), Level::Info);
        self.spawn(move |c| {
            Msg::Done(
                c.upload(&name, &path).map(|_| format!("Uploaded {name}")),
                Some("media"),
            )
        });
    }

    pub fn delete_media(&mut self, name: String) {
        self.spawn(move |c| {
            let path = format!("/media/{}", crate::api::encode(&name));
            Msg::Done(
                c.delete(&path).map(|_| format!("Deleted {name}")),
                Some("media"),
            )
        });
    }
}
