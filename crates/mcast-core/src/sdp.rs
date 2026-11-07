//! A small, lenient SDP (RFC 8866) model: enough to describe and discover
//! RTP streams. Unknown lines are ignored when parsing.

use std::fmt;
use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub username: String,
    pub session_id: String,
    pub version: String,
    /// Unicast address of the machine that created the session.
    pub address: String,
}

impl Origin {
    /// Identity of a session across versions: (username, id, address).
    pub fn key(&self) -> (String, String, String) {
        (
            self.username.clone(),
            self.session_id.clone(),
            self.address.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub address: IpAddr,
    /// IPv4 multicast TTL, if given.
    pub ttl: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    /// "audio", "video", ...
    pub kind: String,
    pub port: u16,
    /// "RTP/AVP", ...
    pub protocol: String,
    /// Payload types (for RTP).
    pub formats: Vec<String>,
    pub connection: Option<Connection>,
    pub attributes: Vec<Attribute>,
}

/// Parsed `a=rtpmap:<pt> <encoding>/<clock>[/<channels>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpMap {
    pub payload_type: u8,
    pub encoding: String,
    pub clock_rate: u32,
    pub channels: Option<u16>,
}

impl Media {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_deref().unwrap_or(""))
    }

    pub fn rtpmap(&self, payload_type: u8) -> Option<RtpMap> {
        self.attributes
            .iter()
            .filter(|a| a.name == "rtpmap")
            .filter_map(|a| parse_rtpmap(a.value.as_deref()?))
            .find(|m| m.payload_type == payload_type)
    }

    /// Packet time in milliseconds (`a=ptime`).
    pub fn ptime_ms(&self) -> Option<f64> {
        self.attribute("ptime")?.trim().parse().ok()
    }
}

