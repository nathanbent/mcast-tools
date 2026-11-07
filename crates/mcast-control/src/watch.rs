//! Runs `mcast-monitor` in the background and keeps the latest snapshot.

use crate::{Event, EventSink};
use mcast_monitor::{GroupStatus, Monitor, MonitorConfig, StreamReport};
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
