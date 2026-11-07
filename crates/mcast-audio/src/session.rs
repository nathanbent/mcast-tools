//! Mapping between audio streams and SDP session descriptions.

use crate::format::{AudioFormat, Encoding};
use mcast_core::random_u32;
use mcast_core::sdp::{Attribute, Connection, Media, Origin, SessionDescription};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// An audio stream as described by SDP.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub name: String,
    pub group: SocketAddr,
    pub payload_type: u8,
    /// Encoding name from rtpmap (or the static payload type), e.g. "L24", "PCMU".
    pub encoding: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub ptime_ms: Option<f64>,
}

impl StreamInfo {
    /// Describe the first audio stream in `sdp`, if it has a usable address.
    pub fn from_sdp(sdp: &SessionDescription) -> Option<Self> {
        let media = sdp
            .media
            .iter()
            .find(|m| m.kind == "audio" && m.protocol.starts_with("RTP/"))?;
        let addr = sdp.connection_for(media)?.address;
        let payload_type: u8 = media.formats.first()?.parse().ok()?;
        let (encoding, sample_rate, channels) = match media.rtpmap(payload_type) {
            Some(m) => (m.encoding, m.clock_rate, m.channels.unwrap_or(1)),
            None => {
                let (e, r, c) = static_payload_type(payload_type)?;
                (e.to_string(), r, c)
            }
        };
        Some(Self {
            name: sdp.name.clone(),
            group: SocketAddr::new(addr, media.port),
            payload_type,
            encoding,
            sample_rate,
            channels,
            ptime_ms: media.ptime_ms(),
        })
    }

    /// The PCM format, if this is a stream we can decode.
    pub fn format(&self) -> Option<AudioFormat> {
        let encoding = match self.encoding.to_ascii_uppercase().as_str() {
            "L16" => Encoding::L16,
            "L24" => Encoding::L24,
            _ => return None,
        };
        Some(AudioFormat {
            sample_rate: self.sample_rate,
            channels: self.channels,
            encoding,
        })
    }
}

impl fmt::Display for StreamInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "\"{}\" {} {}/{}/{}",
            self.name, self.group, self.encoding, self.sample_rate, self.channels
        )?;
        if let Some(p) = self.ptime_ms {
            write!(f, " ptime {p}ms")?;
        }
        if self.format().is_none() {
            write!(f, " (unsupported encoding)")?;
        }
        Ok(())
    }
}

/// RFC 3551 static audio payload types.
fn static_payload_type(pt: u8) -> Option<(&'static str, u32, u16)> {
    Some(match pt {
        0 => ("PCMU", 8000, 1),
        8 => ("PCMA", 8000, 1),
        9 => ("G722", 8000, 1),
        10 => ("L16", 44_100, 2),
        11 => ("L16", 44_100, 1),
        _ => return None,
    })
}

/// Build an SDP for a stream we're sending.
pub fn describe(
    name: &str,
    origin: IpAddr,
    group: SocketAddr,
    ttl: u32,
    format: AudioFormat,
    payload_type: u8,
    ptime: Duration,
) -> SessionDescription {
    let id = random_u32().to_string();
    let attr = |name: &str, value: Option<String>| Attribute {
        name: name.into(),
        value,
    };
    let ptime_ms = (ptime.as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
    SessionDescription {
        origin: Origin {
            username: "-".into(),
            session_id: id.clone(),
            version: id,
            address: origin.to_string(),
        },
        name: name.into(),
        info: Some(format!("mcast-tools audio {format}")),
        connection: Some(Connection {
            address: group.ip(),
            ttl: group.is_ipv4().then_some(ttl.min(255) as u8),
        }),
        attributes: vec![],
        media: vec![Media {
            kind: "audio".into(),
            port: group.port(),
            protocol: "RTP/AVP".into(),
            formats: vec![payload_type.to_string()],
            connection: None,
            attributes: vec![
                attr("recvonly", None),
                attr(
                    "rtpmap",
                    Some(format!(
                        "{payload_type} {}/{}/{}",
                        format.encoding, format.sample_rate, format.channels
                    )),
                ),
                attr("ptime", Some(ptime_ms.to_string())),
            ],
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcast_core::sdp;

    #[test]
    fn describe_then_read_back() {
        let fmt = AudioFormat {
            sample_rate: 48_000,
            channels: 2,
            encoding: Encoding::L24,
        };
        let group: SocketAddr = "239.69.0.1:5004".parse().unwrap();
        let sdp_text = describe(
            "Lobby",
            "10.0.0.5".parse().unwrap(),
            group,
            16,
            fmt,
            96,
            Duration::from_millis(1),
        )
        .to_string();
        assert!(sdp_text.contains("a=rtpmap:96 L24/48000/2\r\n"));
        assert!(sdp_text.contains("c=IN IP4 239.69.0.1/16\r\n"));
        let info = StreamInfo::from_sdp(&sdp::parse(&sdp_text).unwrap()).unwrap();
        assert_eq!(info.name, "Lobby");
        assert_eq!(info.group, group);
        assert_eq!(info.ptime_ms, Some(1.0));
        assert_eq!(info.format(), Some(fmt));
    }

    #[test]
    fn static_payload_types_without_rtpmap() {
        let s = sdp::parse("v=0\no=- 1 1 IN IP4 10.0.0.9\ns=Page\nc=IN IP4 239.192.2.1/16\nt=0 0\nm=audio 10000 RTP/AVP 0\n")
            .unwrap();
        let info = StreamInfo::from_sdp(&s).unwrap();
        assert_eq!(
            (info.encoding.as_str(), info.sample_rate, info.channels),
            ("PCMU", 8000, 1)
        );
        assert_eq!(info.format(), None);
    }
}
