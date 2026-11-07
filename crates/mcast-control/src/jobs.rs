//! Senders and receivers running as background jobs that can be listed,
//! watched and stopped. Pages to a group that is already being sent to are
//! rejected rather than mixed or queued.

use crate::request::{RecvRequest, Resolved, SendRequest};
use crate::{now_ms, Event, EventSink};
use mcast_audio::sender::{Progress, Sender};
use mcast_audio::{receiver, StreamInfo};
use mcast_core::Interface;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Finished jobs kept for listing.
const KEEP_FINISHED: usize = 50;
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Send,
    Recv,
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

fn invalid(e: anyhow::Error) -> StartError {
    StartError::Invalid(format!("{e:#}"))
}

struct Job {
    info: JobInfo,
    stop: Arc<AtomicBool>,
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

    /// Start sending. Waits until the sender is ready (files opened and
    /// measured), so a bad request fails here rather than in the background.
    pub fn start_sender(&self, req: SendRequest) -> Result<JobInfo, StartError> {
        self.start_sender_as(req.clone(), req)
    }

    /// Like [`Jobs::start_sender`], listing the job with `shown` as its
    /// request (e.g. the media name a client gave rather than the full path).
    pub fn start_sender_as(
        &self,
        req: SendRequest,
        shown: SendRequest,
    ) -> Result<JobInfo, StartError> {
        let cfg = req.to_config(self.shared.default_iface).map_err(invalid)?;
        let sends = cfg.render.is_none();
        let stop = Arc::new(AtomicBool::new(false));
        let id = {
            let mut st = self.shared.lock();
            if sends {
                if let Some(other) = st.jobs.values().find(|j| {
                    j.info.kind == JobKind::Send
                        && j.info.state.is_active()
                        && j.info.group == Some(req.group)
                }) {
                    return Err(StartError::Conflict {
                        group: req.group,
                        job: other.info.id,
                    });
                }
            }
            st.next_id += 1;
            let id = st.next_id;
            st.jobs.insert(
                id,
                Job {
                    info: JobInfo {
                        id,
                        kind: JobKind::Send,
                        state: JobState::Starting,
                        // Reserves the group while the sender is prepared.
                        group: sends.then_some(req.group),
                        name: cfg.announce.as_ref().map(|a| a.name.clone()),
                        summary: None,
                        notes: Vec::new(),
                        created_ms: now_ms(),
                        ended_ms: None,
                        error: None,
                        stats: None,
                        request: JobRequest::Send(shown),
                    },
                    stop: stop.clone(),
                    progress: None,
                    thread: None,
                },
            );
            id
        };

        let (ready_tx, ready_rx) = mpsc::channel();
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("mcast-send-{id}"))
            .spawn(move || {
                let sender = match Sender::new(cfg) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let progress = Arc::new(Progress::default());
                let rate = sender.spec().sample_rate;
                let ready = (
                    crate::describe::send_summary(&sender, &req),
                    crate::describe::loudness_note(&sender),
                    progress.clone(),
                    sender.frames_per_packet() as u64,
                    rate,
                );
                if ready_tx.send(Ok(ready)).is_err() {
                    return;
                }
                let result = sender.run_with_progress(&stop, progress).map(|s| {
                    Some(JobStats::Send(SendStats {
                        packets: s.packets,
                        send_errors: s.send_errors,
                        seconds: s.frames as f64 / rate as f64,
                        capture_dropped: s.capture_dropped,
                    }))
                });
                shared.finish(id, result);
            })
            .map_err(|e| StartError::Invalid(format!("starting thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok((summary, note, progress, fpp, rate))) => {
                let info = {
                    let mut st = self.shared.lock();
                    let job = st.jobs.get_mut(&id).expect("starting jobs aren't pruned");
                    job.thread = Some(thread);
                    job.progress = Some((progress, fpp, rate));
                    // Stopped while starting: it ends as soon as it runs.
                    if job.info.state == JobState::Starting {
                        job.info.state = JobState::Running;
                    }
                    job.info.summary = Some(summary);
                    job.info.notes.extend(note);
                    job.refresh();
                    job.info.clone()
                };
                self.shared.emit(Event::JobStarted { job: info.clone() });
                Ok(info)
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                self.shared.lock().jobs.remove(&id);
                Err(invalid(e))
            }
            Err(_) => {
                let _ = thread.join();
                self.shared.lock().jobs.remove(&id);
                Err(StartError::Invalid("sender thread panicked".into()))
            }
        }
    }

