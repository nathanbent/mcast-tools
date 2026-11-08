//! The mcast server, as far as the panel needs it. [`HttpBackend`] talks to
//! `mcast serve` over its HTTP API; tests use a fake.

use serde::{Deserialize, Serialize};
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
    /// The RTP SSRC it sends with (servers from 0.6). Listeners name it.
    #[serde(default)]
    pub ssrc: Option<u32>,
}

impl Job {
    /// Whether the job has ended.
    pub fn is_over(&self) -> bool {
        matches!(self.state.as_str(), "finished" | "stopped" | "failed")
    }
}

/// A receiver heard on a watched group's RTCP port: the parts of the
/// monitor's `listeners` the panel reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    pub group: String,
    /// The name it gives (`mcast receive --name`, else its hostname).
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cname: Option<String>,
    /// SSRC of the stream it's hearing (the last one, once it's over).
    #[serde(default)]
    pub about: Option<u32>,
    #[serde(default)]
    pub loss_percent: Option<f64>,
    #[serde(default)]
    pub last_heard_ms: f64,
    #[serde(default)]
    pub left: bool,
}

impl Listener {
    /// What to call it: its name, else its CNAME.
    pub fn label(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.cname.as_deref())
            .filter(|n| !n.trim().is_empty())
    }
}

/// A phonebook entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub group: String,
    #[serde(default)]
    pub description: String,
}

/// What the event stream reports.
#[derive(Debug, Clone)]
pub enum Feed {
    /// (Re)connected, or events were missed: anything may have changed.
    Connected,
    Disconnected(String),
    JobEnded(Job),
    /// Media files or folders changed on the server.
    MediaChanged,
}

/// A file in the server's media directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaFile {
    /// Its path, folders and all: `bells/lunch.wav`.
    pub name: String,
    pub bytes: u64,
    #[serde(default)]
    pub modified_ms: Option<u64>,
    /// What it holds, if it's audio the server can play: `codec`, `rate`,
    /// `channels`, `duration_s`. (Servers before 0.5.5 don't say.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Value>,
    /// Why the server can't play it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_error: Option<String>,
}

/// Callback for [`Backend::follow`]; return false to stop following.
pub type FeedFn = Box<dyn Fn(Feed) -> bool + Send + Sync + 'static>;

pub trait Backend: Send + Sync + 'static {
    /// Where it is, for people: e.g. the server's URL.
    fn describe(&self) -> String {
        String::new()
    }
    fn phonebook(&self) -> BackendResult<Vec<Entry>>;
    /// The files in the server's media directory.
    fn media_files(&self) -> BackendResult<Vec<MediaFile>>;
    /// Their names (paths).
    fn media(&self) -> BackendResult<Vec<String>> {
        Ok(self.media_files()?.into_iter().map(|f| f.name).collect())
    }
    /// Its folders, empty ones included, by path.
    fn media_folders(&self) -> BackendResult<Vec<String>>;
    /// Upload a sound (replacing one of that name), making its folders. The
    /// server checks it's audio it can play (`422` if not, leaving any old
    /// file), and says what it holds: `{"name", "bytes", "audio"}`.
    fn upload_media(&self, name: &str, data: &[u8]) -> BackendResult<Value>;
    fn delete_media(&self, name: &str) -> BackendResult<()>;
    fn create_folder(&self, name: &str) -> BackendResult<()>;
    /// Delete an empty folder.
    fn delete_folder(&self, name: &str) -> BackendResult<()>;
    /// Rename or move a file or folder; never replaces. True for a folder.
    fn move_media(&self, from: &str, to: &str) -> BackendResult<bool>;
    /// A file's bytes and content type, to listen to.
    fn download_media(&self, name: &str) -> BackendResult<(String, Vec<u8>)>;
    /// The server's built-in chimes, as its API lists them: `name`,
    /// `category`, `description` and `hidden`.
    fn chimes(&self) -> BackendResult<Vec<Value>>;
    /// Hide exactly these built-in chimes from lists; returns the new list.
    fn hide_chimes(&self, hidden: &[String]) -> BackendResult<Vec<Value>>;
    /// A built-in chime as a WAV file, to listen to.
    fn chime_preview(&self, name: &str) -> BackendResult<Vec<u8>>;
    /// Start a sender from a send request (the API's JSON).
    fn start_sender(&self, request: &Value) -> BackendResult<Job>;
    /// Stop a job; returns once it has wound down (or the server gave up waiting).
    fn stop_job(&self, id: u64) -> BackendResult<()>;
    fn job(&self, id: u64) -> BackendResult<Job>;
    /// Receivers the server's monitor hears reporting (404 when it was
    /// started with `--no-monitor`; empty from servers before 0.6).
    fn listeners(&self) -> BackendResult<Vec<Listener>>;
    /// Follow the server's events in the background until `on` returns false.
    fn follow(&self, on: FeedFn);
    /// Stream audio (16-bit little-endian PCM) into a sender with a live
    /// source, chunk by chunk, until `chunks` closes (the page ends) or the
    /// job does. Blocks until then.
    fn stream_live(
        &self,
        job: u64,
        chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> BackendResult<()>;
}

/// A request body read from a channel of chunks, ending when it closes.
struct ChunkReader {
    chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    at: usize,
}

