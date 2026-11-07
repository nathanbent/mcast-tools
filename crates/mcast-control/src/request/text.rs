//! Real-time text (T.140): a channel to chat on, or just listen to.

use super::{hostname, opt_str, NetRequest};
use anyhow::{bail, Result};
use mcast_core::rtcp::RtcpConfig;
use mcast_core::Interface;
use mcast_text::receiver::TextReceiverConfig;
use mcast_text::sender::{self, TextSenderConfig};
use mcast_text::session::TextInfo;
use mcast_text::t140::{DEFAULT_RED_PT, DEFAULT_T140_PT};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

pub const DEFAULT_TEXT_GROUP: &str = "239.69.1.1:5004";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextRequest {
    /// Group to chat on. With neither this nor `stream`, the default text group.
    pub group: Option<SocketAddr>,
    /// Join the announced text stream whose name contains this.
    pub stream: Option<String>,
    #[serde(with = "opt_str")]
    pub interface: Option<Interface>,
    pub ttl: u32,
    pub loopback: bool,
    pub tos: Option<u32>,
    /// Our name, shown to the others (RTCP); `None` is the host name.
    pub name: Option<String>,
    /// Send as well as receive; `false` only listens.
    pub send: bool,
    /// Announce our text with SAP under `name`.
    pub sap: bool,
    pub sap_group: Option<SocketAddr>,
    pub sap_interval_s: f64,
    /// Send each packet's text again in the next two (RFC 2198).
    pub redundancy: bool,
    pub t140_pt: u8,
    pub red_pt: u8,
    pub rtcp: bool,
    pub rtcp_interval_s: f64,
}

impl Default for TextRequest {
    fn default() -> Self {
        Self {
            group: None,
            stream: None,
            interface: None,
            ttl: 16,
            loopback: true,
            tos: None,
            name: None,
            send: true,
            sap: false,
            sap_group: None,
            sap_interval_s: 5.0,
            redundancy: true,
            t140_pt: DEFAULT_T140_PT,
            red_pt: DEFAULT_RED_PT,
            rtcp: true,
            rtcp_interval_s: 5.0,
        }
    }
}

/// Where a text request settled.
#[derive(Debug, Clone, PartialEq)]
pub struct TextTarget {
    pub group: SocketAddr,
    pub t140_pt: u8,
    pub red_pt: Option<u8>,
    /// The announcement it came from.
    pub announced: Option<String>,
}

impl TextRequest {
    pub fn name(&self) -> String {
        self.name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(hostname)
    }

    fn net(&self) -> NetRequest {
        NetRequest {
            interface: self.interface,
            ttl: self.ttl,
            loopback: self.loopback,
        }
    }

    fn rtcp(&self, name: Option<String>) -> Option<RtcpConfig> {
        self.rtcp.then(|| RtcpConfig {
            name,
            interval: Duration::from_secs_f64(self.rtcp_interval_s.max(0.1)),
        })
    }

    /// The group and payload types: `group`, else the announced stream
    /// named by `stream` (from `known`), else the default text group.
    pub fn target(&self, known: &[TextInfo]) -> Result<TextTarget> {
        if let Some(group) = self.group {
            return Ok(TextTarget {
                group,
                t140_pt: self.t140_pt,
                red_pt: Some(self.red_pt),
                announced: None,
            });
        }
        if let Some(wanted) = &self.stream {
            let w = wanted.to_lowercase();
            return match known.iter().find(|i| i.name.to_lowercase().contains(&w)) {
                Some(i) => Ok(TextTarget {
                    group: i.group,
                    t140_pt: i.t140_pt,
                    red_pt: i.red_pt,
                    announced: Some(i.name.clone()),
                }),
                None => bail!("no text stream named \"{wanted}\" is announced"),
            };
        }
        Ok(TextTarget {
            group: DEFAULT_TEXT_GROUP.parse().expect("valid default"),
            t140_pt: self.t140_pt,
            red_pt: Some(self.red_pt),
            announced: None,
        })
    }

    pub fn sender_config(&self, t: &TextTarget, iface: Interface) -> Result<TextSenderConfig> {
        let mut net = self.net().options(t.group, iface)?;
        net.tos = self.tos;
        let name = self.name();
        Ok(TextSenderConfig {
            net,
            t140_pt: t.t140_pt,
            red: t.red_pt.filter(|_| self.redundancy).map(|pt| (pt, 2)),
            interval: sender::DEFAULT_INTERVAL,
            announce: self.sap.then(|| sender::Announce {
                name: name.clone(),
                sap_group: self.sap_group,
                interval: Duration::from_secs_f64(self.sap_interval_s.max(0.5)),
            }),
            rtcp: self.rtcp(Some(name)),
        })
    }

    /// `ignore_ssrc` is our own sender, so our lines don't come back.
    pub fn receiver_config(
        &self,
        t: &TextTarget,
        iface: Interface,
        ignore_ssrc: Option<u32>,
    ) -> Result<TextReceiverConfig> {
        Ok(TextReceiverConfig {
            net: self.net().options(t.group, iface)?,
            t140_pt: t.t140_pt,
            red_pt: t.red_pt,
            ignore_ssrc,
            // When sending, our sender already reports for us; just hear names.
            rtcp: self.rtcp((!self.send).then(|| self.name())),
            send_reports: !self.send,
            duration: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_text_group() {
        let r: TextRequest = serde_json::from_str(r#"{"name": "desk"}"#).unwrap();
        let t = r.target(&[]).unwrap();
        assert_eq!(t.group, DEFAULT_TEXT_GROUP.parse().unwrap());
        assert_eq!(t.red_pt, Some(DEFAULT_RED_PT));
        let s = r.sender_config(&t, Interface::Any).unwrap();
        assert_eq!(s.red, Some((DEFAULT_RED_PT, 2)));
        assert!(s.announce.is_none(), "chat isn't announced by default");
        assert_eq!(s.rtcp.unwrap().name.as_deref(), Some("desk"));
    }

    #[test]
    fn joins_announced_streams_by_name() {
        let known = [TextInfo {
            name: "Front desk".into(),
            group: "239.69.1.9:5004".parse().unwrap(),
            t140_pt: 98,
            red_pt: None,
        }];
        let r = TextRequest {
            stream: Some("desk".into()),
            ..TextRequest::default()
        };
        let t = r.target(&known).unwrap();
        assert_eq!((t.group, t.red_pt), (known[0].group, None));
        let r = TextRequest {
            stream: Some("lobby".into()),
            ..TextRequest::default()
        };
        assert!(r.target(&known).is_err());
        assert!(serde_json::from_str::<TextRequest>(r#"{"grop": 1}"#).is_err());
    }
}
