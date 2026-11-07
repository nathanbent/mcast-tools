//! Real-time text channels: a sender that clients type into, and a
//! receiver that turns everyone else's text into lines.

use super::{
    invalid, JobId, JobInfo, JobKind, JobRequest, JobState, JobStats, Jobs, Shared, StartError,
};
use crate::request::TextRequest;
use crate::{now_ms, Event};
use anyhow::anyhow;
use mcast_text::lines::LineBuffer;
use mcast_text::receiver::{self, TextReceiverConfig};
use mcast_text::sender::TextSender;
use mcast_text::session::TextInfo;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Lines kept per channel for clients that join late.
const HISTORY: usize = 200;
/// Show a half-typed line once its sender has been quiet this long.
const STALE_LINE: Duration = Duration::from_secs(3);
/// How long `start_text` waits for the receiver to be listening.
const READY_WAIT: Duration = Duration::from_secs(2);
/// Longest text accepted in one go.
const MAX_SEND: usize = 4000;

/// A line on a text channel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextLine {
    pub job: JobId,
    /// Who sent it: their RTCP name, else their address; ours for our own.
    pub from: String,
    pub ssrc: Option<u32>,
    pub text: String,
    /// Typed here rather than received.
    pub own: bool,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TextStats {
    pub lines_sent: u64,
    pub lines_received: u64,
    pub chars_sent: u64,
    pub packets_sent: u64,
    /// Others heard on the channel.
    pub senders: usize,
}

/// A text job's input and recent lines.
pub(super) struct Channel {
    /// `None` when only listening.
    input: Option<mpsc::Sender<String>>,
    name: String,
    history: VecDeque<TextLine>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TextError {
    NoJob,
    /// The job isn't a text channel.
    NotText,
    /// The channel only listens.
    NotSending,
    Ended,
    Empty,
    TooLong,
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TextError::NoJob => "no such job",
            TextError::NotText => "that job isn't a text channel",
            TextError::NotSending => "that text channel only listens",
            TextError::Ended => "that text channel has ended",
            TextError::Empty => "nothing to send",
            TextError::TooLong => "too much text at once (4000 characters at most)",
        })
    }
}

impl std::error::Error for TextError {}

impl Shared {
    /// Keep a line and tell subscribers.
    fn text_line(&self, line: TextLine) {
        {
            let mut st = self.lock();
            let Some(job) = st.jobs.get_mut(&line.job) else {
                return;
            };
            if let Some(Some(JobStats::Text(s))) = Some(job.info.stats.as_mut()) {
                if line.own {
                    s.lines_sent += 1;
                } else {
                    s.lines_received += 1;
                }
            }
            if let Some(ch) = job.text.as_mut() {
                if ch.history.len() == HISTORY {
                    ch.history.pop_front();
                }
                ch.history.push_back(line.clone());
            }
        }
        self.emit(Event::TextLine { line });
    }

    fn text_stats(&self, id: JobId, f: impl FnOnce(&mut TextStats)) {
        let mut st = self.lock();
        if let Some(JobStats::Text(s)) = st.jobs.get_mut(&id).and_then(|j| j.info.stats.as_mut()) {
            f(s);
        }
    }
}

