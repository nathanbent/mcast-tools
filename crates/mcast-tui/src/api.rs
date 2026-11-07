//! Blocking client for the mcast API, plus a thread that follows its
//! server-sent events. The only module that talks to the server.

use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ApiError {
    /// HTTP status, or 0 when the server couldn't be reached.
    pub status: u16,
    pub message: String,
    /// For 409: the job already sending to the group.
    pub job: Option<u64>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<ureq::Error> for ApiError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(status, resp) => {
                let body: Value = resp.into_json().unwrap_or(Value::Null);
                ApiError {
                    status,
                    message: body["error"]
                        .as_str()
                        .map_or_else(|| format!("HTTP {status}"), str::to_string),
                    job: body["job"].as_u64(),
                }
            }
            ureq::Error::Transport(t) => ApiError {
                status: 0,
                message: format!("can't reach the server: {t}"),
                job: None,
            },
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError {
            status: 0,
            message: e.to_string(),
            job: None,
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl Client {
    /// `base` is the server, e.g. "http://127.0.0.1:8650".
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            // Starting a page measures its file and finding a stream can
            // wait for SAP, so allow more than a typical API call.
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

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        Ok(self.request(&self.agent, "GET", path).call()?.into_json()?)
    }

    pub fn post(&self, path: &str, body: &Value) -> ApiResult<Value> {
        Ok(self
            .request(&self.agent, "POST", path)
            .send_json(body)?
            .into_json()?)
    }

    pub fn delete(&self, path: &str) -> ApiResult<()> {
        self.request(&self.agent, "DELETE", path).call()?;
        Ok(())
    }

    /// Upload a local file to the server's media directory.
    pub fn upload(&self, name: &str, file: &Path) -> ApiResult<()> {
        let f = std::fs::File::open(file)?;
        let len = f.metadata()?.len();
        self.request(&self.agent, "PUT", &format!("/media/{}", encode(name)))
            .set("Content-Length", &len.to_string())
            .send(f)?;
        Ok(())
    }

    /// Follow the event stream in a background thread, reconnecting as
    /// needed, until `on` returns false.
    pub fn follow(&self, on: impl Fn(Feed) -> bool + Send + 'static) {
        let client = self.clone();
        // The server sends a keep-alive every 15 s, so a quiet minute means it's gone.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(60))
            .build();
        std::thread::spawn(move || loop {
            let result = client.request(&agent, "GET", "/events").call();
            match result {
                Ok(resp) => {
                    if !on(Feed::Connected) {
                        return;
                    }
                    let mut parser = SseParser::default();
                    for line in BufReader::new(resp.into_reader()).lines() {
                        let Ok(line) = line else { break };
                        if let Some(data) = parser.feed(&line) {
                            if let Ok(v) = serde_json::from_str(&data) {
                                if !on(Feed::Event(v)) {
                                    return;
                                }
                            }
                        }
                    }
                    if !on(Feed::Disconnected("event stream ended".into())) {
                        return;
                    }
                }
                Err(e) => {
                    if !on(Feed::Disconnected(ApiError::from(e).message)) {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        });
    }
}

/// What the event stream reports.
#[derive(Debug)]
pub enum Feed {
    /// (Re)connected: lists may be stale, so refetch them.
    Connected,
    Disconnected(String),
    Event(Value),
}

/// Assembles `data:` lines into events; a blank line ends one.
#[derive(Default)]
pub struct SseParser {
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, line: &str) -> Option<String> {
        if line.is_empty() {
            return (!self.data.is_empty()).then(|| std::mem::take(&mut self.data).join("\n"));
        }
        if let Some(d) = line.strip_prefix("data:") {
            self.data.push(d.strip_prefix(' ').unwrap_or(d).to_string());
        }
        // `event:` names repeat the JSON's "type"; comments (":") are keep-alives.
        None
    }
}

/// Percent-encode a path segment.
pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_sent_events() {
        let mut p = SseParser::default();
        let lines = [
            ":keep-alive",
            "",
            "event: monitor",
            "data: {\"type\":\"monitor\"}",
            "",
            "data: a",
            "data: b",
            "",
        ];
        let got: Vec<String> = lines.iter().filter_map(|l| p.feed(l)).collect();
        assert_eq!(got, ["{\"type\":\"monitor\"}", "a\nb"]);
    }

    #[test]
    fn encodes_names() {
        assert_eq!(encode("fire drill #2.wav"), "fire%20drill%20%232.wav");
    }
}
