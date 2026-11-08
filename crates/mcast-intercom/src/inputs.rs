//! Buttons and contacts on GPIO pins (`inputs` in the config): a press
//! plays an announcement, and for a `while_held` input, letting go stops it.
//!
//! The pins are read through [`Pins`]: on Linux, the GPIO character device
//! (`/dev/gpiochipN`) through `gpiocdev`, with the kernel doing the
//! debouncing; tests use [`FakePins`]. [`Inputs`] follows the config (it
//! re-reads it when poked, and every few seconds for edits made by hand),
//! watching each enabled input's pin, and reports each one's state.

use crate::model::{Config, Input};
use crate::now_ms;
use crate::runner::{RunId, Runner, Trigger};
use crate::store;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

/// Look at the config at least this often, for edits made by hand.
const RELOAD: Duration = Duration::from_secs(5);
/// Try a pin that couldn't be opened again after this long.
const RETRY: Duration = Duration::from_secs(30);

/// What a pin watcher hears.
pub enum PinEvent {
    /// Pressed (active) or not: first the level when watching starts, then
    /// each debounced change.
    Level(bool),
    /// The pin can't be read any more.
    Failed(String),
}

pub type OnPin = Box<dyn Fn(PinEvent) + Send + Sync + 'static>;

/// Stops watching when dropped.
pub trait Watching: Send {}

/// Somewhere pins can be watched.
pub trait Pins: Send + Sync + 'static {
    /// Watch an input's pin (its chip, line, active-low, bias and debounce)
    /// until the result is dropped. An error if it can't be opened.
    fn watch(&self, input: &Input, on: OnPin) -> Result<Box<dyn Watching>, String>;
}

/// The machine's own GPIO: the character device on Linux, nothing elsewhere.
pub fn system_pins() -> Arc<dyn Pins> {
    #[cfg(target_os = "linux")]
    return Arc::new(gpio::Gpio);
    #[cfg(not(target_os = "linux"))]
    return Arc::new(NoGpio);
}

#[cfg(not(target_os = "linux"))]
struct NoGpio;

#[cfg(not(target_os = "linux"))]
impl Pins for NoGpio {
    fn watch(&self, _: &Input, _: OnPin) -> Result<Box<dyn Watching>, String> {
        Err("GPIO inputs need Linux (a Raspberry Pi, say)".into())
    }
}

#[cfg(target_os = "linux")]
mod gpio {
    use super::{Input, OnPin, PinEvent, Pins, Watching};
    use crate::model::Bias;
    use gpiocdev::line::{Bias as LineBias, EdgeDetection, EdgeKind, Value};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    pub struct Gpio;

    struct Watch(Arc<AtomicBool>);

    impl Watching for Watch {}