impl Jobs {
    /// Join a text channel: send what clients type (unless listening only)
    /// and report everyone else's lines. Announced streams in `known` can be
    /// joined by name.
    pub fn start_text(&self, req: TextRequest, known: &[TextInfo]) -> Result<JobInfo, StartError> {
        let iface = self.shared.default_iface;
        let target = req.target(known).map_err(invalid)?;
        let name = req.name();
        let stop = Arc::new(AtomicBool::new(false));
        let doing = if req.send { "chatting" } else { "listening" };
        let info = JobInfo {
            id: 0,
            kind: JobKind::Text,
            state: JobState::Running,
            group: Some(target.group),
            name: Some(target.announced.clone().unwrap_or_else(|| name.clone())),
            summary: Some(format!("{doing} on {} as \"{name}\"", target.group)),
            notes: Vec::new(),
            created_ms: now_ms(),
            ended_ms: None,
            error: None,
            stats: Some(JobStats::Text(TextStats::default())),
            request: JobRequest::Text(req.clone()),
        };
        let info = self
            .shared
            .register(info, stop.clone(), req.send.then_some(target.group))?;
        let id = info.id;
        let fail = |e: anyhow::Error| {
            self.shared.discard(id);
            invalid(e)
        };
        let sender = match req.send {
            true => Some(
                req.sender_config(&target, iface)
                    .and_then(TextSender::new)
                    .map_err(fail)?,
            ),
            false => None,
        };
        let rx_cfg = req
            .receiver_config(&target, iface, sender.as_ref().map(TextSender::ssrc))
            .map_err(fail)?;
        let (input, typed) = match sender {
            Some(_) => {
                let (tx, rx) = mpsc::channel();
                (Some(tx), Some(rx))
            }
            None => (None, None),
        };
        if let Some(j) = self.shared.lock().jobs.get_mut(&id) {
            j.text = Some(Channel {
                input,
                name,
                history: VecDeque::new(),
            });
        }
        // Set once the receiver is listening, so lines sent right after
        // this returns aren't missed.
        let (ready_tx, ready_rx) = mpsc::channel();
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("mcast-text-{id}"))
            .spawn(move || {
                let result = std::thread::scope(|scope| {
                    let heard = scope.spawn(|| receive(rx_cfg, &stop, &shared, id, ready_tx));
                    let sent = match (sender, typed) {
                        (Some(mut s), Some(typed)) => drive(&mut s, typed, &stop, &shared, id),
                        _ => Ok(()),
                    };
                    let heard = heard
                        .join()
                        .unwrap_or_else(|_| Err(anyhow!("text receiver panicked")));
                    sent.and(heard)
                });
                shared.finish(id, result.map(|_| None));
            });
        match thread {
            Ok(t) => self.shared.attach(id, t),
            Err(e) => {
                self.shared.discard(id);
                return Err(StartError::Invalid(format!("starting thread: {e}")));
            }
        }
        match ready_rx.recv_timeout(READY_WAIT) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The receiver gave up before listening (e.g. couldn't join).
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let failed = self.stop(id, Duration::from_secs(2));
                self.shared.discard(id);
                let why = failed.and_then(|j| j.error);
                return Err(StartError::Invalid(
                    why.unwrap_or_else(|| "the text receiver didn't start".into()),
                ));
            }
        }
        self.shared.emit(Event::JobStarted { job: info.clone() });
        Ok(info)
    }

    /// Send text on a channel; each line goes out as typed. Returns the
    /// lines as recorded.
    pub fn send_text(&self, id: JobId, text: &str) -> Result<Vec<TextLine>, TextError> {
        let text = text.trim_end_matches(['\r', '\n']);
        if text.trim().is_empty() {
            return Err(TextError::Empty);
        }
        if text.chars().count() > MAX_SEND {
            return Err(TextError::TooLong);
        }
        let (input, name) = {
            let st = self.shared.lock();
            let job = st.jobs.get(&id).ok_or(TextError::NoJob)?;
            let ch = job.text.as_ref().ok_or(TextError::NotText)?;
            if !job.info.state.is_active() {
                return Err(TextError::Ended);
            }
            (
                ch.input.clone().ok_or(TextError::NotSending)?,
                ch.name.clone(),
            )
        };
        let mut lines = Vec::new();
        for line in text.split('\n').map(|l| l.trim_end_matches('\r')) {
            input
                .send(format!("{line}\n"))
                .map_err(|_| TextError::Ended)?;
            let line = TextLine {
                job: id,
                from: name.clone(),
                ssrc: None,
                text: line.to_string(),
                own: true,
                at_ms: now_ms(),
            };
            self.shared.text_line(line.clone());
            lines.push(line);
        }
        Ok(lines)
    }

    /// Recent lines on a text channel, oldest first.
    pub fn text_history(&self, id: JobId) -> Result<Vec<TextLine>, TextError> {
        let st = self.shared.lock();
        let job = st.jobs.get(&id).ok_or(TextError::NoJob)?;
        let ch = job.text.as_ref().ok_or(TextError::NotText)?;
        Ok(ch.history.iter().cloned().collect())
    }
}

