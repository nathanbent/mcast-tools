//! Files and pictures (RaptorQ over RTP): send and receive.

use super::{hostname, opt_str, NetRequest};
use anyhow::{bail, Result};
use mcast_core::rtcp::RtcpConfig;
use mcast_core::Interface;
use mcast_file::receiver::FileReceiverConfig;
use mcast_file::sender::{self, FileSenderConfig};
use mcast_file::session::FileInfo;
use mcast_file::wire::DEFAULT_PT;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_FILE_GROUP: &str = "239.69.2.1:5004";

fn default_group() -> SocketAddr {
    DEFAULT_FILE_GROUP.parse().expect("valid default")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileSendRequest {
    pub group: SocketAddr,
    /// Files to send, one after another.
    pub files: Vec<PathBuf>,
    #[serde(with = "opt_str")]
    pub interface: Option<Interface>,
    pub ttl: u32,
    pub loopback: bool,
    pub tos: Option<u32>,
    /// Sending rate in kbit/s; keep it modest on networks that carry voice.
    pub rate_kbps: u32,
    /// Extra repair data per file, as a fraction of its size.
    pub redundancy: f64,
    /// Keep sending the last file until stopped, for late joiners.
    #[serde(rename = "loop")]
    pub looped: bool,
    pub payload_type: u8,
    /// Name announced via SAP; `None` is "<hostname> files".
    pub name: Option<String>,
    pub sap: bool,
    pub sap_group: Option<SocketAddr>,
    pub sap_interval_s: f64,
    pub rtcp: bool,
    pub rtcp_interval_s: f64,
}

impl Default for FileSendRequest {
    fn default() -> Self {
        Self {
            group: default_group(),
            files: Vec::new(),
            interface: None,
            ttl: 16,
            loopback: true,
            tos: None,
            rate_kbps: 2000,
            redundancy: 0.5,
            looped: false,
            payload_type: DEFAULT_PT,
            name: None,
            sap: true,
            sap_group: None,
            sap_interval_s: 5.0,
            rtcp: true,
            rtcp_interval_s: 5.0,
        }
    }
}

fn rtcp(on: bool, interval_s: f64, name: Option<String>) -> Option<RtcpConfig> {
    on.then(|| RtcpConfig {
        name,
        interval: Duration::from_secs_f64(interval_s.max(0.1)),
    })
}

impl FileSendRequest {
    pub fn name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("{} files", hostname()))
    }

    pub fn to_config(&self, iface: Interface) -> Result<FileSenderConfig> {
        if self.files.is_empty() {
            bail!("no files to send");
        }
        if self.rate_kbps == 0 {
            bail!("the rate must be above 0 kbit/s");
        }
        if !(0.0..=10.0).contains(&self.redundancy) {
            bail!("redundancy must be between 0 and 10");
        }
        let mut net = NetRequest {
            interface: self.interface,
            ttl: self.ttl,
            loopback: self.loopback,
        }
        .options(self.group, iface)?;
        net.tos = self.tos;
        let name = self.name();
        Ok(FileSenderConfig {
            net,
            payload_type: self.payload_type,
            rate_kbps: self.rate_kbps,
            redundancy: self.redundancy,
            looped: false,
            announce: self.sap.then(|| sender::Announce {
                name: name.clone(),
                sap_group: self.sap_group,
                interval: Duration::from_secs_f64(self.sap_interval_s.max(0.5)),
            }),
            rtcp: rtcp(self.rtcp, self.rtcp_interval_s, Some(name)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileRecvRequest {
    /// Group to receive on; otherwise `stream`, else the default file group.
    pub group: Option<SocketAddr>,
    /// Receive from the announced file stream whose name contains this.
    pub stream: Option<String>,
    #[serde(with = "opt_str")]
    pub interface: Option<Interface>,
    pub payload_type: u8,
    /// Refuse files larger than this; `None` is 100 MB.
    pub max_size_mb: Option<u64>,
    /// Our name in RTCP receiver reports; `None` is the host name.
    pub name: Option<String>,
    pub rtcp: bool,
    pub rtcp_interval_s: f64,
    /// Where to save files. Set by the caller, never by clients.
    #[serde(skip)]
    pub out_dir: PathBuf,
}

impl Default for FileRecvRequest {
    fn default() -> Self {
        Self {
            group: None,
            stream: None,
            interface: None,
            payload_type: DEFAULT_PT,
            max_size_mb: None,
            name: None,
            rtcp: true,
            rtcp_interval_s: 5.0,
            out_dir: PathBuf::new(),
        }
    }
}

impl FileRecvRequest {
    pub const DEFAULT_MAX_SIZE_MB: u64 = 100;

    pub fn max_size_mb(&self) -> u64 {
        self.max_size_mb.unwrap_or(Self::DEFAULT_MAX_SIZE_MB)
    }

    /// The group and payload type, and the announcement's name if any.
    pub fn target(&self, known: &[FileInfo]) -> Result<(SocketAddr, u8, Option<String>)> {
        if let Some(g) = self.group {
            let name = known.iter().find(|i| i.group == g).map(|i| i.name.clone());
            return Ok((g, self.payload_type, name));
        }
        if let Some(wanted) = &self.stream {
            let w = wanted.to_lowercase();
            return match known.iter().find(|i| i.name.to_lowercase().contains(&w)) {
                Some(i) => Ok((i.group, i.payload_type, Some(i.name.clone()))),
                None => bail!("no file stream named \"{wanted}\" is announced"),
            };
        }
        Ok((default_group(), self.payload_type, None))
    }

    pub fn to_config(
        &self,
        group: SocketAddr,
        payload_type: u8,
        iface: Interface,
    ) -> Result<FileReceiverConfig> {
        let max_size_mb = self.max_size_mb();
        if max_size_mb == 0 {
            bail!("max_size_mb must be above 0");
        }
        Ok(FileReceiverConfig {
            net: NetRequest {
                interface: self.interface,
                ttl: 16,
                loopback: true,
            }
            .options(group, iface)?,
            payload_type: Some(payload_type),
            max_size: max_size_mb.saturating_mul(1 << 20),
            rtcp: rtcp(
                self.rtcp,
                self.rtcp_interval_s,
                Some(self.name.clone().unwrap_or_else(hostname)),
            ),
            duration: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_request_defaults_and_checks() {
        let r: FileSendRequest =
            serde_json::from_str(r#"{"files": ["map.png"], "loop": true}"#).unwrap();
        assert!(r.looped);
        let c = r.to_config(Interface::Any).unwrap();
        assert_eq!(c.net.group, default_group());
        assert_eq!(c.rate_kbps, 2000);
        assert!(c.announce.is_some());
        assert!(FileSendRequest::default()
            .to_config(Interface::Any)
            .is_err());
    }

    #[test]
    fn clients_cant_choose_where_files_are_saved() {
        assert!(serde_json::from_str::<FileRecvRequest>(r#"{"out_dir": "/etc"}"#).is_err());
        let r: FileRecvRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.out_dir, PathBuf::new());
        let known = [FileInfo {
            name: "Ops pictures".into(),
            group: "239.69.2.7:5004".parse().unwrap(),
            payload_type: 111,
        }];
        let r = FileRecvRequest {
            stream: Some("ops".into()),
            ..FileRecvRequest::default()
        };
        assert_eq!(r.target(&known).unwrap().1, 111);
    }
}