    impl Drop for Watch {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    impl Pins for Gpio {
        fn watch(&self, input: &Input, on: OnPin) -> Result<Box<dyn Watching>, String> {
            let chip = input.chip_path();
            let mut b = gpiocdev::Request::builder();
            b.on_chip(&chip)
                .with_consumer("mcast-intercom")
                .with_line(input.line)
                .as_input()
                .with_bias(match input.bias {
                    Bias::PullUp => LineBias::PullUp,
                    Bias::PullDown => LineBias::PullDown,
                    Bias::None => LineBias::Disabled,
                })
                .with_edge_detection(EdgeDetection::BothEdges)
                .with_debounce_period(Duration::from_millis(input.debounce_ms().into()));
            if input.active_low {
                b.as_active_low();
            }
            let req = b
                .request()
                .map_err(|e| format!("{chip} line {}: {e}", input.line))?;
            let level = req
                .lone_value()
                .map_err(|e| format!("{chip} line {}: {e}", input.line))?;
            on(PinEvent::Level(level == Value::Active));
            let stop = Arc::new(AtomicBool::new(false));
            let done = stop.clone();
            std::thread::Builder::new()
                .name(format!("gpio-{}", input.line))
                .spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        // Short waits, so dropping the watch ends it soon.
                        match req.wait_edge_event(Duration::from_millis(250)) {
                            Ok(false) => {}
                            Ok(true) => match req.read_edge_event() {
                                Ok(e) => on(PinEvent::Level(e.kind == EdgeKind::Rising)),
                                Err(e) => {
                                    on(PinEvent::Failed(e.to_string()));
                                    return;
                                }
                            },
                            Err(e) => {
                                on(PinEvent::Failed(e.to_string()));
                                return;
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(Box::new(Watch(stop)))
        }
    }
}

/// An input as the page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Pressed now; `None` if it isn't being read.
    pub pressed: Option<bool>,
    /// Why it isn't being read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The last press or release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_ms: Option<u64>,
    /// The run its last press started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
}

type OnStatus = Box<dyn Fn(Vec<InputStatus>) + Send + Sync>;

struct Watched {
    /// As configured when it was opened: a change opens it again.
    input: Input,
    watch: Option<Box<dyn Watching>>,
    status: InputStatus,
    /// When opening failed, to try again later.
    failed_at: Option<std::time::Instant>,
}

#[derive(Default)]
struct State {
    inputs: BTreeMap<String, Watched>,
    poked: bool,
    stopping: bool,
}

struct Inner {
    pins: Arc<dyn Pins>,
    runner: Runner,
    config: PathBuf,
    st: Mutex<State>,
    wake: Condvar,
    on_status: OnStatus,
}

/// Watches the config's inputs and plays their announcements. Stops
/// watching when dropped.
pub struct Inputs(Arc<Inner>);

impl Inputs {
    /// Start following `config`'s inputs; `on_status` hears each change to
    /// their states (never with the lock held).
    pub fn start(
        config: PathBuf,
        runner: Runner,
        pins: Arc<dyn Pins>,
        on_status: impl Fn(Vec<InputStatus>) + Send + Sync + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            pins,
            runner,
            config,
            st: Mutex::default(),
            wake: Condvar::new(),
            on_status: Box::new(on_status),
        });
        inner.reload();
        let weak = Arc::downgrade(&inner);
        let _ = std::thread::Builder::new()
            .name("intercom-inputs".into())
            .spawn(move || loop {
                let Some(me) = weak.upgrade() else { return };
                {
                    let st = me.lock();
                    let (st, _) = me
                        .wake
                        .wait_timeout_while(st, RELOAD, |s| !s.poked && !s.stopping)
                        .unwrap_or_else(|e| e.into_inner());
                    if st.stopping {
                        return;
                    }
                }
                me.lock().poked = false;
                me.reload();
            });
        Inputs(inner)
    }

    /// The config changed: look again now.
    pub fn poke(&self) {
        self.0.lock().poked = true;
        self.0.wake.notify_all();
    }

    pub fn status(&self) -> Vec<InputStatus> {
        self.0.status()
    }

    /// Act as if input `id` was pressed (and, for a `while_held` one,
    /// released after `hold`): the Test button.
    pub fn test(&self, id: &str, hold: Duration) -> Result<Option<RunId>, String> {
        let input = {
            let st = self.0.lock();
            st.inputs
                .get(id)
                .map(|w| w.input.clone())
                .ok_or_else(|| format!("no input \"{id}\""))?
        };
        let run = self.0.pressed(&input, true)?;
        if input.while_held {
            let (inner, input) = (Arc::downgrade(&self.0), input.clone());
            std::thread::spawn(move || {
                std::thread::sleep(hold);
                if let Some(me) = inner.upgrade() {
                    let _ = me.pressed(&input, false);
                }
            });
        }
        Ok(run)
    }
}

