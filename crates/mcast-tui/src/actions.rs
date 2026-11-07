//! What the TUI asks the server to do. Every call runs in a background
//! thread and reports back as a `Msg`, so the screen never waits.

use crate::api::{encode, ApiError, Client};
use crate::app::{App, Level, Msg};
use crate::form::FormKind;
use crate::model::Stream;
use serde_json::{json, Value};

impl App {
    /// Run `f` against the API in a background thread.
    pub fn spawn(&self, f: impl FnOnce(&Client) -> Msg + Send + 'static) {
        let (client, tx) = (self.client.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(f(&client));
        });
    }

    pub fn refresh(&self, what: &str) {
        match what {
            "jobs" => self.spawn(|c| Msg::Jobs(c.get("/jobs"))),
            "streams" => self.spawn(|c| Msg::Streams(c.get("/streams"))),
            "media" => self.spawn(|c| Msg::Media(c.get("/media"))),
            "devices" => self.spawn(|c| {
                Msg::Devices(c.get::<Value>("/devices").and_then(|v| {
                    serde_json::from_value(v["devices"].clone()).map_err(|e| ApiError {
                        status: 0,
                        message: e.to_string(),
                        job: None,
                    })
                }))
            }),
            "monitor" => self.spawn(|c| Msg::Monitor(c.get("/monitor"))),
            "probes" => {
                let seen = self.probe_events;
                self.spawn(move |c| Msg::Probes(seen, c.get("/probes")))
            }
            "alerts" => {
                let seen = self.alert_events;
                self.spawn(move |c| Msg::Alerts(seen, c.get("/alerts")))
            }
            _ => {}
        }
    }

    pub fn refresh_all(&self) {
        for w in [
            "jobs", "streams", "media", "devices", "monitor", "probes", "alerts",
        ] {
            self.refresh(w);
        }
    }

    pub fn load_text(&self, id: u64) {
        self.spawn(move |c| Msg::TextHistory(id, c.get(&format!("/jobs/{id}/text"))));
    }

    /// Start whatever the form describes.
    pub fn submit(&mut self, kind: FormKind) {
        let path = match kind {
            FormKind::Page => "/senders",
            FormKind::Text => "/texts",
            FormKind::SendFiles => "/file-senders",
            FormKind::ReceiveFiles => "/file-receivers",
        };
        match self.forms.get(kind).request() {
            Ok(body) => self.start(kind, path, body),
            Err(e) => self.say(e, Level::Error),
        }
    }

    fn start(&mut self, kind: FormKind, path: &'static str, body: Value) {
        self.busy = true;
        self.say("Starting…", Level::Info);
        self.spawn(move |c| Msg::Started(kind, c.post(path, &body)));
    }

    pub fn stop(&mut self, id: u64) {
        self.spawn(move |c| {
            Msg::Done(
                c.delete(&format!("/jobs/{id}"))
                    .map(|_| format!("Stopped job {id}")),
                Some("jobs"),
            )
        });
    }

    /// Enter on an announced stream: listen to audio, join text, receive files.
    pub fn use_stream(&mut self, s: &Stream) {
        if let Some(a) = s.audio.clone() {
            if !a.playable {
                return self.say("can't decode this stream", Level::Error);
            }
            let output = (!self.output.is_empty()).then(|| self.output.clone());
            let body = json!({
                "group": a.group, "encoding": a.encoding, "rate": a.sample_rate, "channels": a.channels,
                "sink": {"type": "play", "device": output},
            });
            self.start(FormKind::Page, "/receivers", body);
        } else if let Some(t) = &s.text {
            let name = self.forms.text.text_or_null("name");
            let body = json!({
                "group": t.group, "t140_pt": t.t140_pt, "red_pt": t.red_pt.unwrap_or(100),
                "redundancy": t.red_pt.is_some(), "name": name,
            });
            self.start(FormKind::Text, "/texts", body);
        } else if let Some(f) = &s.file {
            let body = json!({"group": f.group, "payload_type": f.payload_type});
            self.start(FormKind::ReceiveFiles, "/file-receivers", body);
        } else {
            self.say("nothing here mcast can use", Level::Error);
        }
    }

    /// Send a message on the current text channel.
    pub fn say_text(&mut self, text: String) {
        let Some(id) = self.current_channel().map(|j| j.id) else {
            return self.say("join a text channel first (j)", Level::Error);
        };
        if text.trim().is_empty() {
            return;
        }
        self.spawn(move |c| {
            Msg::Done(
                c.post(&format!("/jobs/{id}/text"), &json!({"text": text}))
                    .map(|_| String::new()),
                None,
            )
        });
    }

    /// Show the next (or previous) open text channel.
    pub fn next_channel(&mut self, delta: isize) {
        let ids: Vec<u64> = self.channels().iter().map(|j| j.id).collect();
        if ids.is_empty() {
            return;
        }
        let cur = self.current_channel().map(|j| j.id);
        let i = ids.iter().position(|&id| Some(id) == cur).unwrap_or(0) as isize;
        self.channel = Some(ids[(i + delta).rem_euclid(ids.len() as isize) as usize]);
    }

    /// Cycle the output device used by Listen.
    pub fn next_output(&mut self) {
        let mut outs: Vec<String> = vec![String::new()];
        outs.extend(
            self.devices
                .iter()
                .filter(|d| d.kind == "output")
                .map(|d| d.name.clone()),
        );
        let i = outs.iter().position(|o| *o == self.output).unwrap_or(0);
        self.output = outs[(i + 1) % outs.len()].clone();
    }

    pub fn upload(&mut self, path: String) {
        let path = std::path::PathBuf::from(path.trim());
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_start_matches('.').to_string())
        else {
            return self.say("that isn't a file path", Level::Error);
        };
        self.busy = true;
        self.say(format!("Uploading {name}…"), Level::Info);
        self.spawn(move |c| {
            Msg::Done(
                c.upload(&name, &path).map(|_| format!("Uploaded {name}")),
                Some("media"),
            )
        });
    }

    pub fn delete_media(&mut self, name: String) {
        self.spawn(move |c| {
            let path = format!("/media/{}", encode(&name));
            Msg::Done(
                c.delete(&path).map(|_| format!("Deleted {name}")),
                Some("media"),
            )
        });
    }
}
