//! SDP for text streams (RFC 4103 section 4).

use crate::t140::CLOCK_RATE;
use mcast_core::random_u32;
use mcast_core::sdp::{Attribute, Connection, Media, Origin, SessionDescription};
use std::fmt;
use std::net::{IpAddr, SocketAddr};

/// A text stream as described by SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextInfo {
    pub name: String,
    pub group: SocketAddr,
    pub t140_pt: u8,
    /// Payload type of the redundant format, if offered.
    pub red_pt: Option<u8>,
}

impl TextInfo {
    /// Describe the first T.140 text stream in `sdp`.
    pub fn from_sdp(sdp: &SessionDescription) -> Option<Self> {
        let media = sdp
            .media
            .iter()
            .find(|m| m.kind == "text" && m.protocol.starts_with("RTP/"))?;
        let addr = sdp.connection_for(media)?.address;
        let pts: Vec<u8> = media
            .formats
            .iter()
            .filter_map(|f| f.parse().ok())
            .collect();
        let named = |enc: &str| {
            pts.iter().copied().find(|&pt| {
                media
                    .rtpmap(pt)
                    .is_some_and(|m| m.encoding.eq_ignore_ascii_case(enc))
            })
        };
        Some(Self {
            name: sdp.name.clone(),
            group: SocketAddr::new(addr, media.port),
            t140_pt: named("t140")?,
            red_pt: named("red"),
        })
    }
}

impl fmt::Display for TextInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\" {} text (T.140", self.name, self.group)?;
        if self.red_pt.is_some() {
            write!(f, ", redundancy")?;
        }
        write!(f, ")")
    }
}

/// Build an SDP for a text stream we're sending.
pub fn describe(
    name: &str,
    origin: IpAddr,
    group: SocketAddr,
    ttl: u32,
    t140_pt: u8,
    red: Option<(u8, usize)>,
) -> SessionDescription {
    let id = random_u32().to_string();
    let attr = |name: &str, value: String| Attribute {
        name: name.into(),
        value: Some(value),
    };
    let mut formats = vec![];
    let mut attributes = vec![Attribute {
        name: "recvonly".into(),
        value: None,
    }];
    // The preferred (redundant) format goes first.
    if let Some((pt, generations)) = red {
        formats.push(pt.to_string());
        attributes.push(attr("rtpmap", format!("{pt} red/{CLOCK_RATE}")));
        let chain = vec![t140_pt.to_string(); generations + 1].join("/");
        attributes.push(attr("fmtp", format!("{pt} {chain}")));
    }
    formats.push(t140_pt.to_string());
    attributes.push(attr("rtpmap", format!("{t140_pt} t140/{CLOCK_RATE}")));
    SessionDescription {
        origin: Origin {
            username: "-".into(),
            session_id: id.clone(),
            version: id,
            address: origin.to_string(),
        },
        name: name.into(),
        info: Some("mcast-tools text (T.140)".into()),
        connection: Some(Connection {
            address: group.ip(),
            ttl: group.is_ipv4().then_some(ttl.min(255) as u8),
        }),
        attributes: vec![],
        media: vec![Media {
            kind: "text".into(),
            port: group.port(),
            protocol: "RTP/AVP".into(),
            formats,
            connection: None,
            attributes,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_then_read_back() {
        let group: SocketAddr = "239.69.1.1:5004".parse().unwrap();
        let sdp = describe(
            "Ops",
            "10.0.0.5".parse().unwrap(),
            group,
            16,
            98,
            Some((100, 2)),
        );
        let text = sdp.to_string();
        assert!(text.contains("m=text 5004 RTP/AVP 100 98"), "{text}");
        assert!(text.contains("a=fmtp:100 98/98/98"), "{text}");
        let parsed = mcast_core::sdp::parse(&text).unwrap();
        let info = TextInfo::from_sdp(&parsed).unwrap();
        assert_eq!(
            info,
            TextInfo {
                name: "Ops".into(),
                group,
                t140_pt: 98,
                red_pt: Some(100),
            }
        );
        let plain = describe("x", "10.0.0.5".parse().unwrap(), group, 16, 98, None);
        assert_eq!(TextInfo::from_sdp(&plain).unwrap().red_pt, None);
    }
}
