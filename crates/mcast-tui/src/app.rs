//! TUI state, and how API results and server events change it. API calls
//! are in `actions.rs`, keys in `keys.rs`, drawing in `ui/`.

use crate::api::{ApiError, Client, Feed};
use crate::form::{FormKind, Forms};
use crate::model::{Device, Job, MediaFile, Probe, Report, Snapshot, Stream, TextLine};
pub use crate::probes::ProbeField;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// Ended jobs listed under the running ones.
const RECENT: usize = 10;
/// Lines kept per text channel.
const MAX_LINES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Jobs,
    Text,
    Streams,
    Monitor,
    Media,
    Probes,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Jobs,
        Tab::Text,
        Tab::Streams,
        Tab::Monitor,
        Tab::Media,
        Tab::Probes,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Jobs => "Now playing",
            Tab::Text => "Text",
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
    /// A form is open.
    Form(FormKind),
    /// Typing a message for the current text channel.
    Compose(String),
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
    /// A text channel's lines so far.
    TextHistory(u64, Result<Vec<TextLine>, ApiError>),
    /// A job was started (or failed to), from this form.
    Started(FormKind, Result<Value, ApiError>),
    /// Some other action finished: the message to show (none if empty),
    /// and what to refetch.
    Done(Result<String, ApiError>, Option<&'static str>),
}

pub struct App {
    pub client: Client,
    pub(crate) tx: Sender<Msg>,
    pub tab: Tab,
    pub mode: Mode,
    pub jobs: BTreeMap<u64, Job>,
    pub streams: Vec<Stream>,
    pub media: Vec<MediaFile>,
    pub devices: Vec<Device>,
    /// `None` until loaded; `Some(None)` when the server's monitor is off.
    pub monitor: Option<Option<Snapshot>>,
    /// Lines per text channel, oldest first.
    pub texts: HashMap<u64, Vec<TextLine>>,
    /// The text channel shown on the Text tab.
    pub channel: Option<u64>,
    /// Sorted by ID.
    pub probes: Vec<Probe>,
    /// The selected probe's reports (its ID, oldest first).
    pub reports: Option<(String, Vec<Report>)>,
    /// A probe whose reports are being fetched.
    pub(crate) reports_loading: Option<String>,
    /// Selected row per tab.
    pub selected: [usize; 6],
    pub forms: Forms,
    /// Output device for Listen ("" = default).
    pub output: String,
    pub status: Option<(String, Level, Instant)>,
    pub connection: Result<(), String>,
    /// The job that refused the last start (409), offered for stopping.
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
            texts: HashMap::new(),
            channel: None,
            probes: Vec::new(),
            reports: None,
            reports_loading: None,
            selected: [0; 6],
            forms: Forms::default(),
            output: String::new(),
            status: None,
            connection: Err("connecting".into()),
            conflict: None,
            busy: false,
            quit: false,
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

    /// Open text channels, newest first.
    pub fn channels(&self) -> Vec<&Job> {
        self.jobs
            .values()
            .rev()
            .filter(|j| j.kind == "text" && j.active())
            .collect()
    }

    /// The channel shown: the chosen one while it's open, else the newest.
    pub fn current_channel(&self) -> Option<&Job> {
        let open = self.channels();
        self.channel
            .and_then(|id| open.iter().find(|j| j.id == id).copied())
            .or_else(|| open.first().copied())
    }

