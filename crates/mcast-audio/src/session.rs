//! Mapping between audio streams and SDP session descriptions.

use crate::codec::{Codec, StreamSpec};
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
    /// Opus fmtp `stereo=1` / `sprop-stereo=1`.
    pub opus_stereo: bool,
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
                let spec = StreamSpec::from_static_payload_type(payload_type)?;
                // RTP clock rate is what SDP reports (8000 for G.722).
                let rate = if spec.codec == Codec::G722 {
                    8_000
                } else {
                    spec.sample_rate
                };
                (spec.codec.to_string(), rate, spec.channels)
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
            opus_stereo: media.attributes.iter().any(|a| {
                a.name == "fmtp"
                    && a.value.as_deref().is_some_and(|v| {
                        v.split([' ', ';'])
                            .any(|p| matches!(p.trim(), "stereo=1" | "sprop-stereo=1"))
                    })
            }),
        })
    }

    /// How to decode this stream, if we support its codec.
    pub fn spec(&self) -> Option<StreamSpec> {
        let codec = Codec::from_sdp_name(&self.encoding)?;
        // Opus always says "/2" in rtpmap (RFC 7587); decode stereo only if
        // the sender says it sends stereo.
        let channels = match codec {
            Codec::Opus if !self.opus_stereo => 1,
            _ => self.channels,
        };
        Some(StreamSpec::new(codec, self.sample_rate, channels))
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
        if self.spec().is_none() {
            write!(f, " (unsupported encoding)")?;
        }
        Ok(())
    }
}

/// Build an SDP for a stream we're sending.
#[allow(clippy::too_many_arguments)]
pub fn describe(
    name: &str,
    origin: IpAddr,
    group: SocketAddr,
    ttl: u32,
    spec: StreamSpec,
    payload_type: u8,
    ptime: Duration,
    opus_fec: bool,
) -> SessionDescription {
    let id = random_u32().to_string();
    let attr = |name: &str, value: Option<String>| Attribute {
        name: name.into(),
        value,
    };
    let ptime_ms = (ptime.as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
    let mut attributes = vec![
        attr("recvonly", None),
        attr("rtpmap", Some(format!("{payload_type} {}", spec.rtpmap()))),
    ];
    if spec.codec == Codec::Opus {
        let mut fmtp = vec![];
        if spec.channels == 2 {
            fmtp.push("sprop-stereo=1");
        }
        if opus_fec {
            fmtp.push("useinbandfec=1");
        }
        if !fmtp.is_empty() {
            attributes.push(attr(
                "fmtp",
                Some(format!("{payload_type} {}", fmtp.join("; "))),
            ));
        }
    }
    attributes.push(attr("ptime", Some(ptime_ms.to_string())));
    SessionDescription {
        origin: Origin {
            username: "-".into(),
            session_id: id.clone(),
            version: id,
            address: origin.to_string(),
        },
        name: name.into(),
        info: Some(format!("mcast-tools audio {spec}")),
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
            attributes,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcast_core::sdp;

    #[test]
    fn describe_then_read_back() {
        let group: SocketAddr = "239.69.0.1:5004".parse().unwrap();
        let origin = "10.0.0.5".parse().unwrap();
        let cases = [
            (
                StreamSpec::new(Codec::L24, 48_000, 2),
                96,
                "a=rtpmap:96 L24/48000/2\r\n",
            ),
            (
                StreamSpec::new(Codec::Pcmu, 0, 0),
                0,
                "a=rtpmap:0 PCMU/8000\r\n",
            ),
            (
                StreamSpec::new(Codec::G722, 0, 0),
                9,
                "a=rtpmap:9 G722/8000\r\n",
            ),
            (
                StreamSpec::new(Codec::Opus, 0, 2),
                107,
                "a=fmtp:107 sprop-stereo=1; useinbandfec=1\r\n",
            ),
            (
                StreamSpec::new(Codec::Opus, 0, 1),
                107,
                "a=fmtp:107 useinbandfec=1\r\n",
            ),
        ];
        for (spec, pt, line) in cases {
            let text = describe(
                "Lobby",
                origin,
                group,
                16,
                spec,
                pt,
                Duration::from_millis(20),
                true,
            )
            .to_string();
            assert!(text.contains(line), "{spec}: {text}");
            assert!(text.contains("c=IN IP4 239.69.0.1/16\r\n"));
            let info = StreamInfo::from_sdp(&sdp::parse(&text).unwrap()).unwrap();
            assert_eq!((info.name.as_str(), info.group), ("Lobby", group));
            assert_eq!(info.ptime_ms, Some(20.0));
            assert_eq!(info.spec(), Some(spec), "{spec}");
        }
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
        assert_eq!(info.spec(), Some(StreamSpec::new(Codec::Pcmu, 8_000, 1)));
    }

    #[test]
    fn g722_and_opus_from_rtpmap() {
        let parse = |m: &str| {
            let text =
                format!("v=0\no=- 1 1 IN IP4 10.0.0.9\ns=x\nc=IN IP4 239.192.2.1/16\nt=0 0\n{m}");
            StreamInfo::from_sdp(&sdp::parse(&text).unwrap())
                .unwrap()
                .spec()
                .unwrap()
        };
        assert_eq!(
            parse("m=audio 10000 RTP/AVP 9\na=rtpmap:9 G722/8000\n").to_string(),
            "G722/16000/1"
        );
        assert_eq!(
            parse("m=audio 10000 RTP/AVP 107\na=rtpmap:107 opus/48000/2\n").to_string(),
            "opus/48000/1"
        );
        assert_eq!(
            parse(
                "m=audio 10000 RTP/AVP 107\na=rtpmap:107 opus/48000/2\na=fmtp:107 sprop-stereo=1\n"
            )
            .to_string(),
            "opus/48000/2"
        );
        let g729 = "v=0\no=- 1 1 IN IP4 10.0.0.9\ns=x\nc=IN IP4 239.192.2.1/16\nt=0 0\nm=audio 10000 RTP/AVP 18\na=rtpmap:18 G729/8000\n";
        assert_eq!(
            StreamInfo::from_sdp(&sdp::parse(g729).unwrap())
                .unwrap()
                .spec(),
            None
        );
    }
}
