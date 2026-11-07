//! Keeps a live list of streams announced via SAP.

use crate::{Event, EventSink};
use mcast_audio::StreamInfo;
use mcast_core::sap::{Directory, DirectoryEvent, Session};
use mcast_core::Interface;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndReason {
    /// The sender withdrew it.
    Deleted,
    /// Not re-announced in time.
    Expired,
}

impl Announced {
    fn new(s: &Session) -> (Self, Option<StreamInfo>) {
        let info = StreamInfo::from_sdp(&s.sdp);
        let audio = info.as_ref().map(|i| AudioStream {
            group: i.group,
            payload_type: i.payload_type,
            encoding: i.encoding.clone(),
            sample_rate: i.sample_rate,
            channels: i.channels,
            ptime_ms: i.ptime_ms,
            playable: i.spec().is_some(),
        });
        let a = Announced {
            name: s.sdp.name.clone(),
            origin: s.origin,
            from: s.from,
            audio,
            sdp: s.sdp.to_string(),
        };
        (a, info)
    }
}

type List = Arc<Mutex<Vec<(Announced, Option<StreamInfo>)>>>;

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
                        let snapshot: Vec<_> = dir.sessions().map(Announced::new).collect();
                        *list.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
                        for e in events {
                            sink(match e {
                                DirectoryEvent::Added(s) => Event::StreamAdded {
                                    stream: Announced::new(&s).0,
                                },
                                DirectoryEvent::Changed(s) => Event::StreamChanged {
                                    stream: Announced::new(&s).0,
                                },
                                DirectoryEvent::Deleted(s) => Event::StreamRemoved {
                                    stream: Announced::new(&s).0,
                                    reason: EndReason::Deleted,
                                },
                                DirectoryEvent::Expired(s) => Event::StreamRemoved {
                                    stream: Announced::new(&s).0,
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Announced, Option<StreamInfo>)>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Everything currently announced, sorted by name.
    pub fn list(&self) -> Vec<Announced> {
        let mut v: Vec<Announced> = self.lock().iter().map(|(a, _)| a.clone()).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then(a.sdp.cmp(&b.sdp)));
        v
    }

    /// Announced audio streams, for picking one to receive.
    pub fn streams(&self) -> Vec<StreamInfo> {
        self.lock().iter().filter_map(|(_, i)| i.clone()).collect()
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
