//! Senders and receivers running as background jobs that can be listed,
//! watched and stopped. A job that would send to a group another job is
//! already sending to is rejected rather than mixed or queued.
//!
//! This module is the manager; each kind of job starts in its own module.

mod audio;
mod file;
mod text;

pub use file::ReceivedFile;
pub use text::{TextError, TextLine};

use crate::request::{FileRecvRequest, FileSendRequest, RecvRequest, SendRequest, TextRequest};
use crate::{now_ms, Event, EventSink};
use mcast_audio::receiver;
use mcast_audio::sender::Progress;
use mcast_core::Interface;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Finished jobs kept for listing.
const KEEP_FINISHED: usize = 50;
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Audio sender (a page, tone or live input).
    Send,
    /// Audio receiver.
    Recv,
    /// Real-time text channel.
    Text,
    FileSend,
    FileRecv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Preparing (opening files, measuring loudness, finding the stream).
    Starting,
    Running,
    /// Asked to stop; still sending its last packets or SAP deletion.
    Stopping,
    /// The source ended or the duration elapsed.
    Finished,
    /// Stopped on request.
    Stopped,
    Failed,
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum JobRequest {
    Send(SendRequest),
    Recv(RecvRequest),
    Text(TextRequest),
    FileSend(FileSendRequest),
    FileRecv(FileRecvRequest),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SendStats {
    pub packets: u64,
    pub send_errors: u64,
    /// Audio sent so far, in seconds.
    pub seconds: f64,
    /// Captured samples dropped because processing fell behind (known at the end).
    pub capture_dropped: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RecvStats {
    pub packets: u64,
    pub lost: u64,
    pub concealed: u64,
    pub late: u64,
    pub invalid: u64,
    pub underruns: u64,
    pub overflows: u64,
    pub buffered_ms: f64,
    /// From the sender's RTCP reports: how long packets take from the
    /// sender's clock to here (meaningful when the clocks are synced).
    pub transit_ms: Option<f64>,
}

impl From<&receiver::ReceiverStats> for RecvStats {
    fn from(s: &receiver::ReceiverStats) -> Self {
        Self {
            packets: s.packets,
            lost: s.lost,
            concealed: s.concealed,
            late: s.late,
            invalid: s.invalid,
            underruns: s.underruns,
            overflows: s.overflows,
            buffered_ms: s.buffered.as_secs_f64() * 1000.0,
            transit_ms: s.transit_secs.map(|t| t * 1000.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum JobStats {
    Send(SendStats),
    Recv(RecvStats),
    Text(text::TextStats),
    FileSend(file::FileSendStats),
    FileRecv(file::FileRecvStats),
}

/// A job as reported to clients.
#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    /// Multicast group being sent to or received from (once known).
    pub group: Option<SocketAddr>,
    /// SAP name announced (send) or of the stream joined (recv).
    pub name: Option<String>,
    /// One line describing the stream, e.g. "PCMU/8000/1 sending to ...".
    pub summary: Option<String>,
    /// Notes worth showing, e.g. loudness normalisation or decode warnings.
    pub notes: Vec<String>,
    /// Unix time in milliseconds.
    pub created_ms: u64,
    pub ended_ms: Option<u64>,
    pub error: Option<String>,
    pub stats: Option<JobStats>,
    pub request: JobRequest,
}

#[derive(Debug)]
pub enum StartError {
    /// The request can't be carried out (bad file, no such stream, ...).
    Invalid(String),
    /// Another job is already sending to the group.
    Conflict { group: SocketAddr, job: JobId },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Invalid(m) => f.write_str(m),
            StartError::Conflict { group, job } => {
                write!(f, "{group} is already being sent to by job {job}")
            }
        }
    }
}

impl std::error::Error for StartError {}

pub(crate) fn invalid(e: anyhow::Error) -> StartError {
    StartError::Invalid(format!("{e:#}"))
}

struct Job {
    info: JobInfo,
    stop: Arc<AtomicBool>,
    /// The group this job sends to, reserved while it's active.
    sends_to: Option<SocketAddr>,
    /// Text channels: where typed text goes, and recent lines.
    text: Option<text::Channel>,
    /// Sender progress, and frames per packet and sample rate to turn it into seconds.
    progress: Option<(Arc<Progress>, u64, u32)>,
    thread: Option<JoinHandle<()>>,
}

impl Job {
    fn refresh(&mut self) {
        if let Some((p, fpp, rate)) = &self.progress {
            let packets = p.packets.load(Ordering::Relaxed);
            let prev = match &self.info.stats {
                Some(JobStats::Send(s)) => s.capture_dropped,
                _ => 0,
            };
            self.info.stats = Some(JobStats::Send(SendStats {
                packets,
                send_errors: p.send_errors.load(Ordering::Relaxed),
                seconds: (packets * fpp) as f64 / *rate as f64,
                capture_dropped: prev,
            }));
        }
    }
}

#[derive(Default)]
struct State {
    next_id: JobId,
    jobs: BTreeMap<JobId, Job>,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled whenever a job ends.
    ended: Condvar,
    sink: EventSink,
    default_iface: Interface,
    closing: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panicking job thread mustn't take the whole manager down.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, e: Event) {
        (self.sink)(e)
    }

    /// Add a job, reserving the group it sends to (if any). Fails if
    /// another active job already sends there.
    fn register(
        &self,
        info: JobInfo,
        stop: Arc<AtomicBool>,
        sends_to: Option<SocketAddr>,
    ) -> Result<JobInfo, StartError> {
        let mut st = self.lock();
        if let Some(group) = sends_to {
            if let Some(other) = st
                .jobs
                .values()
                .find(|j| j.info.state.is_active() && j.sends_to == Some(group))
            {
                return Err(StartError::Conflict {
                    group,
                    job: other.info.id,
                });
            }
        }
        st.next_id += 1;
        let info = JobInfo {
            id: st.next_id,
            ..info
        };
        st.jobs.insert(
            info.id,
            Job {
                info: info.clone(),
                stop,
                sends_to,
                text: None,
                progress: None,
                thread: None,
            },
        );
        Ok(info)
    }

    /// Keep a job's thread so shutdown can wait for it.
    fn attach(&self, id: JobId, thread: JoinHandle<()>) {
        if let Some(j) = self.lock().jobs.get_mut(&id) {
            j.thread = Some(thread);
        }
    }

    /// Forget a job that never got going.
    fn discard(&self, id: JobId) {
        self.lock().jobs.remove(&id);
    }

    fn update(&self, id: JobId, f: impl FnOnce(&mut JobInfo)) -> Option<JobInfo> {
        let mut st = self.lock();
        let job = st.jobs.get_mut(&id)?;
        f(&mut job.info);
        Some(job.info.clone())
    }

    /// Record how a job ended and tell subscribers.
    fn finish(&self, id: JobId, result: anyhow::Result<Option<JobStats>>) {
        let info = {
            let mut st = self.lock();
            let Some(job) = st.jobs.get_mut(&id) else {
                return;
            };
            job.refresh();
            let stopped = job.stop.load(Ordering::Relaxed);
            let i = &mut job.info;
            i.ended_ms = Some(now_ms());
            match result {
                Ok(stats) => {
                    if stats.is_some() {
                        i.stats = stats;
                    }
                    i.state = if stopped {
                        JobState::Stopped
                    } else {
                        JobState::Finished
                    };
                }
                Err(e) => {
                    i.state = JobState::Failed;
                    i.error = Some(format!("{e:#}"));
                }
            }
            let info = i.clone();
            prune(&mut st);
            info
        };
        self.ended.notify_all();
        self.emit(Event::JobEnded { job: info });
    }
}

fn prune(st: &mut State) {
    let finished: Vec<JobId> = st
        .jobs
        .iter()
        .filter(|(_, j)| !j.info.state.is_active())
        .map(|(&id, _)| id)
        .collect();
    for id in finished
        .iter()
        .take(finished.len().saturating_sub(KEEP_FINISHED))
    {
        st.jobs.remove(id);
    }
}

/// Runs senders and receivers in background threads.
pub struct Jobs {
    shared: Arc<Shared>,
    ticker: Mutex<Option<JoinHandle<()>>>,
}

impl Jobs {
    /// `default_iface` applies to requests that don't name an interface.
    /// Job events (started, stats about once a second, ended) go to `sink`.
    pub fn new(default_iface: Interface, sink: EventSink) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            ended: Condvar::new(),
            sink,
            default_iface,
            closing: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&shared);
        let ticker = std::thread::Builder::new()
            .name("mcast-jobs".into())
            .spawn(move || tick(weak))
            .expect("spawning job ticker");
        Self {
            shared,
            ticker: Mutex::new(Some(ticker)),
        }
    }

    pub fn list(&self) -> Vec<JobInfo> {
        let mut st = self.shared.lock();
        st.jobs
            .values_mut()
            .map(|j| {
                j.refresh();
                j.info.clone()
            })
            .collect()
    }

    pub fn get(&self, id: JobId) -> Option<JobInfo> {
        let mut st = self.shared.lock();
        let j = st.jobs.get_mut(&id)?;
        j.refresh();
        Some(j.info.clone())
    }

    /// Ask a job to stop and wait up to `wait` for it to end. Returns the
    /// job as it stands, or `None` if there's no such job.
    pub fn stop(&self, id: JobId, wait: Duration) -> Option<JobInfo> {
        let deadline = Instant::now() + wait;
        let mut st = self.shared.lock();
        let job = st.jobs.get_mut(&id)?;
        if job.info.state.is_active() {
            job.stop.store(true, Ordering::Relaxed);
            job.info.state = JobState::Stopping;
            let info = job.info.clone();
            drop(st);
            self.shared.emit(Event::JobUpdated { job: info });
            st = self.shared.lock();
        }
        loop {
            let job = st.jobs.get_mut(&id)?;
            let now = Instant::now();
            if !job.info.state.is_active() || now >= deadline {
                job.refresh();
                return Some(job.info.clone());
            }
            st = self
                .shared
                .ended
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Stop every job and wait for them to end (senders withdraw their SAP
    /// announcements as they go).
    pub fn shutdown(&self) {
        self.shared.closing.store(true, Ordering::Relaxed);
        let threads: Vec<JoinHandle<()>> = {
            let mut st = self.shared.lock();
            st.jobs
                .values_mut()
                .filter_map(|j| {
                    j.stop.store(true, Ordering::Relaxed);
                    j.thread.take()
                })
                .collect()
        };
        for t in threads {
            let _ = t.join();
        }
        let ticker = self.ticker.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(t) = ticker {
            let _ = t.join();
        }
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Publish running jobs' stats about once a second.
fn tick(shared: Weak<Shared>) {
    loop {
        std::thread::sleep(UPDATE_INTERVAL);
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if shared.closing.load(Ordering::Relaxed) {
            return;
        }
        let running: Vec<JobInfo> = {
            let mut st = shared.lock();
            st.jobs
                .values_mut()
                .filter(|j| j.info.state == JobState::Running)
                .map(|j| {
                    j.refresh();
                    j.info.clone()
                })
                .collect()
        };
        for job in running {
            shared.emit(Event::JobUpdated { job });
        }
    }
}

#[cfg(test)]
mod tests;
