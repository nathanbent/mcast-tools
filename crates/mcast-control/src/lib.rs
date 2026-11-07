//! Control layer for mcast-tools: request types that the CLI and the API
//! server share, and long-running services (jobs, SAP discovery, monitoring)
//! that report what they're doing as [`Event`]s.

pub mod announcements;
pub mod describe;
pub mod jobs;
pub mod request;
pub mod watch;

pub use announcements::{Announced, Announcements};
pub use jobs::{JobId, JobInfo, JobState, Jobs, StartError};
pub use request::{RecvRequest, SendRequest};
pub use watch::{MonitorSnapshot, Watch};

use serde::Serialize;
use std::sync::Arc;

/// Something clients may want to hear about as it happens.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    JobStarted {
        job: JobInfo,
    },
    /// Stats (about once a second) or state changes.
    JobUpdated {
        job: JobInfo,
    },
    JobEnded {
        job: JobInfo,
    },
    StreamAdded {
        stream: Announced,
    },
    StreamChanged {
        stream: Announced,
    },
    StreamRemoved {
        stream: Announced,
        reason: announcements::EndReason,
    },
    Monitor {
        snapshot: MonitorSnapshot,
    },
}

/// Where services send their events. Must not block.
pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
