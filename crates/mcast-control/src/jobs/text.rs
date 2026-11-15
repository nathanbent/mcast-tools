//! Real-time text channels: a sender that clients type into, and a
//! receiver that turns everyone else's text into lines.

use super::{
    invalid, JobId, JobInfo, JobKind, JobRequest, JobState, JobStats, Jobs, Shared, StartError,
};
use crate::request::TextRequest;
use crate::{now_ms, Event};
use anyhow::anyhow;
use mcast_text::page::{self, Element, PageInfo, Priority};
use mcast_text::receiver::{self, TextReceiverConfig};
use mcast_text::sender::TextSender;
use mcast_text::session::TextInfo;
use mcast_text::{Entry, Transcript};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Lines kept per channel for clients that join late.
const HISTORY: usize = 200;
/// How long `start_text` waits for the receiver to be listening.
const READY_WAIT: Duration = Duration::from_secs(2);
/// Longest text accepted in one go, in characters.
pub const MAX_SEND: usize = 4000;

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
    /// The page it's a line of, when its sender said.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<PageRef>,
}

/// The page a line belongs to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageRef {
    pub id: String,
    /// What to head it with.
    pub name: String,
    /// `normal`, `high` or `emergency`.
    pub priority: &'static str,
    /// Seconds it stays up after it's cleared.
    pub hold_s: u32,
}

impl From<&PageInfo> for PageRef {
    fn from(p: &PageInfo) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            priority: p.priority.name(),
            hold_s: p.hold_s,
        }
    }
}

/// How long a page stays up after it's cleared, unless it says.
pub const DEFAULT_HOLD_S: u32 = 10;
/// The longest it can ask for.
pub const MAX_HOLD_S: u32 = 3600;

/// Sends text as a page: lines that go up together on displays that know
/// pages, with a name and a priority (see `docs/text-page-metadata.md`).
/// Other receivers show the lines.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PageRequest {
    /// Tells it from other pages (letters, digits, `.-_`); one is made up
    /// when it's left out. Clear the page by it.
    pub id: Option<String>,
    /// What displays head it with.
    pub name: String,
    /// `normal`, `high` or `emergency`.
    pub priority: String,
    /// Seconds it stays up after it's cleared.
    pub hold_s: u32,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            id: None,
            name: String::new(),
            priority: "normal".into(),
            hold_s: DEFAULT_HOLD_S,
        }
    }
}

impl PageRequest {
    fn info(&self) -> Result<PageInfo, TextError> {
        let bad = |m: String| Err(TextError::BadPage(m));
        let id = match &self.id {
            Some(id) if page::is_page_id(id) => id.clone(),
            Some(id) => {
                return bad(format!(
                    "\"{id}\" isn't a page id (letters, digits and .-_)"
                ))
            }
            None => format!("p{}.{:08x}", now_ms(), mcast_core::random_u32()),
        };
        let Some(priority) = Priority::parse(&self.priority) else {
            return bad(format!(
                "no priority \"{}\" (normal, high or emergency)",
                self.priority
            ));
        };
        if self.hold_s > MAX_HOLD_S {
            return bad(format!("hold_s is at most {MAX_HOLD_S}"));
        }
        Ok(PageInfo {
            id,
            name: mcast_plain::label(&mcast_plain::shown(&self.name)),
            priority,
            hold_s: self.hold_s,
        })
    }
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
    /// Something wrong with a page's id, priority or hold.
    BadPage(String),
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TextError::NoJob => "no such job",
            TextError::NotText => "that job isn't a text channel",
            TextError::NotSending => "that text channel only listens",
            TextError::Ended => "that text channel has ended",
            TextError::Empty => "nothing to send",
            TextError::BadPage(m) => return f.write_str(m),
            TextError::TooLong => {
                return write!(f, "too much text at once ({MAX_SEND} characters at most)")
            }
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

    /// Relabel the lines already received from `ssrc`, now its name is known.
    fn text_renamed(&self, id: JobId, ssrc: u32, name: &str) {
        {
            let mut st = self.lock();
            let Some(ch) = st.jobs.get_mut(&id).and_then(|j| j.text.as_mut()) else {
                return;
            };
            for l in ch.history.iter_mut() {
                if !l.own && l.ssrc == Some(ssrc) {
                    l.from = name.to_string();
                }
            }
        }
        self.emit(Event::TextRenamed {
            job: id,
            ssrc,
            name: name.to_string(),
        });
    }

