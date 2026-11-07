//! TUI state, and how API results and server events change it. Keys are
//! handled in `keys.rs`, drawing in `ui/`.

use crate::api::{ApiError, Client, Feed};
use crate::form::PageForm;
use crate::model::{Device, Job, MediaFile, Snapshot, Stream};
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
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Jobs, Tab::Streams, Tab::Monitor, Tab::Media];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Jobs => "Now playing",
            Tab::Streams => "Streams",
            Tab::Monitor => "Monitor",
            Tab::Media => "Media",
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
    Help,
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
    /// Selected row per tab.
    pub selected: [usize; 4],
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
            selected: [0; 4],
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
            _ => {}
        }
    }

    pub fn refresh_all(&self) {
        for w in ["jobs", "streams", "media", "devices", "monitor"] {
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
            "lagged" => self.refresh_all(),
            _ => {}
        }
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