    /// Number of rows on the current tab.
    pub fn rows(&self) -> usize {
        match self.tab {
            Tab::Jobs => self.job_rows().len(),
            Tab::Text => 0,
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

    fn sync_forms(&mut self) {
        let media: Vec<String> = self.media.iter().map(|m| m.name.clone()).collect();
        let inputs: Vec<String> = self
            .devices
            .iter()
            .filter(|d| d.kind == "input")
            .map(|d| d.name.clone())
            .collect();
        self.forms.set_options(&media, &inputs);
    }

    fn add_job(&mut self, j: Job) {
        // A text channel we haven't heard yet: fetch what was said so far.
        if j.kind == "text" && !self.texts.contains_key(&j.id) {
            self.texts.insert(j.id, Vec::new());
            self.load_text(j.id);
        }
        self.jobs.insert(j.id, j);
    }

    fn add_line(&mut self, line: TextLine) {
        let lines = self.texts.entry(line.job).or_default();
        lines.push(line);
        if lines.len() > MAX_LINES {
            lines.drain(..lines.len() - MAX_LINES);
        }
    }

    pub fn handle(&mut self, msg: Msg) {
        let fail = |app: &mut App, e: ApiError| app.say(e.message, Level::Error);
        match msg {
            Msg::Feed(Feed::Connected) => {
                self.connection = Ok(());
                // Events may have been missed while disconnected.
                self.texts.clear();
                self.refresh_all();
            }
            Msg::Feed(Feed::Disconnected(why)) => self.connection = Err(why),
            Msg::Feed(Feed::Event(e)) => self.event(e),
            Msg::Jobs(r) => match r {
                Ok(jobs) => {
                    self.jobs.clear();
                    for j in jobs {
                        self.add_job(j);
                    }
                }
                Err(e) => fail(self, e),
            },
            Msg::Streams(r) => match r {
                Ok(s) => self.streams = s,
                Err(e) => fail(self, e),
            },
            Msg::Media(r) => match r {
                Ok(m) => {
                    self.media = m;
                    self.sync_forms();
                }
                Err(e) => fail(self, e),
            },
            Msg::Devices(r) => match r {
                Ok(d) => {
                    self.devices = d;
                    self.sync_forms();
                }
                Err(e) => fail(self, e),
            },
            m @ (Msg::Probes(_) | Msg::Reports(..) | Msg::ProbeChanged(..)) => self.probe_msg(m),
            Msg::Monitor(r) => match r {
                Ok(m) => self.monitor = Some(Some(m)),
                Err(e) if e.status == 404 => self.monitor = Some(None),
                Err(e) => fail(self, e),
            },
            Msg::TextHistory(id, r) => {
                if let Ok(lines) = r {
                    // Lines that arrived meanwhile came after these.
                    let newer = self.texts.remove(&id).unwrap_or_default();
                    let last = lines.last().map_or(0, |l| l.at_ms);
                    let mut all = lines;
                    all.extend(newer.into_iter().filter(|l| l.at_ms > last));
                    self.texts.insert(id, all);
                }
            }
            Msg::Started(kind, r) => self.started(kind, r),
            Msg::Done(r, then) => {
                self.busy = false;
                match r {
                    Ok(text) if text.is_empty() => {}
                    Ok(text) => self.say(text, Level::Ok),
                    Err(e) => fail(self, e),
                }
                if let Some(w) = then {
                    self.refresh(w);
                }
            }
        }
    }

    fn started(&mut self, kind: FormKind, r: Result<Value, ApiError>) {
        self.busy = false;
        match r {
            Ok(job) => {
                let Ok(j) = serde_json::from_value::<Job>(job) else {
                    return;
                };
                self.say(format!("{} started", j.title()), Level::Ok);
                self.conflict = None;
                self.mode = Mode::Normal;
                if j.kind == "text" {
                    self.channel = Some(j.id);
                    self.tab = Tab::Text;
                } else {
                    self.tab = Tab::Jobs;
                    self.selected[Tab::Jobs.index()] = 0;
                }
                self.add_job(j);
            }
            Err(e) if e.status == 409 => {
                // Show what's in the way; the form keeps its values.
                self.conflict = e.job;
                self.mode = Mode::Normal;
                self.tab = Tab::Jobs;
                let again = match kind {
                    FormKind::Page => "p to page again",
                    FormKind::Text => "j to join again",
                    FormKind::SendFiles => "f to send again",
                    FormKind::ReceiveFiles => "r to receive again",
                };
                self.say(
                    format!("{} · x stops it, then {again}", e.message),
                    Level::Error,
                );
            }
            Err(e) => self.say(e.message, Level::Error),
        }
    }

    fn event(&mut self, e: Value) {
        if self.probe_event(&e) {
            return;
        }
        match e["type"].as_str().unwrap_or("") {
            "job_started" | "job_updated" | "job_ended" => {
                if let Ok(j) = serde_json::from_value::<Job>(e["job"].clone()) {
                    self.add_job(j);
                }
            }
            "stream_added" | "stream_changed" | "stream_removed" => self.refresh("streams"),
            "monitor" => {
                if let Ok(s) = serde_json::from_value(e["snapshot"].clone()) {
                    self.monitor = Some(Some(s));
                }
            }
            "text_line" => {
                if let Ok(l) = serde_json::from_value(e["line"].clone()) {
                    self.add_line(l);
                }
            }
            "file_received" => {
                let f = &e["file"];
                let name = f["name"].as_str().unwrap_or("a file");
                let bytes = f["bytes"].as_u64().unwrap_or(0);
                self.say(
                    format!("Received {name} ({})", crate::model::bytes(bytes)),
                    Level::Ok,
                );
                self.refresh("media");
            }
            "lagged" => self.refresh_all(),
            _ => {}
        }
    }
}