    fn note(&self, id: JobId, text: String) {
        self.update(id, |i| i.notes.push(text));
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
            ssrc: None,
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

    /// Send text on a channel, a line at a time. Emoji shortcodes
    /// (`:fire:`) become emoji and what isn't safe to show is taken out
    /// ([`mcast_plain::shown`]). Returns the lines as sent and recorded.
    pub fn send_text(&self, id: JobId, text: &str) -> Result<Vec<TextLine>, TextError> {
        self.send(id, text, None)
    }

    /// Send text as a page (see [`PageRequest`]); blank lines are left out.
    pub fn send_page(
        &self,
        id: JobId,
        text: &str,
        page: &PageRequest,
    ) -> Result<Vec<TextLine>, TextError> {
        self.send(id, text, Some(page.info()?))
    }

    /// Take a page down on the displays that put it up: after its hold,
    /// or at once with `now`.
    pub fn clear_page(&self, id: JobId, page: &str, now: bool) -> Result<(), TextError> {
        if !page::is_page_id(page) {
            return Err(TextError::BadPage(format!("\"{page}\" isn't a page id")));
        }
        let (input, _) = self.text_input(id)?;
        let clear = Element::Clear {
            id: page.to_string(),
            now,
        };
        input.send(clear.encode()).map_err(|_| TextError::Ended)?;
        self.shared.emit(Event::TextCleared {
            job: id,
            ssrc: None,
            page: page.to_string(),
            now,
        });
        Ok(())
    }

    /// A sending channel's input and our name on it.
    fn text_input(&self, id: JobId) -> Result<(mpsc::Sender<String>, String), TextError> {
        let st = self.shared.lock();
        let job = st.jobs.get(&id).ok_or(TextError::NoJob)?;
        let ch = job.text.as_ref().ok_or(TextError::NotText)?;
        if !job.info.state.is_active() {
            return Err(TextError::Ended);
        }
        Ok((
            ch.input.clone().ok_or(TextError::NotSending)?,
            ch.name.clone(),
        ))
    }

    fn send(
        &self,
        id: JobId,
        text: &str,
        page: Option<PageInfo>,
    ) -> Result<Vec<TextLine>, TextError> {
        let text = mcast_plain::shown(text);
        let text = text.trim_end_matches('\n');
        if text.trim().is_empty() {
            return Err(TextError::Empty);
        }
        if text.chars().count() > MAX_SEND {
            return Err(TextError::TooLong);
        }
        let (input, name) = self.text_input(id)?;
        let page_ref = page.as_ref().map(PageRef::from);
        let text: Vec<&str> = match &page {
            // A page is its lines; a blank one would only be a gap on a sign.
            Some(_) => text.split('\n').filter(|l| !l.trim().is_empty()).collect(),
            None => text.split('\n').collect(),
        };
        if let Some(info) = &page {
            // In one piece, so nothing else on the channel lands inside it.
            input
                .send(page::encode_page(info, &text.join("\n")))
                .map_err(|_| TextError::Ended)?;
        }
        let mut lines = Vec::new();
        for line in text {
            if page.is_none() {
                input
                    .send(format!("{line}\n"))
                    .map_err(|_| TextError::Ended)?;
            }
            let line = TextLine {
                job: id,
                from: name.clone(),
                ssrc: None,
                text: line.to_string(),
                own: true,
                at_ms: now_ms(),
                page: page_ref.clone(),
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
    let mut transcript = Transcript::new();
    let show = |entries: Vec<Entry>, senders: usize| {
        for e in entries {
            match e {
                Entry::Line { ssrc, from, text } => shared.text_line(TextLine {
                    job: id,
                    from,
                    ssrc: Some(ssrc),
                    text,
                    own: false,
                    at_ms: now_ms(),
                    page: None,
                }),
                Entry::Page {
                    ssrc,
                    from,
                    info,
                    lines,
                } => {
                    let at_ms = now_ms();
                    for text in lines {
                        shared.text_line(TextLine {
                            job: id,
                            from: from.clone(),
                            ssrc: Some(ssrc),
                            text,
                            own: false,
                            at_ms,
                            page: Some(PageRef::from(&info)),
                        });
                    }
                }
                Entry::Clear {
                    ssrc,
                    id: page,
                    now,
                } => shared.emit(Event::TextCleared {
                    job: id,
                    ssrc: Some(ssrc),
                    page,
                    now,
                }),
                Entry::Joined { .. } => shared.text_stats(id, |t| t.senders = senders),
                // Relabel the lines already out rather than leave an address.
                Entry::Named {
                    ssrc,
                    name,
                    relabel: true,
                    ..
                } => shared.text_renamed(id, ssrc, &name),
                Entry::Named { .. } => {}
                Entry::Left { who, reason } => shared.note(
                    id,
                    match reason {
                        Some(r) => format!("{who} left: {r}"),
                        None => format!("{who} left"),
                    },
                ),
                Entry::UnknownPayloadType(pt) => {
                    shared.note(id, format!("ignoring payload type {pt}"))
                }
                Entry::Warning(message) => shared.note(id, message),
            }
        }
    };
    let result = receiver::run(cfg, stop, |e| {
        if let Some(r) = ready.take() {
            let _ = r.send(());
        }
        let entries = transcript.event(e, Instant::now());
        show(entries, transcript.senders());
    });
    show(transcript.finish(), transcript.senders());
    result
}
