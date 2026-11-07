//! Blocking client for the probe routes of the mcast API.

use mcast_control::probes::{CheckIn, CheckInReply, Hello, IntervalReport};
use mcast_control::{ProbeConfig, ProbeInfo};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ClientError {
    /// HTTP status, or 0 when the server couldn't be reached.
    pub status: u16,
    pub message: String,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

impl From<ureq::Error> for ClientError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(status, resp) => {
                let body: serde_json::Value = resp.into_json().unwrap_or_default();
                let detail = body["error"]
                    .as_str()
                    .map_or_else(|| format!("HTTP {status}"), str::to_string);
                ClientError {
                    status,
                    message: format!("the server said: {detail}"),
                }
            }
            ureq::Error::Transport(t) => ClientError {
                status: 0,
                message: format!("can't reach the server: {t}"),
            },
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError {
            status: 0,
            message: format!("bad reply from the server: {e}"),
        }
    }
}

pub type ClientResult<T> = Result<T, ClientError>;

#[derive(Clone)]
pub struct Client {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
}

impl Client {
    /// `base` is the server, e.g. "http://10.0.0.5:8650".
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout(Duration::from_secs(15))
                .build(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let r = self
            .agent
            .request(method, &format!("{}/api/v1{path}", self.base));
        match &self.token {
            Some(t) => r.set("Authorization", &format!("Bearer {t}")),
            None => r,
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> ClientResult<T> {
        Ok(self.request("GET", path).call()?.into_json()?)
    }

    fn send<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: &impl Serialize,
    ) -> ClientResult<T> {
        Ok(self.request(method, path).send_json(body)?.into_json()?)
    }

    pub fn hello(&self, nonce: &str) -> ClientResult<Hello> {
        self.get(&format!("/probe-hello?nonce={nonce}"))
    }

    pub fn check_in(&self, id: &str, c: &CheckIn) -> ClientResult<CheckInReply> {
        self.send("POST", &format!("/probes/{id}/checkin"), c)
    }

    pub fn probes(&self) -> ClientResult<Vec<ProbeInfo>> {
        self.get("/probes")
    }

    pub fn probe(&self, id: &str) -> ClientResult<ProbeInfo> {
        self.get(&format!("/probes/{id}"))
    }

    pub fn reports(&self, id: &str) -> ClientResult<Vec<IntervalReport>> {
        self.get(&format!("/probes/{id}/reports"))
    }

    pub fn set_config(&self, id: &str, c: &ProbeConfig) -> ClientResult<ProbeInfo> {
        self.send("PUT", &format!("/probes/{id}/config"), c)
    }

    pub fn reset_config(&self, id: &str) -> ClientResult<ProbeInfo> {
        Ok(self
            .request("DELETE", &format!("/probes/{id}/config"))
            .call()?
            .into_json()?)
    }

    pub fn remove(&self, id: &str) -> ClientResult<()> {
        self.request("DELETE", &format!("/probes/{id}")).call()?;
        Ok(())
    }

    pub fn defaults(&self) -> ClientResult<ProbeConfig> {
        self.get("/probe-defaults")
    }

    pub fn set_defaults(&self, c: &ProbeConfig) -> ClientResult<ProbeConfig> {
        self.send("PUT", "/probe-defaults", c)
    }
}