fn parse_rtpmap(value: &str) -> Option<RtpMap> {
    let (pt, rest) = value.split_once(' ')?;
    let mut parts = rest.trim().split('/');
    Some(RtpMap {
        payload_type: pt.trim().parse().ok()?,
        encoding: parts.next()?.to_string(),
        clock_rate: parts.next()?.parse().ok()?,
        channels: parts.next().and_then(|c| c.parse().ok()),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescription {
    pub origin: Origin,
    pub name: String,
    pub info: Option<String>,
    pub connection: Option<Connection>,
    pub attributes: Vec<Attribute>,
    pub media: Vec<Media>,
}

impl SessionDescription {
    /// The connection for `media`, falling back to the session-level one.
    pub fn connection_for<'a>(&'a self, media: &'a Media) -> Option<&'a Connection> {
        media.connection.as_ref().or(self.connection.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdpError {
    MissingOrigin,
    Invalid(String),
}

impl fmt::Display for SdpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdpError::MissingOrigin => write!(f, "SDP has no o= line"),
            SdpError::Invalid(line) => write!(f, "invalid SDP line: {line}"),
        }
    }
}

impl std::error::Error for SdpError {}

/// Parse the `o=` line value: `<user> <id> <version> IN <IP4|IP6> <address>`.
pub fn parse_origin(value: &str) -> Option<Origin> {
    let f: Vec<&str> = value.split_whitespace().collect();
    if f.len() != 6 {
        return None;
    }
    Some(Origin {
        username: f[0].into(),
        session_id: f[1].into(),
        version: f[2].into(),
        address: f[5].into(),
    })
}

/// Parse the `c=` line value: `IN <IP4|IP6> <address>[/ttl[/count]]`.
fn parse_connection(value: &str) -> Option<Connection> {
    let f: Vec<&str> = value.split_whitespace().collect();
    if f.len() != 3 {
        return None;
    }
    let mut parts = f[2].split('/');
    let address = parts.next()?.parse().ok()?;
    let ttl = match (f[1], parts.next()) {
        ("IP4", Some(t)) => t.parse().ok(),
        _ => None,
    };
    Some(Connection { address, ttl })
}

pub fn parse(text: &str) -> Result<SessionDescription, SdpError> {
    let mut origin = None;
    let mut name = String::new();
    let mut info = None;
    let mut connection = None;
    let mut attributes = Vec::new();
    let mut media: Vec<Media> = Vec::new();

    for line in text.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        let Some((kind, value)) = line.split_once('=') else {
            continue;
        };
        let bad = || SdpError::Invalid(line.to_string());
        match (kind, media.last_mut()) {
            ("o", _) => origin = Some(parse_origin(value).ok_or_else(bad)?),
            ("s", None) => name = value.to_string(),
            ("i", None) => info = Some(value.to_string()),
            ("c", None) => connection = Some(parse_connection(value).ok_or_else(bad)?),
            ("c", Some(m)) => m.connection = Some(parse_connection(value).ok_or_else(bad)?),
            ("a", m) => {
                let attr = match value.split_once(':') {
                    Some((n, v)) => Attribute {
                        name: n.into(),
                        value: Some(v.into()),
                    },
                    None => Attribute {
                        name: value.into(),
                        value: None,
                    },
                };
                match m {
                    Some(m) => m.attributes.push(attr),
                    None => attributes.push(attr),
                }
            }
            ("m", _) => {
                let f: Vec<&str> = value.split_whitespace().collect();
                if f.len() < 3 {
                    return Err(bad());
                }
                // Port may carry a count ("5004/2"); we only use the first.
                let port = f[1]
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .parse()
                    .map_err(|_| bad())?;
                media.push(Media {
                    kind: f[0].into(),
                    port,
                    protocol: f[2].into(),
                    formats: f[3..].iter().map(|s| s.to_string()).collect(),
                    connection: None,
                    attributes: Vec::new(),
                });
            }
            _ => {}
        }
    }
    Ok(SessionDescription {
        origin: origin.ok_or(SdpError::MissingOrigin)?,
        name,
        info,
        connection,
        attributes,
        media,
    })
}

fn write_connection(f: &mut fmt::Formatter<'_>, c: &Connection) -> fmt::Result {
    match (c.address, c.ttl) {
        (IpAddr::V4(a), Some(ttl)) => write!(f, "c=IN IP4 {a}/{ttl}\r\n"),
        (IpAddr::V4(a), None) => write!(f, "c=IN IP4 {a}\r\n"),
        (IpAddr::V6(a), _) => write!(f, "c=IN IP6 {a}\r\n"),
    }
}

fn write_attribute(f: &mut fmt::Formatter<'_>, a: &Attribute) -> fmt::Result {
    match &a.value {
        Some(v) => write!(f, "a={}:{v}\r\n", a.name),
        None => write!(f, "a={}\r\n", a.name),
    }
}

impl fmt::Display for SessionDescription {
    /// Serialize with CRLF line endings, in the order RFC 8866 requires.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let o = &self.origin;
        let family = if o.address.contains(':') {
            "IP6"
        } else {
            "IP4"
        };
        write!(f, "v=0\r\n")?;
        write!(
            f,
            "o={} {} {} IN {family} {}\r\n",
            o.username, o.session_id, o.version, o.address
        )?;
        write!(
            f,
            "s={}\r\n",
            if self.name.is_empty() {
                "-"
            } else {
                &self.name
            }
        )?;
        if let Some(i) = &self.info {
            write!(f, "i={i}\r\n")?;
        }
        if let Some(c) = &self.connection {
            write_connection(f, c)?;
        }
        write!(f, "t=0 0\r\n")?;
        for a in &self.attributes {
            write_attribute(f, a)?;
        }
        for m in &self.media {
            write!(f, "m={} {} {}", m.kind, m.port, m.protocol)?;
            for fmt_ in &m.formats {
                write!(f, " {fmt_}")?;
            }
            write!(f, "\r\n")?;
            if let Some(c) = &m.connection {
                write_connection(f, c)?;
            }
            for a in &m.attributes {
                write_attribute(f, a)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shaped like a typical AES67 device announcement.
    const AES67: &str = "v=0\r\n\
        o=- 1311738121 1311738121 IN IP4 192.168.1.10\r\n\
        s=Stage Box 1\r\n\
        c=IN IP4 239.69.83.67/32\r\n\
        t=0 0\r\n\
        a=keywds:Dante\r\n\
        m=audio 5004 RTP/AVP 97\r\n\
        i=2 channels: Left, Right\r\n\
        a=recvonly\r\n\
        a=rtpmap:97 L24/48000/2\r\n\
        a=ptime:1\r\n\
        a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-12-34-56:0\r\n\
        a=mediaclk:direct=0\r\n";

    #[test]
    fn parses_aes67_style_sdp() {
        let sdp = parse(AES67).unwrap();
        assert_eq!(sdp.name, "Stage Box 1");
        assert_eq!(sdp.origin.session_id, "1311738121");
        let m = &sdp.media[0];
        assert_eq!((m.kind.as_str(), m.port), ("audio", 5004));
        let c = sdp.connection_for(m).unwrap();
        assert_eq!(c.address, "239.69.83.67".parse::<IpAddr>().unwrap());
        assert_eq!(c.ttl, Some(32));
        assert_eq!(
            m.rtpmap(97).unwrap(),
            RtpMap {
                payload_type: 97,
                encoding: "L24".into(),
                clock_rate: 48000,
                channels: Some(2)
            }
        );
        assert_eq!(m.ptime_ms(), Some(1.0));
        assert_eq!(m.attribute("recvonly"), Some(""));
    }

    #[test]
    fn roundtrips() {
        let sdp = parse(AES67).unwrap();
        assert_eq!(parse(&sdp.to_string()).unwrap(), sdp);
    }

    #[test]
    fn media_level_ipv6_connection() {
        let sdp = parse(
            "v=0\no=- 1 1 IN IP6 fe80::1\ns=x\nt=0 0\nm=audio 5004 RTP/AVP 0\nc=IN IP6 ff15::69\n",
        )
        .unwrap();
        let m = &sdp.media[0];
        assert_eq!(
            sdp.connection_for(m).unwrap().address,
            "ff15::69".parse::<IpAddr>().unwrap()
        );
        assert_eq!(m.rtpmap(0), None);
    }

    #[test]
    fn requires_origin() {
        assert_eq!(parse("v=0\ns=x\n"), Err(SdpError::MissingOrigin));
    }
}
