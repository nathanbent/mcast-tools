//! Runs `mcast-monitor` in the background and keeps the latest snapshot.

use crate::{Event, EventSink};
use mcast_monitor::{GroupStatus, ListenerReport, Monitor, MonitorConfig, StreamReport};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize)]
pub struct MonitorSnapshot {
    pub groups: Vec<WatchedGroup>,
    pub streams: Vec<StreamRow>,
    /// Receivers heard sending RTCP receiver reports on a watched group.
    pub listeners: Vec<ListenerRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WatchedGroup {
    pub group: SocketAddr,
    pub name: Option<String>,
    pub from_sap: bool,
    pub ended: bool,
    pub error: Option<String>,
}

impl From<GroupStatus> for WatchedGroup {
    fn from(g: GroupStatus) -> Self {
        Self {
            group: g.group,
            name: g.name,
            from_sap: g.from_sap,
            ended: g.ended,
            error: g.error,
        }
    }
}

/// One sender on one group, over the last interval.
#[derive(Debug, Clone, Serialize)]
pub struct StreamRow {
    pub group: SocketAddr,
    pub name: Option<String>,
    pub source: SocketAddr,
    pub ssrc: Option<u32>,
    pub payload_type: Option<u8>,
    pub codec: Option<String>,
    pub packets: u64,
    pub bytes: u64,
    pub packets_per_sec: f64,
    pub kbit_per_sec: f64,
    pub lost: u64,
    pub late: u64,
    pub loss_percent: f64,
    pub jitter_ms: Option<f64>,
    pub max_gap_ms: f64,
    pub ptime_ms: Option<f64>,
    pub ttl: Option<u8>,
    pub tos: Option<u8>,
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    pub idle_ms: f64,
}

impl From<StreamReport> for StreamRow {
    fn from(r: StreamReport) -> Self {
        // JSON has no infinities; silence is reported as no level.
        let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
        Self {
            group: r.group,
            name: r.name,
            source: r.source,
            ssrc: r.ssrc,
            payload_type: r.payload_type,
            codec: r.codec,
            packets: r.packets,
            bytes: r.bytes,
            packets_per_sec: r.packets_per_sec,
            kbit_per_sec: r.kbit_per_sec,
            lost: r.lost,
            late: r.late,
            loss_percent: r.loss_percent,
            jitter_ms: finite(r.jitter_ms),
            max_gap_ms: r.max_gap_ms,
            ptime_ms: finite(r.ptime_ms),
            ttl: r.ttl,
            tos: r.tos,
            peak_dbfs: finite(r.peak_dbfs),
            rms_dbfs: finite(r.rms_dbfs),
            idle_ms: r.idle.as_secs_f64() * 1000.0,
        }
    }
}

/// A receiver heard on a group's RTCP port, e.g. a speaker running
/// `mcast receive --name Lobby`. It's dropped once it has been silent for
/// about 30 s, or soon after it says BYE.
#[derive(Debug, Clone, Serialize)]
pub struct ListenerRow {
    /// The group's media address (RTCP is on the next port).
    pub group: SocketAddr,
    pub ssrc: u32,
    /// Where its reports come from.
    pub from: SocketAddr,
    /// The name it gives, e.g. a room (`--name`, else its hostname).
    pub name: Option<String>,
    pub cname: Option<String>,
    /// SSRC of the stream it's reporting on (see `streams[].ssrc`);
    /// `null` when it's receiving nothing.
    pub about: Option<u32>,
    /// Time since its last report.
    pub last_heard_ms: f64,
    /// It said BYE.
    pub left: bool,
    /// Loss since its previous report.
    pub loss_percent: Option<f64>,
    pub cumulative_lost: Option<i32>,
    pub jitter_ms: Option<f64>,
    /// mcast-tools receivers only: sender clock to arrival, and audio in
    /// the playout buffer.
    pub transit_ms: Option<f64>,
    pub buffered_ms: Option<f64>,
    /// How far behind the sender it plays: transit plus buffer.
    pub latency_ms: Option<f64>,
}

impl From<ListenerReport> for ListenerRow {
    fn from(r: ListenerReport) -> Self {
        let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
        Self {
            group: r.group,
            ssrc: r.ssrc,
            from: r.from,
            latency_ms: finite(r.latency_ms()),
            name: r.name,
            cname: r.cname,
            about: r.about,
            last_heard_ms: r.last_heard.as_secs_f64() * 1000.0,
            left: r.left,
            loss_percent: finite(r.loss_percent),
            cumulative_lost: r.cumulative_lost,
            jitter_ms: finite(r.jitter_ms),
            transit_ms: finite(r.transit_ms),
            buffered_ms: finite(r.buffered_ms),
        }
    }
}

/// Watches groups (and SAP-announced streams) in a background thread,
/// publishing a snapshot every `interval`.
pub struct Watch {
    latest: Arc<Mutex<MonitorSnapshot>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    pub fn start(cfg: MonitorConfig, interval: Duration, sink: EventSink) -> std::io::Result<Self> {
        let mut m = Monitor::start(cfg)?;
        let latest: Arc<Mutex<MonitorSnapshot>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (latest, stop) = (latest.clone(), stop.clone());
            std::thread::Builder::new()
                .name("mcast-monitor".into())
                .spawn(move || {
                    let mut next = Instant::now() + interval;
                    while !stop.load(Ordering::Relaxed) {
                        // Short slices so stopping is prompt.
                        let until = next.min(Instant::now() + Duration::from_millis(250));
                        m.poll(until);
                        if Instant::now() < next {
                            continue;
                        }
                        next += interval;
                        let snapshot = MonitorSnapshot {
                            streams: m.report().into_iter().map(StreamRow::from).collect(),
                            groups: m.groups().into_iter().map(WatchedGroup::from).collect(),
                            listeners: m.listeners().into_iter().map(ListenerRow::from).collect(),
                        };
                        *latest.lock().unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
                        sink(Event::Monitor { snapshot });
                    }
                })?
        };
        Ok(Self {
            latest,
            stop,
            thread: Some(thread),
        })
    }

