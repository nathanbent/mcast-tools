//! The mcast server, as far as the panel needs it. [`HttpBackend`] talks to
//! `mcast serve` over its HTTP API; tests use a fake.

use serde::Deserialize;
use serde_json::Value;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct BackendError {
    /// HTTP status, or 0 when the server couldn't be reached.
    pub status: u16,
    pub message: String,
    /// For 409: the job already sending to the group.
    pub job: Option<u64>,
}

impl BackendError {
    pub fn is_conflict(&self) -> bool {
        self.status == 409
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BackendError {}

pub type BackendResult<T> = Result<T, BackendError>;

/// A job on the server: the parts of the API's job object the panel reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Job {
    pub id: u64,
    /// `starting`, `running`, `stopping`, `finished`, `stopped` or `failed`.
    pub state: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Job {
    /// Whether the job has ended.
    pub fn is_over(&self) -> bool {
        matches!(self.state.as_str(), "finished" | "stopped" | "failed")
    }
}

/// A phonebook entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Entry {
    pub name: String,
    pub group: String,
}

/// What the event stream reports.
#[derive(Debug, Clone)]
pub enum Feed {
    /// (Re)connected, or events were missed: anything may have changed.
    Connected,
    Disconnected(String),
    JobEnded(Job),
}

/// Callback for [`Backend::follow`]; return false to stop following.
pub type FeedFn = Box<dyn Fn(Feed) -> bool + Send + Sync + 'static>;

pub trait Backend: Send + Sync + 'static {
    fn phonebook(&self) -> BackendResult<Vec<Entry>>;
    /// Names of the files in the server's media directory.
    fn media(&self) -> BackendResult<Vec<String>>;
    /// Start a sender from a send request (the API's JSON).
    fn start_sender(&self, request: &Value) -> BackendResult<Job>;
    /// Stop a job; returns once it has wound down (or the server gave up waiting).
    fn stop_job(&self, id: u64) -> BackendResult<()>;
    fn job(&self, id: u64) -> BackendResult<Job>;
    /// Follow the server's events in the background until `on` returns false.
    fn follow(&self, on: FeedFn);
}

/// `mcast serve`'s HTTP API.
#[derive(Clone)]
pub struct HttpBackend {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl From<ureq::Error> for BackendError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(status, resp) => {
                let body: Value = resp.into_json().unwrap_or(Value::Null);
                BackendError {
                    status,
                    message: body["error"]
                        .as_str()
                        .map_or_else(|| format!("HTTP {status}"), str::to_string),
                    job: body["job"].as_u64(),
                }
            }
            ureq::Error::Transport(t) => BackendError {
                status: 0,
                message: format!("can't reach the mcast server: {t}"),
                job: None,
            },
        }
    }
}

impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        BackendError {
            status: 0,
            message: e.to_string(),
            job: None,
        }
    }
}

impl HttpBackend {
    /// `base` is the server, e.g. "http://127.0.0.1:8650".
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            // Starting a page measures its file's loudness, and stopping
            // waits for the job, so allow more than a typical API call.
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(30))
                .build(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, agent: &ureq::Agent, method: &str, path: &str) -> ureq::Request {
        let r = agent.request(method, &format!("{}/api/v1{path}", self.base));
        match &self.token {
            Some(t) => r.set("Authorization", &format!("Bearer {t}")),
            None => r,
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> BackendResult<T> {
        Ok(self.request(&self.agent, "GET", path).call()?.into_json()?)
    }
}

impl Backend for HttpBackend {
    fn phonebook(&self) -> BackendResult<Vec<Entry>> {
        self.get("/phonebook")
    }

    fn media(&self) -> BackendResult<Vec<String>> {
        #[derive(Deserialize)]
        struct File {
            name: String,
        }
        let files: Vec<File> = self.get("/media")?;
        Ok(files.into_iter().map(|f| f.name).collect())
    }

    fn start_sender(&self, request: &Value) -> BackendResult<Job> {
        Ok(self
            .request(&self.agent, "POST", "/senders")
            .send_json(request)?
            .into_json()?)
    }

    fn stop_job(&self, id: u64) -> BackendResult<()> {
        self.request(&self.agent, "DELETE", &format!("/jobs/{id}"))
            .call()?;
        Ok(())
    }

    fn job(&self, id: u64) -> BackendResult<Job> {
        self.get(&format!("/jobs/{id}"))
    }

    fn follow(&self, on: FeedFn) {
        let me = self.clone();
        // The server sends a keep-alive every 15 s, so a quiet minute means it's gone.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(60))
            .build();
        std::thread::spawn(move || loop {
            match me.request(&agent, "GET", "/events").call() {
                Ok(resp) => {
                    if !on(Feed::Connected) {
                        return;
                    }
                    let mut parser = SseParser::default();
                    for line in BufReader::new(resp.into_reader()).lines() {
                        let Ok(line) = line else { break };
                        let Some(data) = parser.feed(&line) else {
                            continue;
                        };
                        let Some(feed) = feed_of(&data) else {
                            continue;
                        };
                        if !on(feed) {
                            return;
                        }
                    }
                    if !on(Feed::Disconnected("event stream ended".into())) {
                        return;
                    }
                }
                Err(e) => {
                    if !on(Feed::Disconnected(BackendError::from(e).message)) {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        });
    }
}

/// The events the panel cares about.
fn feed_of(data: &str) -> Option<Feed> {
    let v: Value = serde_json::from_str(data).ok()?;
    match v["type"].as_str()? {
        "job_ended" => serde_json::from_value(v["job"].clone())
            .ok()
            .map(Feed::JobEnded),
        // Events were dropped: treat it like a reconnect.
        "lagged" => Some(Feed::Connected),
        _ => None,
    }
}

/// Assembles `data:` lines into events; a blank line ends one.
#[derive(Default)]
struct SseParser {
    data: Vec<String>,
}

impl SseParser {
    fn feed(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            return (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"));
        }
        if let Some(d) = line.strip_prefix("data:") {
            self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_job_ended_events() {
        let mut p = SseParser::default();
        let lines = [
            ":keep-alive",
            "",
            "event: job_updated",
            r#"data: {"type":"job_updated","job":{"id":1,"state":"running"}}"#,
            "",
            "event: job_ended",
            r#"data: {"type":"job_ended","job":{"id":1,"state":"failed","error":"boom","stats":null}}"#,
            "",
            r#"data: {"type":"lagged","missed":3}"#,
            "",
        ];
        let got: Vec<Feed> = lines
            .iter()
            .filter_map(|l| p.feed(l))
            .filter_map(|d| feed_of(&d))
            .collect();
        assert_eq!(got.len(), 2, "{got:?}");
        match &got[0] {
            Feed::JobEnded(j) => {
                assert_eq!(j.id, 1);
                assert!(j.is_over());
                assert_eq!(j.error.as_deref(), Some("boom"));
            }
            f => panic!("{f:?}"),
        }
        assert!(matches!(got[1], Feed::Connected));
    }
}
