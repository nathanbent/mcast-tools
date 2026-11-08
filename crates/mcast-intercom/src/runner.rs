//! Playing announcements: one sender per multicast group, followed until
//! they end, with repeats, stop, and priorities for groups that are busy.
//!
//! The runner never holds its lock while calling the backend, so backends
//! may report events from inside their own calls.

use crate::backend::{Backend, Feed, Job};
use crate::model::{Announcement, Config, Priority};
use crate::now_ms;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

pub type RunId = u64;

/// How many ended runs to remember.
const KEEP_ENDED: usize = 50;
/// Ended jobs remembered in case their event beats the start reply.
const KEEP_EARLY: usize = 64;
/// After taking a group over, how often to try it again while the old job winds down.
const TAKEOVER_TRIES: usize = 10;
const TAKEOVER_WAIT: Duration = Duration::from_millis(200);

/// What started a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// A button, push to talk, or the command line; `by` names the token
    /// used, if any.
    Manual {
        #[serde(skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    /// The trigger API (`/trigger/...`); `by` names the token used, if any.
    Api {
        #[serde(skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    /// A schedule, for the time it was due.
    Schedule { id: String, at_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Playing,
    /// Between repeats.
    Waiting,
    Stopping,
    /// Played to at least one group.
    Done,
    Stopped,
    /// Played to no group at all.
    Failed,
    /// A schedule's time passed while the panel couldn't play it.
    Missed,
}

impl RunState {
    pub fn is_over(self) -> bool {
        matches!(
            self,
            RunState::Done | RunState::Stopped | RunState::Failed | RunState::Missed
        )
    }
}

/// A group's state in the current (or last) round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    Starting,
    Playing,
    /// Played to the end.
    Done,
    /// Another job was sending to the group, and this announcement may not take it over.
    Busy,
    /// A higher-priority announcement took the group over.
    Preempted,
    Stopped,
    Failed,
}

impl TargetState {
    fn is_active(self) -> bool {
        matches!(self, TargetState::Starting | TargetState::Playing)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetRun {
    /// Which of the announcement's parts it plays (from 1); `None` for its own audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<usize>,
    /// As written in the zone: a phonebook name or an address.
    pub group: String,
    /// The address it resolved to; `None` if it couldn't be.
    pub address: Option<String>,
    pub state: TargetState,
    /// The sender job on the mcast server, once started.
    pub job: Option<u64>,
    pub error: Option<String>,
    /// Rounds that played to the end.
    pub played: u32,
    #[serde(skip)]
    request: Option<Value>,
    #[serde(skip)]
    preempted: bool,
}

/// One play of an announcement.
#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: RunId,
    /// Announcement id.
    pub announcement: String,
    pub name: String,
    pub priority: Priority,
    pub trigger: Trigger,
    pub state: RunState,
    /// Which repeat is playing, from 1.
    pub round: u32,
    pub rounds: u32,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Why it failed.
    pub error: Option<String>,
    pub targets: Vec<TargetRun>,
    #[serde(skip)]
    gap: Duration,
}

#[cfg(test)]
impl Run {
    /// A bare run, for tests elsewhere.
    pub(crate) fn for_tests(id: RunId) -> Self {
        Run {
            id,
            announcement: "a".into(),
            name: "A".into(),
            priority: Default::default(),
            trigger: Trigger::Manual { by: None },
            state: RunState::Playing,
            round: 1,
            rounds: 1,
            started_ms: 0,
            ended_ms: None,
            error: None,
            targets: Vec::new(),
            gap: Duration::ZERO,
        }
    }
}

struct State {
    next_id: RunId,
    runs: BTreeMap<RunId, Run>,
    /// Server job id → run and target index.
    jobs: HashMap<u64, (RunId, usize)>,
    /// Ended jobs that weren't (yet) ours.
    early: VecDeque<Job>,
}

/// Whether the runner can hear from the mcast server.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackendStatus {
    /// Following the server's events.
    pub connected: bool,
    /// Why not.
    pub error: Option<String>,
    /// When this status began.
    pub since_ms: u64,
}

/// What [`Runner::new`]'s callback hears about.
#[derive(Debug, Clone, Copy)]
pub enum Change<'a> {
    Run(&'a Run),
    Backend(&'a BackendStatus),
    /// Media files or folders changed on the mcast server.
    Media,
}

/// Why an announcement couldn't be played at all.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayError {
    NoAnnouncement(String),
    /// The mcast server couldn't be asked something it had to be.
    Server(String),
}

impl std::fmt::Display for PlayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlayError::NoAnnouncement(w) => write!(f, "no announcement \"{w}\""),
            PlayError::Server(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for PlayError {}

type OnChange = Box<dyn Fn(Change) + Send + Sync>;

struct Inner {
    backend: Arc<dyn Backend>,
    state: Mutex<State>,
    status: Mutex<BackendStatus>,
    on_change: OnChange,
}

/// Plays announcements and keeps track of them. Cheap to clone.
#[derive(Clone)]
pub struct Runner(Arc<Inner>);

impl Runner {
    /// Start following the backend's events. `on_change` hears about every
    /// change to a run, and to the connection (from any thread, never while
    /// the runner is locked).
    pub fn new(
        backend: Arc<dyn Backend>,
        on_change: impl Fn(Change) + Send + Sync + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            backend,
            state: Mutex::new(State {
                next_id: 0,
                runs: BTreeMap::new(),
                jobs: HashMap::new(),
                early: VecDeque::new(),
            }),
            status: Mutex::new(BackendStatus {
                connected: false,
                error: None,
                since_ms: now_ms(),
            }),
            on_change: Box::new(on_change),
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        inner.backend.follow(Box::new(move |feed| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            match feed {
                Feed::Connected => {
                    inner.set_status(true, None);
                    inner.reconcile();
                }
                Feed::JobEnded(job) => inner.job_ended(job),
                Feed::Disconnected(e) => inner.set_status(false, Some(e)),
                Feed::MediaChanged => (inner.on_change)(Change::Media),
            }
            true
        }));
        Runner(inner)
    }

    /// Play an announcement (by id or name). Returns once every group has
    /// been started (or found busy); follow the rest through `on_change`.
    pub fn play(&self, cfg: &Config, which: &str, trigger: Trigger) -> Result<Run, PlayError> {
        self.play_at(cfg, which, trigger, None)
    }

    /// Play an announcement to its zones at `site` only (all of them for
    /// `None`), as a site's schedule does.
    pub fn play_at(
        &self,
        cfg: &Config,
        which: &str,
        trigger: Trigger,
        site: Option<&str>,
    ) -> Result<Run, PlayError> {
        let a = &cfg
            .announcement(which)
            .ok_or_else(|| PlayError::NoAnnouncement(which.to_string()))?
            .with_defaults(&cfg.defaults);
        let targets = self.0.targets(cfg, a, site)?;
        let id = {
            let mut st = self.0.lock();
            st.next_id += 1;
            let id = st.next_id;
            st.runs.insert(
                id,
                Run {
                    id,
                    announcement: a.id.clone(),
                    name: a.name.clone(),
                    priority: a.priority,
                    trigger,
                    state: RunState::Playing,
                    round: 1,
                    rounds: a.repeat.max(1),
                    started_ms: now_ms(),
                    ended_ms: None,
                    error: None,
                    targets,
                    gap: Duration::from_secs_f64(a.repeat_gap_s.max(0.0)),
                },
            );
            prune(&mut st);
            id
        };
        self.0.start_round(id);
        self.get(id)
            .ok_or_else(|| PlayError::Server(format!("run {id} vanished")))
    }

    /// Remember a play that never started: one that was missed, or that
    /// failed before reaching any group (the server was away).
    pub fn record(
        &self,
        a: &Announcement,
        trigger: Trigger,
        state: RunState,
        error: String,
    ) -> Run {
        let id = {
            let mut st = self.0.lock();
            st.next_id += 1;
            let id = st.next_id;
            let now = now_ms();
            st.runs.insert(
                id,
                Run {
                    id,
                    announcement: a.id.clone(),
                    name: a.name.clone(),
                    priority: a.priority,
                    trigger,
                    state,
                    round: 1,
                    rounds: a.repeat.max(1),
                    started_ms: now,
                    ended_ms: Some(now),
                    error: Some(error),
                    targets: Vec::new(),
                    gap: Duration::ZERO,
                },
            );
            prune(&mut st);
            id
        };
        self.0.emit(id);
        self.get(id).expect("just added")
    }

    pub fn backend_status(&self) -> BackendStatus {
        self.0
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stop a run: its senders, and any repeats still to come.
    pub fn stop(&self, id: RunId) -> Option<Run> {
        self.0.stop(id);
        self.get(id)
    }

    /// Stop every run that hasn't ended.
    pub fn stop_all(&self) {
        let ids: Vec<RunId> = {
            let st = self.0.lock();
            st.runs
                .values()
                .filter(|r| !r.state.is_over())
                .map(|r| r.id)
                .collect()
        };
        std::thread::scope(|s| {
            for id in ids {
                s.spawn(move || self.0.stop(id));
            }
        });
    }

    pub fn get(&self, id: RunId) -> Option<Run> {
        self.0.lock().runs.get(&id).cloned()
    }

    /// Runs still going and recently ended ones, newest first.
    pub fn list(&self) -> Vec<Run> {
        self.0.lock().runs.values().rev().cloned().collect()
    }

    /// Ask the server about every job still playing, in case an event was missed.
    pub fn reconcile(&self) {
        self.0.reconcile();
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, id: RunId) {
        let run = self.lock().runs.get(&id).cloned();
        if let Some(run) = run {
            (self.on_change)(Change::Run(&run));
        }
    }

    /// Record the connection; tell listeners only when it changes (the
    /// backend retries every few seconds while the server is away).
    fn set_status(&self, connected: bool, error: Option<String>) {
        let now = {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if s.connected == connected && s.error == error {
                return;
            }
            *s = BackendStatus {
                connected,
                error,
                since_ms: now_ms(),
            };
            s.clone()
        };
        (self.on_change)(Change::Backend(&now));
    }

    /// The groups an announcement goes to, resolved and without repeats.
    fn targets(
        &self,
        cfg: &Config,
        a: &Announcement,
        site: Option<&str>,
    ) -> Result<Vec<TargetRun>, PlayError> {
        // Each group with the audio its section plays there.
        let wanted: Vec<(&crate::model::Audio, Option<usize>, &crate::model::Target)> = a
            .sections()
            .into_iter()
            .enumerate()
            .flat_map(|(i, (audio, zones))| {
                cfg.zones_in(zones, site)
                    .into_iter()
                    .flat_map(|z| &z.targets)
                    .map(move |t| (audio, (i > 0).then_some(i), t))
            })
            .collect();
        let book = if wanted
            .iter()
            .any(|(_, _, t)| t.group.trim().parse::<SocketAddr>().is_err())
        {
            self.backend
                .phonebook()
                .map_err(|e| PlayError::Server(format!("reading the phonebook: {e}")))?
        } else {
            Vec::new()
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (audio, part, t) in wanted {
            let name = t.group.trim();
            let address = match name.parse::<SocketAddr>() {
                Ok(a) => Some(a.to_string()),
                Err(_) => book
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case(name))
                    .map(|e| e.group.clone()),
            };
            // The same group through two zones, or by name and address, plays once.
            let key = address.clone().unwrap_or_else(|| name.to_lowercase());
            if !seen.insert(key) {
                continue;
            }
            let target = cfg.defaults.target(t);
            let request = address
                .as_deref()
                .map(|g| a.send_request(cfg, audio, &target, g));
            out.push(TargetRun {
                part,
                group: t.group.clone(),
                state: if request.is_some() {
                    TargetState::Starting
                } else {
                    TargetState::Failed
                },
                error: address
                    .is_none()
                    .then(|| format!("\"{name}\" isn't in the phonebook")),
                address,
                job: None,
                played: 0,
                request,
                preempted: false,
            });
        }
        Ok(out)
    }

    /// Start every group of a run's current round, all at once.
    fn start_round(self: &Arc<Self>, id: RunId) {
        let (priority, todo) = {
            let mut st = self.lock();
            let Some(run) = st.runs.get_mut(&id) else {
                return;
            };
            if run.state.is_over() || run.state == RunState::Stopping {
                return;
            }
            run.state = RunState::Playing;
            let mut todo = Vec::new();
            for (i, t) in run.targets.iter_mut().enumerate() {
                if let Some(req) = &t.request {
                    t.state = TargetState::Starting;
                    t.job = None;
                    t.error = None;
                    t.preempted = false;
                    todo.push((i, req.clone()));
                }
            }
            (run.priority, todo)
        };
        self.emit(id);
        std::thread::scope(|s| {
            for (i, req) in &todo {
                s.spawn(move || self.start_target(id, *i, req, priority));
            }
        });
        self.check_round(id);
    }

    fn start_target(self: &Arc<Self>, id: RunId, i: usize, req: &Value, priority: Priority) {
        let mut result = self.backend.start_sender(req);
        if let Err(e) = &result {
            if let Some(blocker) = e.job.filter(|_| e.is_conflict()) {
                if self.may_take_over(priority, blocker) {
                    // Ignore errors: it may have ended by itself meanwhile.
                    let _ = self.backend.stop_job(blocker);
                    for _ in 0..TAKEOVER_TRIES {
                        result = self.backend.start_sender(req);
                        if !matches!(&result, Err(e) if e.is_conflict()) {
                            break;
                        }
                        std::thread::sleep(TAKEOVER_WAIT);
                    }
                }
            }
        }
        match result {
            Ok(job) => self.started(id, i, job),
            Err(e) => {
                let state = if e.is_conflict() {
                    TargetState::Busy
                } else {
                    TargetState::Failed
                };
                {
                    let mut st = self.lock();
                    if let Some(t) = st.runs.get_mut(&id).and_then(|r| r.targets.get_mut(i)) {
                        t.state = state;
                        t.error = Some(e.message);
                    }
                }
                self.emit(id);
            }
        }
    }

    /// Whether an announcement of `priority` may stop job `blocker` to use
    /// its group. If the job is one of ours, it's marked as taken over.
    fn may_take_over(&self, priority: Priority, blocker: u64) -> bool {
        if priority == Priority::Normal {
            return false;
        }
        let mut st = self.lock();
        let Some(&(other, i)) = st.jobs.get(&blocker) else {
            // Not ours: a page from the web GUI, a test tone, ...
            return true;
        };
        let Some(run) = st.runs.get_mut(&other) else {
            return true;
        };
        if run.priority >= priority {
            return false;
        }
        if let Some(t) = run.targets.get_mut(i) {
            t.preempted = true;
        }
        true
    }

    fn started(self: &Arc<Self>, id: RunId, i: usize, job: Job) {
        let (stop_now, ended) = {
            let mut st = self.lock();
            let stop_now = match st.runs.get_mut(&id) {
                Some(run) => {
                    if let Some(t) = run.targets.get_mut(i) {
                        t.job = Some(job.id);
                        t.state = TargetState::Playing;
                    }
                    run.state == RunState::Stopping
                }
                None => true,
            };
            st.jobs.insert(job.id, (id, i));
            let early = st.early.iter().position(|j| j.id == job.id);
            let ended = match early {
                Some(p) => st.early.remove(p),
                None => job.is_over().then(|| job.clone()),
            };
            (stop_now, ended)
        };
        self.emit(id);
        if let Some(j) = ended {
            self.job_ended(j);
        } else if stop_now {
            self.stop_job(job.id);
        }
    }

    fn job_ended(self: &Arc<Self>, job: Job) {
        let id = {
            let mut st = self.lock();
            let Some((id, i)) = st.jobs.remove(&job.id) else {
                st.early.push_back(job);
                while st.early.len() > KEEP_EARLY {
                    st.early.pop_front();
                }
                return;
            };
            let Some(t) = st.runs.get_mut(&id).and_then(|r| r.targets.get_mut(i)) else {
                return;
            };
            if t.preempted {
                t.state = TargetState::Preempted;
                t.error = Some("taken over by a higher-priority announcement".into());
            } else {
                match job.state.as_str() {
                    "finished" => {
                        t.state = TargetState::Done;
                        t.played += 1;
                    }
                    "stopped" => t.state = TargetState::Stopped,
                    _ => {
                        t.state = TargetState::Failed;
                        t.error = Some(job.error.unwrap_or_else(|| "failed".into()));
                    }
                }
            }
            id
        };
        self.emit(id);
        self.check_round(id);
    }

    /// When no group of the round is still going: repeat, or end the run.
    fn check_round(self: &Arc<Self>, id: RunId) {
        let next = {
            let mut st = self.lock();
            let Some(run) = st.runs.get_mut(&id) else {
                return;
            };
            if run.state.is_over()
                || run.state == RunState::Waiting
                || run.targets.iter().any(|t| t.state.is_active())
            {
                return;
            }
            if run.state == RunState::Stopping {
                finish(run, RunState::Stopped);
                None
            } else if run.round < run.rounds {
                run.state = RunState::Waiting;
                Some(run.gap)
            } else if run.targets.iter().any(|t| t.played > 0) {
                finish(run, RunState::Done);
                None
            } else {
                run.error = Some(nothing_played(run));
                finish(run, RunState::Failed);
                None
            }
        };
        self.emit(id);
        if let Some(gap) = next {
            let me = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(gap);
                {
                    let mut st = me.lock();
                    let Some(run) = st.runs.get_mut(&id) else {
                        return;
                    };
                    if run.state != RunState::Waiting {
                        return;
                    }
                    run.round += 1;
                }
                me.start_round(id);
            });
        }
    }

    fn stop(self: &Arc<Self>, id: RunId) {
        let jobs: Vec<u64> = {
            let mut st = self.lock();
            let Some(run) = st.runs.get_mut(&id) else {
                return;
            };
            match run.state {
                s if s.is_over() => return,
                RunState::Waiting => {
                    finish(run, RunState::Stopped);
                    Vec::new()
                }
                _ => {
                    run.state = RunState::Stopping;
                    run.targets
                        .iter()
                        .filter(|t| t.state == TargetState::Playing)
                        .filter_map(|t| t.job)
                        .collect()
                }
            }
        };
        self.emit(id);
        std::thread::scope(|s| {
            for j in jobs {
                s.spawn(move || self.stop_job(j));
            }
        });
        self.check_round(id);
    }

    /// Stop a job and, in case its event goes astray, check it ended.
    fn stop_job(self: &Arc<Self>, job: u64) {
        let _ = self.backend.stop_job(job);
        if let Ok(j) = self.backend.job(job) {
            if j.is_over() && self.lock().jobs.contains_key(&job) {
                self.job_ended(j);
            }
        }
    }

    fn reconcile(self: &Arc<Self>) {
        let jobs: Vec<u64> = self.lock().jobs.keys().copied().collect();
        for id in jobs {
            let ended = match self.backend.job(id) {
                Ok(j) => j.is_over().then_some(j),
                Err(e) if e.status == 404 => Some(Job {
                    id,
                    state: "failed".into(),
                    group: None,
                    error: Some("the mcast server no longer has this job".into()),
                }),
                Err(_) => None,
            };
            if let Some(j) = ended {
                if self.lock().jobs.contains_key(&id) {
                    self.job_ended(j);
                }
            }
        }
    }
}

fn finish(run: &mut Run, state: RunState) {
    run.state = state;
    run.ended_ms = Some(now_ms());
}

fn nothing_played(run: &Run) -> String {
    let count = |s: TargetState| run.targets.iter().filter(|t| t.state == s).count();
    let mut parts = Vec::new();
    for (s, what) in [
        (TargetState::Busy, "busy"),
        (TargetState::Failed, "failed"),
        (TargetState::Preempted, "taken over"),
        (TargetState::Stopped, "stopped"),
    ] {
        let n = count(s);
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    }
    if parts.is_empty() {
        "nothing played".into()
    } else {
        format!("nothing played: {}", parts.join(", "))
    }
}

/// Forget the oldest ended runs beyond [`KEEP_ENDED`].
fn prune(st: &mut State) {
    let ended: Vec<RunId> = st
        .runs
        .values()
        .filter(|r| r.state.is_over())
        .map(|r| r.id)
        .collect();
    for id in ended.iter().take(ended.len().saturating_sub(KEEP_ENDED)) {
        st.runs.remove(id);
    }
}