    pub fn snapshot(&self) -> MonitorSnapshot {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{RecvRequest, SendRequest, SinkRequest};
    use crate::Jobs;
    use mcast_core::Interface;

    #[test]
    fn a_named_receiver_shows_up_as_a_listener_on_the_stream() {
        let group: SocketAddr = "239.255.77.31:5004".parse().unwrap();
        let cfg = MonitorConfig {
            targets: vec![mcast_monitor::Target::new(group)],
            sap: None,
            interface: Interface::Any,
            default_spec: None,
        };
        let watch = Watch::start(cfg, Duration::from_millis(200), Arc::new(|_| {})).unwrap();

        let jobs = Jobs::new(Interface::Any, Arc::new(|_| {}));
        let sender = jobs
            .start_sender(SendRequest {
                group,
                sap: false,
                rtcp_interval_s: 0.5,
                ..SendRequest::default()
            })
            .unwrap();
        let wav = std::env::temp_dir().join(format!(
            "mcast-watch-listener-{}.wav",
            mcast_core::random_u32()
        ));
        let receiver = jobs
            .start_receiver(
                RecvRequest {
                    group: Some(group),
                    name: Some("Lobby".into()),
                    rtcp_interval_s: 0.5,
                    sink: SinkRequest::Wav { path: wav.clone() },
                    ..RecvRequest::default()
                },
                &[],
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let (listener, stream_ssrc) = loop {
            let snap = watch.snapshot();
            let stream = snap.streams.iter().find_map(|s| s.ssrc);
            let lobby = snap
                .listeners
                .iter()
                .find(|l| l.name.as_deref() == Some("Lobby") && l.about.is_some());
            if let (Some(l), Some(ssrc)) = (lobby, stream) {
                break (l.clone(), ssrc);
            }
            assert!(Instant::now() < deadline, "no listener: {snap:?}");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(listener.group, group);
        assert_eq!(listener.about, Some(stream_ssrc));
        // The send's job names the same stream, so a client can tell which
        // listeners heard its page.
        assert_eq!(sender.ssrc, Some(stream_ssrc));
        assert_eq!(receiver.ssrc, None);
        assert!(!listener.left);
        assert!(listener.last_heard_ms < 5000.0);

        // Serialized as the API shows it.
        let json = serde_json::to_value(watch.snapshot()).unwrap();
        assert!(json["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "Lobby" && l["about"] == stream_ssrc));

        jobs.stop(receiver.id, Duration::from_secs(5));
        jobs.stop(sender.id, Duration::from_secs(5));
        let _ = std::fs::remove_file(wav);
    }
}
