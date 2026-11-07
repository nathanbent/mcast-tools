//! Posting alerts to a webhook (`--alert-webhook URL`).
//!
//! Each raised or resolved alert is POSTed as JSON:
//!
//! ```json
//! {"text": "ALERT: idf-3 (3rd floor IDF): 2.4% multicast loss on Zone 1; ...",
//!  "event": "alert_raised", "alert": {...}}
//! ```
//!
//! `text` is what Slack, Mattermost and Rocket.Chat incoming webhooks show;
//! anything else can use `event` and `alert`. Posts happen on their own
//! thread, in order, so a slow or dead endpoint never holds up the server;
//! each is tried three times before it's dropped (and logged).

use mcast_control::{Alert, Event};
use serde_json::json;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

const ATTEMPTS: u32 = 3;

pub struct Webhook {
    tx: Sender<(String, Alert)>,
}

impl Webhook {
    pub fn start(url: String) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<(String, Alert)>();
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build();
        std::thread::Builder::new()
            .name("mcast-webhook".into())
            .spawn(move || {
                for (event, alert) in rx {
                    let body = payload(&event, &alert);
                    let mut last = String::new();
                    let sent = (0..ATTEMPTS).any(|i| {
                        if i > 0 {
                            std::thread::sleep(Duration::from_secs(2u64.pow(i)));
                        }
                        match agent.post(&url).send_json(&body) {
                            Ok(_) => true,
                            Err(e) => {
                                last = e.to_string();
                                false
                            }
                        }
                    });
                    if !sent {
                        eprintln!("alert webhook: gave up on alert {}: {last}", alert.id);
                    }
                }
            })?;
        Ok(Self { tx })
    }

    /// Queue an event for posting if it's about an alert.
    pub fn offer(&self, e: &Event) {
        let item = match e {
            Event::AlertRaised { alert } => ("alert_raised", alert),
            Event::AlertResolved { alert } => ("alert_resolved", alert),
            _ => return,
        };
        let _ = self.tx.send((item.0.to_string(), item.1.clone()));
    }
}

fn payload(event: &str, alert: &Alert) -> serde_json::Value {
    let text = if event == "alert_raised" {
        format!("ALERT: {}", alert.message)
    } else {
        format!("RESOLVED: {}", alert.message)
    };
    json!({ "text": text, "event": event, "alert": alert })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcast_control::AlertKind;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    #[test]
    fn posts_alerts_as_json_with_text() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let hook = Webhook::start(url).unwrap();
        let alert = Alert {
            id: 3,
            kind: AlertKind::ProbeOffline,
            probe: "pi-1".into(),
            label: None,
            stream: None,
            group: None,
            message: "pi-1 stopped checking in".into(),
            raised_ms: 1,
            resolved_ms: None,
            worst_loss_percent: None,
            unicast_clean: None,
        };
        hook.offer(&Event::ProbeRemoved { id: "x".into() }); // not an alert: ignored
        hook.offer(&Event::AlertRaised { alert });

        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut len = 0;
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        assert!(first.starts_with("POST /hook"), "{first}");
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        reader
            .get_mut()
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["event"], "alert_raised");
        assert_eq!(v["text"], "ALERT: pi-1 stopped checking in");
        assert_eq!(v["alert"]["kind"], "probe_offline");
    }
}
