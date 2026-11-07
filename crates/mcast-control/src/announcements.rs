//! Keeps a live list of streams announced via SAP.

use crate::{Event, EventSink};
use mcast_audio::StreamInfo;
use mcast_core::sap::{Directory, DirectoryEvent, Session};
use mcast_core::Interface;
use mcast_file::session::FileInfo;
use mcast_text::session::TextInfo;
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// An announced session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Announced {
    pub name: String,
    /// Origin address from the SAP header.
    pub origin: IpAddr,
    /// Where the announcement came from.
    pub from: SocketAddr,
    /// Its audio stream, if it has one we understand.
    pub audio: Option<AudioStream>,
    /// Its real-time text stream, if any.
    pub text: Option<TextStream>,
    /// Its file stream, if any.
    pub file: Option<FileStream>,
    pub sdp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AudioStream {
    pub group: SocketAddr,
    pub payload_type: u8,
    pub encoding: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub ptime_ms: Option<f64>,
    /// Whether `mcast` can decode it.
    pub playable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextStream {
    pub group: SocketAddr,
    pub t140_pt: u8,
    pub red_pt: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileStream {
    pub group: SocketAddr,
    pub payload_type: u8,
}

/// One session, with what each kind of client needs from it.
#[derive(Debug, Clone)]
struct Entry {
    shown: Announced,
    audio: Option<StreamInfo>,
    text: Option<TextInfo>,
    file: Option<FileInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndReason {
    /// The sender withdrew it.
    Deleted,
    /// Not re-announced in time.
    Expired,
}

impl Entry {
    fn new(s: &Session) -> Self {
        let info = StreamInfo::from_sdp(&s.sdp);
        let text = TextInfo::from_sdp(&s.sdp);
        let file = FileInfo::from_sdp(&s.sdp);
        let audio = info.as_ref().map(|i| AudioStream {
            group: i.group,
            payload_type: i.payload_type,
            encoding: i.encoding.clone(),
            sample_rate: i.sample_rate,
            channels: i.channels,
            ptime_ms: i.ptime_ms,
            playable: i.spec().is_some(),
        });
        let shown = Announced {
            name: s.sdp.name.clone(),
            origin: s.origin,
            from: s.from,
            audio,
            text: text.as_ref().map(|t| TextStream {
                group: t.group,
                t140_pt: t.t140_pt,
                red_pt: t.red_pt,
            }),
            file: file.as_ref().map(|f| FileStream {
                group: f.group,
                payload_type: f.payload_type,
            }),
            sdp: s.sdp.to_string(),
        };
        Entry {
            shown,
            audio: info,
            text,
            file,
        }
    }
}

fn shown(s: &Session) -> Announced {
    Entry::new(s).shown
}

type List = Arc<Mutex<Vec<Entry>>>;

/// Listens for SAP announcements in a background thread.
pub struct Announcements {
    list: List,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Announcements {
    pub fn start(
        groups: &[SocketAddr],
        interface: Interface,
        sink: EventSink,
    ) -> std::io::Result<Self> {
        let mut dir = Directory::open(groups, interface, Duration::from_secs(300))?;
        let list: List = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (list, stop) = (list.clone(), stop.clone());
            std::thread::Builder::new()
                .name("mcast-sap".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let events = match dir.poll(Duration::from_millis(500)) {
                            Ok(e) => e,
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(500));
                                continue;
                            }
                        };
                        if events.is_empty() {
                            continue;
                        }
                        let snapshot: Vec<_> = dir.sessions().map(Entry::new).collect();
                        *list.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
                        for e in events {
                            sink(match e {
                                DirectoryEvent::Added(s) => {
                                    Event::StreamAdded { stream: shown(&s) }
                                }
                                DirectoryEvent::Changed(s) => {
                                    Event::StreamChanged { stream: shown(&s) }
                                }
                                DirectoryEvent::Deleted(s) => Event::StreamRemoved {
                                    stream: shown(&s),
                                    reason: EndReason::Deleted,
                                },
                                DirectoryEvent::Expired(s) => Event::StreamRemoved {
                                    stream: shown(&s),
                                    reason: EndReason::Expired,
                                },
                            });
                        }
                    }
                })?
        };
        Ok(Self {
            list,
            stop,
            thread: Some(thread),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Everything currently announced, sorted by name.
    pub fn list(&self) -> Vec<Announced> {
        let mut v: Vec<Announced> = self.lock().iter().map(|e| e.shown.clone()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then(a.sdp.cmp(&b.sdp)));
        v
    }

    /// Announced audio streams, for picking one to receive.
    pub fn streams(&self) -> Vec<StreamInfo> {
        self.lock().iter().filter_map(|e| e.audio.clone()).collect()
    }

    /// Announced text streams, for joining one by name.
    pub fn text_streams(&self) -> Vec<TextInfo> {
        self.lock().iter().filter_map(|e| e.text.clone()).collect()
    }

    /// Announced file streams, for receiving from one by name.
    pub fn file_streams(&self) -> Vec<FileInfo> {
        self.lock().iter().filter_map(|e| e.file.clone()).collect()
    }
}

impl Drop for Announcements {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
