//! Audio senders (pages) and receivers.

use super::{
    invalid, JobInfo, JobKind, JobRequest, JobState, JobStats, Jobs, RecvStats, SendStats,
    StartError,
};
use crate::request::{RecvRequest, Resolved, SendRequest};
use crate::{now_ms, Event};
use mcast_audio::sender::{Progress, Sender};
use mcast_audio::{receiver, StreamInfo};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

/// Why audio can't be streamed into a job.
#[derive(Debug, PartialEq, Eq)]
pub enum LiveError {
    NoJob,
    /// It isn't a sender with a live source.
    NotLive,
    Ended,
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LiveError::NoJob => "no such job",
            LiveError::NotLive => "that job doesn't have a live source",
            LiveError::Ended => "that job has ended",
        })
    }
}

impl std::error::Error for LiveError {}

impl Jobs {
    /// Where to push audio for a sender with a live source.
    pub fn live_feed(&self, id: super::JobId) -> Result<mcast_audio::live::LiveFeed, LiveError> {
        let st = self.shared.lock();
        let job = st.jobs.get(&id).ok_or(LiveError::NoJob)?;
        let feed = job.live.clone().ok_or(LiveError::NotLive)?;
        if !job.info.state.is_active() {
            return Err(LiveError::Ended);
        }
        Ok(feed)
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
        let info = JobInfo {
            id: 0,
            kind: JobKind::Send,
            state: JobState::Starting,
            group: sends.then_some(req.group),
            name: cfg.announce.as_ref().map(|a| a.name.clone()),
            summary: None,
            notes: Vec::new(),
            created_ms: now_ms(),
            ended_ms: None,
            error: None,
            stats: None,
            request: JobRequest::Send(shown),
        };
        // Reserves the group while the sender is prepared.
        let id = self
            .shared
            .register(info, stop.clone(), sends.then_some(req.group))?
            .id;
        if let mcast_audio::sender::Source::Live { feed } = &cfg.source {
            if let Some(job) = self.shared.lock().jobs.get_mut(&id) {
                job.live = Some(feed.clone());
            }
        }

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
                self.shared.discard(id);
                Err(invalid(e))
            }
            Err(_) => {
                let _ = thread.join();
                self.shared.discard(id);
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
        let info = self.shared.register(info, stop.clone(), None)?;
        let id = info.id;
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
            Ok(t) => self.shared.attach(id, t),
            Err(e) => {
                self.shared.discard(id);
                return Err(StartError::Invalid(format!("starting thread: {e}")));
            }
        }
        self.shared.emit(Event::JobStarted { job: info.clone() });
        Ok(info)
    }
}