impl Drop for Inputs {
    fn drop(&mut self) {
        let mut st = self.0.lock();
        st.stopping = true;
        for w in st.inputs.values_mut() {
            w.watch = None;
        }
        drop(st);
        self.0.wake.notify_all();
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn status(&self) -> Vec<InputStatus> {
        self.lock()
            .inputs
            .values()
            .map(|w| w.status.clone())
            .collect()
    }

    fn tell(&self) {
        (self.on_status)(self.status());
    }

    /// Match the watched pins to the config's inputs.
    fn reload(self: &Arc<Self>) {
        let cfg = match store::load(&self.config) {
            Ok(c) => c,
            // A broken file keeps what's watched; the panel says why elsewhere.
            Err(_) => return,
        };
        let mut changed = false;
        let mut open: Vec<Input> = Vec::new();
        {
            let mut st = self.lock();
            let wanted: BTreeMap<&str, &Input> =
                cfg.inputs.iter().map(|i| (i.id.as_str(), i)).collect();
            let gone: Vec<String> = st
                .inputs
                .keys()
                .filter(|id| !wanted.contains_key(id.as_str()))
                .cloned()
                .collect();
            for id in gone {
                st.inputs.remove(&id);
                changed = true;
            }
            for (id, i) in wanted {
                let w = st.inputs.entry(id.to_string()).or_insert_with(|| Watched {
                    input: (*i).clone(),
                    watch: None,
                    status: InputStatus {
                        id: i.id.clone(),
                        name: i.name.clone(),
                        enabled: i.enabled,
                        pressed: None,
                        error: None,
                        changed_ms: None,
                        run: None,
                    },
                    failed_at: None,
                });
                let same = w.input == *i;
                if !same {
                    // Close it, to open it as it is now.
                    w.input = (*i).clone();
                    w.watch = None;
                    w.failed_at = None;
                    w.status.name = i.name.clone();
                    w.status.enabled = i.enabled;
                    w.status.pressed = None;
                    w.status.error = None;
                    changed = true;
                }
                let retry = w.failed_at.is_none_or(|t| t.elapsed() >= RETRY);
                if i.enabled && w.watch.is_none() && retry {
                    open.push((*i).clone());
                }
            }
        }
        for i in open {
            self.open(&i);
            changed = true;
        }
        if changed {
            self.tell();
        }
    }

    fn open(self: &Arc<Self>, input: &Input) {
        let weak: Weak<Inner> = Arc::downgrade(self);
        let id = input.id.clone();
        let result = self.pins.watch(
            input,
            Box::new(move |e| {
                if let Some(me) = weak.upgrade() {
                    me.heard(&id, e);
                }
            }),
        );
        let mut st = self.lock();
        let Some(w) = st.inputs.get_mut(&input.id) else {
            return;
        };
        match result {
            Ok(watch) => {
                w.watch = Some(watch);
                w.failed_at = None;
                w.status.error = None;
            }
            Err(e) => {
                w.failed_at = Some(std::time::Instant::now());
                w.status.error = Some(e);
                w.status.pressed = None;
            }
        }
    }

    /// From a pin's watcher.
    fn heard(self: &Arc<Self>, id: &str, e: PinEvent) {
        let (input, first) = {
            let mut st = self.lock();
            let Some(w) = st.inputs.get_mut(id) else {
                return;
            };
            match e {
                PinEvent::Failed(msg) => {
                    w.watch = None;
                    w.failed_at = Some(std::time::Instant::now());
                    w.status.error = Some(msg);
                    w.status.pressed = None;
                    drop(st);
                    self.tell();
                    return;
                }
                PinEvent::Level(pressed) => {
                    let first = w.status.pressed.is_none();
                    if w.status.pressed == Some(pressed) {
                        return;
                    }
                    w.status.pressed = Some(pressed);
                    if !first {
                        w.status.changed_ms = Some(now_ms());
                    }
                    (w.input.clone(), first.then_some(pressed))
                }
            }
        };
        match first {
            // The level when watching started isn't a press: a contact that's
            // closed when the panel starts doesn't page.
            Some(_) => self.tell(),
            None => {
                let pressed = self.lock().inputs[id].status.pressed == Some(true);
                let _ = self.pressed(&input, pressed);
            }
        }
    }

    /// A press (or release) of `input`: play its announcement, or stop it.
    fn pressed(self: &Arc<Self>, input: &Input, down: bool) -> Result<Option<RunId>, String> {
        let last = self.lock().inputs.get(&input.id).and_then(|w| w.status.run);
        let playing = last
            .and_then(|r| self.runner.get(r))
            .is_some_and(|r| !r.state.is_over());
        let result = if down {
            if playing {
                // Pressed again while it still plays: once is enough.
                Ok(last)
            } else {
                let cfg: Config = store::load(&self.config).map_err(|e| format!("{e:#}"))?;
                let trigger = Trigger::Input {
                    id: input.id.clone(),
                    by: input.name.clone(),
                };
                match self.runner.play(&cfg, &input.announcement, trigger) {
                    Ok(run) => {
                        if let Some(w) = self.lock().inputs.get_mut(&input.id) {
                            w.status.run = Some(run.id);
                        }
                        Ok(Some(run.id))
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
        } else {
            if input.while_held && playing {
                if let Some(r) = last {
                    self.runner.stop(r);
                }
            }
            Ok(last)
        };
        self.tell();
        result
    }
}

/// Pins for tests: [`FakePins::set`] changes a level, as a button would.
#[derive(Default)]
pub struct FakePins {
    watching: Mutex<BTreeMap<(String, u32), Arc<OnPin>>>,
    /// Levels set before watching starts.
    levels: Mutex<BTreeMap<(String, u32), bool>>,
    /// Chips that "don't exist".
    pub missing: Mutex<Vec<String>>,
}

struct FakeWatch {
    pins: Weak<FakePins>,
    key: (String, u32),
}

impl Watching for FakeWatch {}

impl Drop for FakeWatch {
    fn drop(&mut self) {
        if let Some(p) = self.pins.upgrade() {
            p.watching
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.key);
        }
    }
}

impl FakePins {
    /// Set a pin's level (pressed or not), telling its watcher.
    pub fn set(&self, chip: &str, line: u32, pressed: bool) {
        let key = (chip.to_string(), line);
        self.levels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), pressed);
        let on = self
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned();
        if let Some(on) = on {
            on(PinEvent::Level(pressed));
        }
    }

    pub fn watched(&self) -> Vec<(String, u32)> {
        self.watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

impl Pins for Arc<FakePins> {
    fn watch(&self, input: &Input, on: OnPin) -> Result<Box<dyn Watching>, String> {
        let chip = input.chip_path();
        if self
            .missing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&chip)
        {
            return Err(format!("\"{chip}\" No such file or directory."));
        }
        let key = (chip, input.line);
        let level = self
            .levels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .copied()
            .unwrap_or(false);
        on(PinEvent::Level(level));
        self.watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), Arc::new(on));
        Ok(Box::new(FakeWatch {
            pins: Arc::downgrade(self),
            key,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::Fake;
    use crate::runner::RunState;
    use serde_json::json;
    use std::time::Instant;

    struct Rig {
        fake: Arc<Fake>,
        pins: Arc<FakePins>,
        runner: Runner,
        inputs: Inputs,
        path: PathBuf,
        told: Arc<Mutex<Vec<Vec<InputStatus>>>>,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
        }
    }

    fn config(inputs: serde_json::Value) -> Config {
        serde_json::from_value(json!({
            "zones": [{"id": "hall", "name": "Hall", "targets": [{"group": "239.1.1.1:5000"}]}],
            "announcements": [
                {"id": "alarm", "name": "Alarm", "audio": "chime_only", "zones": ["hall"]},
                {"id": "bell", "name": "Bell", "audio": "chime_only", "zones": ["hall"]}
            ],
            "inputs": inputs,
        }))
        .unwrap()
    }

    fn rig(inputs: serde_json::Value) -> Rig {
        let dir = std::env::temp_dir().join(format!(
            "mcast-inputs-test-{}-{}",
            std::process::id(),
            unique()
        ));
        let path = dir.join("intercom.json");
        store::save(&path, &config(inputs)).unwrap();
        let fake = Arc::new(Fake::default());
        let pins = Arc::new(FakePins::default());
        let runner = Runner::new(fake.clone(), |_| {});
        let told: Arc<Mutex<Vec<Vec<InputStatus>>>> = Arc::default();
        let t = told.clone();
        let inputs = Inputs::start(
            path.clone(),
            runner.clone(),
            Arc::new(pins.clone()),
            move |s| t.lock().unwrap().push(s),
        );
        Rig {
            fake,
            pins,
            runner,
            inputs,
            path,
            told,
        }
    }

    /// A different number each call, for temp dirs.
    fn unique() -> u128 {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            + N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u128
    }

    fn wait(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn status(r: &Rig, id: &str) -> InputStatus {
        r.inputs.status().into_iter().find(|s| s.id == id).unwrap()
    }

    #[test]
    fn a_press_plays_its_announcement_once() {
        let r = rig(
            json!([{"id": "panic", "name": "Panic button", "line": 17, "announcement": "alarm"}]),
        );
        assert_eq!(r.pins.watched(), [("/dev/gpiochip0".to_string(), 17)]);
        let s = status(&r, "panic");
        assert_eq!(s.pressed, Some(false));
        assert!(s.error.is_none() && s.run.is_none());

        r.pins.set("/dev/gpiochip0", 17, true);
        let run = status(&r, "panic").run.expect("a run");
        let played = r.runner.get(run).unwrap();
        assert_eq!(played.announcement, "alarm");
        assert_eq!(
            played.trigger,
            Trigger::Input {
                id: "panic".into(),
                by: "Panic button".into()
            }
        );
        assert!(status(&r, "panic").changed_ms.is_some());
        // Let go, press again while it plays: still the one run.
        r.pins.set("/dev/gpiochip0", 17, false);
        r.pins.set("/dev/gpiochip0", 17, true);
        assert_eq!(status(&r, "panic").run, Some(run));
        assert_eq!(r.fake.requests().len(), 1);
        assert!(
            !r.runner.get(run).unwrap().state.is_over(),
            "release doesn't stop it"
        );
        // Once it's over, a press plays it again.
        r.fake.finish_all();
        wait("the run to end", || {
            r.runner.get(run).unwrap().state.is_over()
        });
        r.pins.set("/dev/gpiochip0", 17, false);
        r.pins.set("/dev/gpiochip0", 17, true);
        assert_ne!(status(&r, "panic").run, Some(run));
        assert!(!r.told.lock().unwrap().is_empty());
    }

    #[test]
    fn a_held_input_stops_on_release() {
        let r = rig(
            json!([{"id": "door", "name": "Door", "line": 4, "announcement": "bell",
                             "while_held": true, "active_low": false, "bias": "pull_down"}]),
        );
        r.pins.set("/dev/gpiochip0", 4, true);
        let run = status(&r, "door").run.unwrap();
        assert_eq!(r.runner.get(run).unwrap().state, RunState::Playing);
        r.pins.set("/dev/gpiochip0", 4, false);
        wait("it to stop", || {
            r.runner.get(run).unwrap().state == RunState::Stopped
        });
    }

    #[test]
    fn a_contact_closed_at_start_doesnt_page() {
        let dir_cfg = json!([{"id": "door", "name": "Door", "line": 4, "announcement": "bell"}]);
        let pins = Arc::new(FakePins::default());
        pins.set("/dev/gpiochip0", 4, true);
        let dir = std::env::temp_dir().join(format!("mcast-inputs-closed-{}", unique()));
        let path = dir.join("intercom.json");
        store::save(&path, &config(dir_cfg)).unwrap();
        let fake = Arc::new(Fake::default());
        let inputs = Inputs::start(
            path,
            Runner::new(fake.clone(), |_| {}),
            Arc::new(pins.clone()),
            |_| {},
        );
        let s = &inputs.status()[0];
        assert_eq!(s.pressed, Some(true));
        assert!(s.run.is_none());
        assert!(fake.requests().is_empty());
        drop(inputs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn follows_the_config() {
        let r = rig(json!([
            {"id": "a", "name": "A", "line": 5, "announcement": "bell"},
            {"id": "off", "name": "Off", "line": 6, "announcement": "bell", "enabled": false},
            {"id": "b", "name": "B", "chip": "gpiochip1", "line": 5, "announcement": "bell"}
        ]));
        *r.pins.missing.lock().unwrap() = vec!["/dev/gpiochip1".into()];
        // (gpiochip1 was opened before it went missing; open the config again.)
        let mut cfg = store::load(&r.path).unwrap();
        cfg.inputs[2].line = 7;
        store::save(&r.path, &cfg).unwrap();
        r.inputs.poke();
        wait("b to fail", || status(&r, "b").error.is_some());
        assert!(status(&r, "b").error.unwrap().contains("gpiochip1"));
        assert_eq!(status(&r, "off").pressed, None);
        assert!(!status(&r, "off").enabled);
        assert_eq!(r.pins.watched(), [("/dev/gpiochip0".to_string(), 5)]);

        // Moved to another line, and one removed.
        cfg.inputs[0].line = 9;
        cfg.inputs.remove(1);
        store::save(&r.path, &cfg).unwrap();
        r.inputs.poke();
        wait("line 9", || {
            r.pins.watched() == [("/dev/gpiochip0".to_string(), 9)]
        });
        wait("off to go", || r.inputs.status().len() == 2);
    }

    #[test]
    fn the_test_button_presses_and_lets_go() {
        let r = rig(
            json!([{"id": "door", "name": "Door", "line": 4, "announcement": "bell",
                             "while_held": true}]),
        );
        let run = r
            .inputs
            .test("door", Duration::from_millis(50))
            .unwrap()
            .unwrap();
        wait("it to stop", || {
            r.runner.get(run).unwrap().state == RunState::Stopped
        });
        assert!(r.inputs.test("nope", Duration::ZERO).is_err());
    }
}