impl std::io::Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.at == self.current.len() {
            match self.chunks.recv() {
                Ok(c) => {
                    self.current = c;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len() - self.at);
        buf[..n].copy_from_slice(&self.current[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
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

    /// Whether a job with a live source has been ended by the server: it's
    /// stopping or over, or is soon. A page whose feed was closed plays what
    /// it has left and its end chime first, so allow it a few seconds.
    fn job_winding_down(&self, job: u64) -> bool {
        let deadline = std::time::Instant::now() + LIVE_WIND_DOWN;
        loop {
            match Backend::job(self, job) {
                Ok(j) if j.state != "starting" && j.state != "running" => return true,
                // Gone from the server's list: long over.
                Err(e) if e.status == 404 => return true,
                Err(e) if e.status == 0 => return false,
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// How long a live page the server ended may take to finish playing.
const LIVE_WIND_DOWN: Duration = Duration::from_secs(10);

impl Backend for HttpBackend {
    fn describe(&self) -> String {
        self.base.clone()
    }

    fn phonebook(&self) -> BackendResult<Vec<Entry>> {
        self.get("/phonebook")
    }

    fn listeners(&self) -> BackendResult<Vec<Listener>> {
        #[derive(Deserialize)]
        struct Snapshot {
            #[serde(default)]
            listeners: Vec<Listener>,
        }
        Ok(self.get::<Snapshot>("/monitor")?.listeners)
    }

    fn media_files(&self) -> BackendResult<Vec<MediaFile>> {
        self.get("/media?audio=1")
    }

    fn media_folders(&self) -> BackendResult<Vec<String>> {
        self.get("/media-folders")
    }

    fn upload_media(&self, name: &str, data: &[u8]) -> BackendResult<Value> {
        let path = format!("/media/{}?audio=1", encode(name));
        Ok(self
            .request(&self.agent, "PUT", &path)
            .send_bytes(data)?
            .into_json()
            .unwrap_or(Value::Null))
    }

    fn delete_media(&self, name: &str) -> BackendResult<()> {
        self.request(&self.agent, "DELETE", &format!("/media/{}", encode(name)))
            .call()?;
        Ok(())
    }

    fn create_folder(&self, name: &str) -> BackendResult<()> {
        self.request(&self.agent, "POST", "/media-folders")
            .send_json(serde_json::json!({ "name": name }))?;
        Ok(())
    }

    fn delete_folder(&self, name: &str) -> BackendResult<()> {
        self.request(
            &self.agent,
            "DELETE",
            &format!("/media-folders/{}", encode(name)),
        )
        .call()?;
        Ok(())
    }

    fn move_media(&self, from: &str, to: &str) -> BackendResult<bool> {
        let v: Value = self
            .request(&self.agent, "POST", "/media-move")
            .send_json(serde_json::json!({ "from": from, "to": to }))?
            .into_json()?;
        Ok(v["folder"].as_bool().unwrap_or(false))
    }

    fn download_media(&self, name: &str) -> BackendResult<(String, Vec<u8>)> {
        use std::io::Read;
        // To listen to in the browser: sounds, not anything enormous.
        const MAX: u64 = 64 << 20;
        let resp = self
            .request(&self.agent, "GET", &format!("/media/{}", encode(name)))
            .call()?;
        let kind = resp.content_type().to_string();
        let mut data = Vec::new();
        resp.into_reader().take(MAX).read_to_end(&mut data)?;
        Ok((kind, data))
    }

    fn chimes(&self) -> BackendResult<Vec<Value>> {
        self.get("/chimes")
    }

    fn hide_chimes(&self, hidden: &[String]) -> BackendResult<Vec<Value>> {
        Ok(self
            .request(&self.agent, "PUT", "/chimes")
            .send_json(serde_json::json!({ "hidden": hidden }))?
            .into_json()?)
    }

    fn chime_preview(&self, name: &str) -> BackendResult<Vec<u8>> {
        use std::io::Read;
        // A chime is a few seconds; a preview is well under this.
        const MAX: u64 = 4 << 20;
        let resp = self
            .request(
                &self.agent,
                "GET",
                &format!("/chimes/{}/preview", encode(name)),
            )
            .call()?;
        let mut wav = Vec::new();
        resp.into_reader().take(MAX).read_to_end(&mut wav)?;
        Ok(wav)
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

    fn stream_live(
        &self,
        job: u64,
        chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> BackendResult<()> {
        // No read timeout: the reply only comes when the page is over.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .build();
        let body = ChunkReader {
            chunks,
            current: Vec::new(),
            at: 0,
        };
        match self
            .request(&agent, "POST", &format!("/jobs/{job}/live"))
            .set("Content-Type", "application/octet-stream")
            .send(body)
        {
            Ok(_) => Ok(()),
            // When the job ends first (stopped, taken over, or no audio for
            // a while), the server answers and hangs up while we're still
            // sending, so its answer shows up as a broken pipe. Tell that
            // apart from a server we really can't reach by asking after the job.
            Err(ureq::Error::Transport(_)) if self.job_winding_down(job) => Ok(()),
            Err(e) => Err(e.into()),
        }
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

/// A path segment, percent-encoded (chime names are plain, but be safe).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The events the panel cares about.
fn feed_of(data: &str) -> Option<Feed> {
    let v: Value = serde_json::from_str(data).ok()?;
    match v["type"].as_str()? {
        "job_ended" => serde_json::from_value(v["job"].clone())
            .ok()
            .map(Feed::JobEnded),
        "media_changed" => Some(Feed::MediaChanged),
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