    /// Start receiving. Streams in `known` (e.g. from a running
    /// [`crate::Announcements`]) are tried before waiting for SAP.
    pub fn start_receiver(
        &self,
        req: RecvRequest,
        known: &[StreamInfo],
    ) -> Result<JobInfo, StartError> {
        let iface = self.shared.default_iface;
        let resolved = req.direct().or_else(|| {
            known
                .iter()
                .find(|i| req.matches(i))
                .cloned()
                .map(Resolved::from_info)
        });
        let resolved = match resolved {
            Some(r) => r,
            None => {
                // Wait for an announcement, unless stopped by shutdown.
                let stop = AtomicBool::new(false);
                match req.resolve(iface, &stop, |_| {}).map_err(invalid)? {
                    Some(r) => r,
                    None => return Err(StartError::Invalid("stopped".into())),
                }
            }
        };
        let cfg = req.to_config(&resolved, iface).map_err(invalid)?;
        // Fail now rather than once the first packet arrives.
        if let receiver::Sink::Play { device } = &cfg.sink {
            mcast_audio::device::output_device(device.as_deref()).map_err(invalid)?;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let summary = match &resolved.stream {
            Some(spec) => format!("{spec} from {}", resolved.group),
            None => format!("{} (codec from payload type)", resolved.group),
        };
        let info = JobInfo {
            id: 0,
            kind: JobKind::Recv,
            state: JobState::Running,
            group: Some(resolved.group),
            name: resolved.announced.as_ref().map(|i| i.name.clone()),
            summary: Some(summary),
            notes: Vec::new(),
            created_ms: now_ms(),
            ended_ms: None,
            error: None,
            stats: Some(JobStats::Recv(RecvStats::default())),
            request: JobRequest::Recv(req),
        };
        let (id, info) = {
            let mut st = self.shared.lock();
            st.next_id += 1;
            let id = st.next_id;
            let info = JobInfo { id, ..info };
            st.jobs.insert(
                id,
                Job {
                    info: info.clone(),
                    stop: stop.clone(),
                    progress: None,
                    thread: None,
                },
            );
            (id, info)
        };
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("mcast-recv-{id}"))
            .spawn(move || {
                let result = receiver::run(cfg, &stop, |event| {
                    shared.update(id, |i| match event {
                        receiver::Event::Stats(s) => i.stats = Some(JobStats::Recv(s.into())),
                        receiver::Event::NewSource {
                            ssrc,
                            from,
                            payload_type,
                            spec,
                        } => i.notes.push(format!(
                            "stream from {from} (ssrc {ssrc:08x}, pt {payload_type}): decoding as {spec}"
                        )),
                        receiver::Event::UnknownPayloadType { payload_type } => i.notes.push(
                            format!("payload type {payload_type} is dynamic; set the encoding"),
                        ),
                        receiver::Event::DecodeError { message } => i.notes.push(message),
                        receiver::Event::SenderInfo { ssrc, cname, name } => {
                            let who = name.or(cname).unwrap_or_else(|| "sender".into());
                            i.notes.push(format!("ssrc {ssrc:08x} is \"{who}\" (RTCP)"))
                        }
                        receiver::Event::Bye { ssrc, reason } => i.notes.push(match reason {
                            Some(r) => format!("ssrc {ssrc:08x} ended the stream: {r}"),
                            None => format!("ssrc {ssrc:08x} ended the stream"),
                        }),
                        receiver::Event::Warning { message } => i.notes.push(message),
                    });
                });
                let result = result.map(|s| Some(JobStats::Recv((&s).into())));
                shared.finish(id, result);
            });
        match thread {
            Ok(t) => {
                if let Some(j) = self.shared.lock().jobs.get_mut(&id) {
                    j.thread = Some(t);
                }
            }
            Err(e) => {
                self.shared.lock().jobs.remove(&id);
                return Err(StartError::Invalid(format!("starting thread: {e}")));
            }
        }
        self.shared.emit(Event::JobStarted { job: info.clone() });
        Ok(info)
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
mod tests {
    use super::*;
    use crate::request::SourceRequest;

    fn tone(group: &str) -> SendRequest {
        SendRequest {
            group: group.parse().unwrap(),
            sap: false,
            ..SendRequest::default()
        }
    }

    fn jobs() -> (Jobs, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        let jobs = Jobs::new(
            Interface::Any,
            Arc::new(move |ev| e.lock().unwrap().push(ev)),
        );
        (jobs, events)
    }

    #[test]
    fn rejects_a_second_page_to_the_same_group() {
        let (jobs, _) = jobs();
        let first = jobs.start_sender(tone("239.255.77.11:5004")).unwrap();
        assert_eq!(first.state, JobState::Running);
        match jobs.start_sender(tone("239.255.77.11:5004")) {
            Err(StartError::Conflict { job, .. }) => assert_eq!(job, first.id),
            other => panic!("expected a conflict, got {other:?}"),
        }
        // Another group is fine.
        let other = jobs.start_sender(tone("239.255.77.12:5004")).unwrap();
        // Once the first stops, its group is free again.
        let stopped = jobs.stop(first.id, Duration::from_secs(5)).unwrap();
        assert_eq!(stopped.state, JobState::Stopped);
        let again = jobs.start_sender(tone("239.255.77.11:5004")).unwrap();
        jobs.stop(again.id, Duration::from_secs(5));
        jobs.stop(other.id, Duration::from_secs(5));
    }

    #[test]
    fn bad_requests_fail_up_front_and_leave_no_job() {
        let (jobs, _) = jobs();
        let req = SendRequest {
            source: SourceRequest::File {
                path: "/nonexistent/page.wav".into(),
                looped: false,
            },
            ..tone("239.255.77.13:5004")
        };
        assert!(matches!(
            jobs.start_sender(req),
            Err(StartError::Invalid(_))
        ));
        assert!(jobs.list().is_empty());
    }

    #[test]
    fn finished_jobs_report_stats_and_events() {
        let (jobs, events) = jobs();
        let req = SendRequest {
            duration_s: Some(0.2),
            tail_ms: 0.0,
            ..tone("239.255.77.14:5004")
        };
        let job = jobs.start_sender(req).unwrap();
        let done = jobs.stop_when_done(job.id);
        assert_eq!(done.state, JobState::Finished);
        let Some(JobStats::Send(s)) = done.stats else {
            panic!("no send stats");
        };
        assert!(s.packets > 0 && (s.seconds - 0.2).abs() < 0.05, "{s:?}");
        let events = events.lock().unwrap();
        assert!(matches!(events.first(), Some(Event::JobStarted { .. })));
        assert!(matches!(events.last(), Some(Event::JobEnded { .. })));
    }

    #[test]
    fn receives_from_a_sender() {
        let (jobs, _) = jobs();
        let rx = jobs
            .start_receiver(
                RecvRequest {
                    group: Some("239.255.77.15:5004".parse().unwrap()),
                    sink: crate::request::SinkRequest::Wav {
                        path: std::env::temp_dir().join("mcast-control-test.wav"),
                    },
                    stats_interval_s: 0.1,
                    ..RecvRequest::default()
                },
                &[],
            )
            .unwrap();
        let tx = jobs
            .start_sender(SendRequest {
                duration_s: Some(0.3),
                ..tone("239.255.77.15:5004")
            })
            .unwrap();
        jobs.stop_when_done(tx.id);
        let rx = jobs.stop(rx.id, Duration::from_secs(5)).unwrap();
        let Some(JobStats::Recv(s)) = rx.stats else {
            panic!("no receive stats");
        };
        assert!(s.packets > 100, "{s:?}");
    }

    impl Jobs {
        fn stop_when_done(&self, id: JobId) -> JobInfo {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let j = self.get(id).unwrap();
                if !j.state.is_active() || Instant::now() > deadline {
                    return j;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
