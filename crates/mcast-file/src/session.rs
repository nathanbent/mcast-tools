//! SDP for file streams.

use crate::wire::{CLOCK_RATE, ENCODING_NAME};
use mcast_core::random_u32;
use mcast_core::sdp::{Attribute, Connection, Media, Origin, SessionDescription};
use std::fmt;
use std::net::{IpAddr, SocketAddr};

/// A file stream as described by SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub name: String,
    pub group: SocketAddr,
    pub payload_type: u8,
}

impl FileInfo {
    pub fn from_sdp(sdp: &SessionDescription) -> Option<Self> {
        let media = sdp
            .media
            .iter()
            .find(|m| m.kind == "application" && m.protocol.starts_with("RTP/"))?;
        let addr = sdp.connection_for(media)?.address;
        let payload_type = media
            .formats
            .iter()
            .filter_map(|f| f.parse().ok())
            .find(|&pt| {
                media
                    .rtpmap(pt)
                    .is_some_and(|m| m.encoding.eq_ignore_ascii_case(ENCODING_NAME))
            })?;
        Some(Self {
            name: sdp.name.clone(),
            group: SocketAddr::new(addr, media.port),
            payload_type,
        })
    }
}

impl fmt::Display for FileInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\" {} files", self.name, self.group)
    }
}

pub fn describe(
    name: &str,
    origin: IpAddr,
    group: SocketAddr,
    ttl: u32,
    payload_type: u8,
) -> SessionDescription {
    let id = random_u32().to_string();
    SessionDescription {
        origin: Origin {
            username: "-".into(),
            session_id: id.clone(),
            version: id,
            address: origin.to_string(),
        },
        name: name.into(),
        info: Some("mcast-tools files (RaptorQ)".into()),
        connection: Some(Connection {
            address: group.ip(),
            ttl: group.is_ipv4().then_some(ttl.min(255) as u8),
        }),
        attributes: vec![],
        media: vec![Media {
            kind: "application".into(),
            port: group.port(),
            protocol: "RTP/AVP".into(),
            formats: vec![payload_type.to_string()],
            connection: None,
            attributes: vec![
                Attribute {
                    name: "recvonly".into(),
                    value: None,
                },
                Attribute {
                    name: "rtpmap".into(),
                    value: Some(format!("{payload_type} {ENCODING_NAME}/{CLOCK_RATE}")),
                },
            ],
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_then_read_back() {
        let group: SocketAddr = "239.69.2.1:5004".parse().unwrap();
        let sdp = describe("Photos", "10.0.0.5".parse().unwrap(), group, 16, 110);
        let parsed = mcast_core::sdp::parse(&sdp.to_string()).unwrap();
        assert_eq!(
            FileInfo::from_sdp(&parsed),
            Some(FileInfo {
                name: "Photos".into(),
                group,
                payload_type: 110
            })
        );
    }
}