/// Feed typed text to the sender until stopped, then flush.
fn drive(
    s: &mut TextSender,
    typed: mpsc::Receiver<String>,
    stop: &AtomicBool,
    shared: &Shared,
    id: JobId,
) -> anyhow::Result<()> {
    let mut reported = (0, 0);
    while !stop.load(Ordering::Relaxed) {
        let wait = s
            .next_wake()
            .map_or(Duration::MAX, |t| {
                t.saturating_duration_since(Instant::now())
            })
            .min(Duration::from_millis(200));
        if let Ok(text) = typed.recv_timeout(wait) {
            s.push(&text);
            while let Ok(more) = typed.try_recv() {
                s.push(&more);
            }
        }
        s.poll()?;
        let st = s.stats();
        if (st.chars, st.packets) != reported {
            reported = (st.chars, st.packets);
            shared.text_stats(id, |t| {
                t.chars_sent = st.chars;
                t.packets_sent = st.packets;
            });
        }
    }
    s.flush()?;
    Ok(())
}

/// Turn everyone's text into lines until stopped.
fn receive(
    cfg: TextReceiverConfig,
    stop: &AtomicBool,
    shared: &Shared,
    id: JobId,
    ready: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    // The receiver reports its first event (at least a tick) once its socket is open.
    let mut ready = Some(ready);
    let mut labels: HashMap<u32, String> = HashMap::new();
    let mut buffers: HashMap<u32, LineBuffer> = HashMap::new();
    let line = |labels: &HashMap<u32, String>, ssrc: u32, text: String| TextLine {
        job: id,
        from: labels
            .get(&ssrc)
            .cloned()
            .unwrap_or_else(|| format!("{ssrc:08x}")),
        ssrc: Some(ssrc),
        text,
        own: false,
        at_ms: now_ms(),
    };
    let note = |text: String| {
        shared.update(id, |i| i.notes.push(text));
    };
    let result = receiver::run(cfg, stop, |e| {
        if let Some(r) = ready.take() {
            let _ = r.send(());
        }
        let now = Instant::now();
        match e {
            receiver::Event::NewSource { ssrc, from } => {
                labels.entry(ssrc).or_insert_with(|| from.ip().to_string());
                let senders = labels.len();
                shared.text_stats(id, |t| t.senders = senders);
            }
            receiver::Event::Named { ssrc, name } => {
                labels.insert(ssrc, name);
            }
            receiver::Event::Text { ssrc, text } => {
                let buf = buffers.entry(ssrc).or_insert_with(|| LineBuffer::new(now));
                for l in buf.push(text, now) {
                    shared.text_line(line(&labels, ssrc, l));
                }
            }
            receiver::Event::Bye { ssrc, reason } => {
                if let Some(l) = buffers.get_mut(&ssrc).and_then(LineBuffer::take) {
                    shared.text_line(line(&labels, ssrc, l));
                }
                let who = labels
                    .get(&ssrc)
                    .cloned()
                    .unwrap_or_else(|| format!("{ssrc:08x}"));
                note(match reason {
                    Some(r) => format!("{who} left: {r}"),
                    None => format!("{who} left"),
                });
            }
            receiver::Event::UnknownPayloadType { payload_type } => {
                note(format!("ignoring payload type {payload_type}"))
            }
            receiver::Event::Warning { message } => note(message),
            receiver::Event::Tick => {
                for (&ssrc, buf) in buffers.iter_mut() {
                    if let Some(l) = buf.take_stale(now, STALE_LINE) {
                        shared.text_line(line(&labels, ssrc, l));
                    }
                }
            }
        }
    });
    for (ssrc, mut buf) in buffers {
        if let Some(l) = buf.take() {
            shared.text_line(line(&labels, ssrc, l));
        }
    }
    result
}
