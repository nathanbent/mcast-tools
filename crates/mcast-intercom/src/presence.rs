//! Whether the speakers zones expect (`listeners` in a zone) are there
//! between plays. Receivers (`mcast receive --name`) send RTCP reports every
//! few seconds even while idle, and `mcast serve`'s monitor lists those it
//! has heard from lately (forgetting one about 30 s after its last report,
//! or at once when it says goodbye). [`Presence`] asks every [`POLL`]; a
//! speaker missing for [`OFFLINE_AFTER`] is offline, and one that reports
//! again is back. Each change is reported once.
//!
//! While the monitor can't be asked (it's off, or the mcast server is
//! away), nothing is called offline: that isn't known.

use crate::backend::Backend;
use crate::now_ms;
use crate::store;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How often to ask the monitor.
pub const POLL: Duration = Duration::from_secs(10);
/// Missing this long: offline.
pub const OFFLINE_AFTER: Duration = Duration::from_secs(60);

/// One expected speaker.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Speaker {
    pub name: String,
    /// The zones that expect it (their names).
    pub zones: Vec<String>,
    /// Their sites (ids), each once; empty when they have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
    /// Reporting (true), offline (false), or not known yet (`null`: just
    /// added, or the monitor can't be asked).
    pub online: Option<bool>,
    /// Since when it's been online or offline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    /// Its last report seen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_ms: Option<u64>,
}

/// Every expected speaker, as `GET /speakers` gives it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Speakers {
    /// The monitor could be asked last time.
    pub known: bool,
    /// Why not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub speakers: Vec<Speaker>,
}

/// A speaker went offline, or came back.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeakerChange {
    pub speaker: Speaker,
    pub online: bool,
}

type OnChange = Box<dyn Fn(&Speakers, &[SpeakerChange]) + Send + Sync>;

struct Seen {
    speaker: Speaker,
    /// When it started being watched: missing since then counts.
    watched_ms: u64,
}

#[derive(Default)]
struct State {
    /// By name, any case.
    seen: BTreeMap<String, Seen>,
    status: Speakers,
    poked: bool,
    stopping: bool,
}

struct Inner {
    backend: Arc<dyn Backend>,
    config: PathBuf,
    poll: Duration,
    offline_after_ms: u64,
    st: Mutex<State>,
    wake: Condvar,
    on_change: OnChange,
}

/// Watches the expected speakers. Stops when dropped.
pub struct Presence(Arc<Inner>);

impl Presence {
    /// Start watching `config`'s zones' listeners through `backend`.
    /// `on_change` hears the state after each look that changed anything,
    /// with the speakers that went offline or came back (never with the
    /// lock held).
    pub fn start(
        config: PathBuf,
        backend: Arc<dyn Backend>,
        poll: Duration,
        offline_after: Duration,
        on_change: impl Fn(&Speakers, &[SpeakerChange]) + Send + Sync + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            backend,
            config,
            poll,
            offline_after_ms: offline_after.as_millis() as u64,
            st: Mutex::default(),
            wake: Condvar::new(),
            on_change: Box::new(on_change),
        });
        let weak = Arc::downgrade(&inner);
        let _ = std::thread::Builder::new()
            .name("intercom-speakers".into())
            .spawn(move || loop {
                let Some(me) = weak.upgrade() else { return };
                me.look();
                let st = me.lock();
                let (mut st, _) = me
                    .wake
                    .wait_timeout_while(st, me.poll, |s| !s.poked && !s.stopping)
                    .unwrap_or_else(|e| e.into_inner());
                if st.stopping {
                    return;
                }
                st.poked = false;
            });
        Presence(inner)
    }

    /// The config changed: look again now.
    pub fn poke(&self) {
        self.0.lock().poked = true;
        self.0.wake.notify_all();
    }

    pub fn status(&self) -> Speakers {
        self.0.lock().status.clone()
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        self.0.lock().stopping = true;
        self.0.wake.notify_all();
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn look(&self) {
        // Who's expected, and by which zones.
        let Ok(cfg) = store::load(&self.config) else {
            return;
        };
        // Name → (as written, zone names, site ids).
        type Wanted = BTreeMap<String, (String, Vec<String>, Vec<String>)>;
        let mut wanted: Wanted = BTreeMap::new();
        for z in &cfg.zones {
            for l in &z.listeners {
                let e = wanted
                    .entry(l.to_lowercase())
                    .or_insert_with(|| (l.clone(), Vec::new(), Vec::new()));
                e.1.push(z.name.clone());
                if let Some(site) = &z.site {
                    if !e.2.contains(site) {
                        e.2.push(site.clone());
                    }
                }
            }
        }
        // Nothing to watch: don't ask.
        let heard = if wanted.is_empty() {
            Ok(Vec::new())
        } else {
            self.backend.listeners()
        };
        let now = now_ms();
        let (status, changes) = {
            let mut st = self.lock();
            let before = st.status.clone();
            st.seen.retain(|k, _| wanted.contains_key(k));
            let (known, error) = match &heard {
                Ok(_) => (true, None),
                Err(e) if e.status == 404 => {
                    (false, Some("the mcast server's monitor is off".to_string()))
                }
                Err(e) => (false, Some(format!("can't ask the mcast server: {e}"))),
            };
            let present: Vec<String> = match &heard {
                Ok(list) => list
                    .iter()
                    .filter(|l| !l.left)
                    .filter_map(|l| l.label().map(str::to_lowercase))
                    .collect(),
                Err(_) => Vec::new(),
            };
            let mut changes = Vec::new();
            for (key, (name, zones, sites)) in &wanted {
                let s = st.seen.entry(key.clone()).or_insert_with(|| Seen {
                    speaker: Speaker {
                        name: name.clone(),
                        zones: Vec::new(),
                        sites: Vec::new(),
                        online: None,
                        since_ms: None,
                        last_seen_ms: None,
                    },
                    watched_ms: now,
                });
                s.speaker.name = name.clone();
                s.speaker.zones = zones.clone();
                s.speaker.sites = sites.clone();
                if !known {
                    // Not known: nobody is called offline, nor watched missing.
                    s.watched_ms = now;
                    continue;
                }
                let was = s.speaker.online;
                if present.contains(key) {
                    s.speaker.last_seen_ms = Some(now);
                    if was != Some(true) {
                        s.speaker.online = Some(true);
                        s.speaker.since_ms = Some(now);
                        // Back after being offline; a first sighting isn't news.
                        if was == Some(false) {
                            changes.push(SpeakerChange {
                                speaker: s.speaker.clone(),
                                online: true,
                            });
                        }
                    }
                } else {
                    let missing_since = s.speaker.last_seen_ms.unwrap_or(0).max(s.watched_ms);
                    if was != Some(false) && now >= missing_since + self.offline_after_ms {
                        s.speaker.online = Some(false);
                        s.speaker.since_ms = Some(missing_since);
                        changes.push(SpeakerChange {
                            speaker: s.speaker.clone(),
                            online: false,
                        });
                    }
                }
            }
            st.status = Speakers {
                known,
                error,
                speakers: st.seen.values().map(|s| s.speaker.clone()).collect(),
            };
            if st.status == before && changes.is_empty() {
                return;
            }
            (st.status.clone(), changes)
        };
        (self.on_change)(&status, &changes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Listener;
    use crate::fake::Fake;
    use serde_json::json;
    use std::time::Instant;

    fn listener(name: &str, left: bool) -> Listener {
        Listener {
            group: "239.1.1.1:5000".into(),
            name: Some(name.into()),
            cname: None,
            about: None,
            loss_percent: None,
            last_heard_ms: 100.0,
            left,
        }
    }

    struct Rig {
        fake: Arc<Fake>,
        presence: Presence,
        changes: Arc<Mutex<Vec<SpeakerChange>>>,
        dir: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig(listeners: serde_json::Value, monitor: Option<Vec<Listener>>) -> Rig {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mcast-presence-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let cfg: crate::model::Config = serde_json::from_value(json!({
            "zones": [
                {"id": "hall", "name": "Hall", "targets": [{"group": "239.1.1.1:5000"}], "listeners": listeners},
                {"id": "yard", "name": "Yard", "targets": [{"group": "239.1.1.2:5000"}], "listeners": ["lobby"]}
            ]
        }))
        .unwrap();
        store::save(&dir.join("intercom.json"), &cfg).unwrap();
        let fake = Arc::new(Fake::default());
        fake.st.lock().unwrap().listeners = monitor;
        let changes: Arc<Mutex<Vec<SpeakerChange>>> = Arc::default();
        let c = changes.clone();
        let presence = Presence::start(
            dir.join("intercom.json"),
            fake.clone(),
            Duration::from_millis(20),
            Duration::from_millis(300),
            move |_, ch| c.lock().unwrap().extend_from_slice(ch),
        );
        Rig {
            fake,
            presence,
            changes,
            dir,
        }
    }

    fn wait(what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The speaker, once the first look has found it.
    fn speaker(r: &Rig, name: &str) -> Speaker {
        let find = || {
            r.presence
                .status()
                .speakers
                .into_iter()
                .find(|s| s.name.eq_ignore_ascii_case(name))
        };
        wait(name, || find().is_some());
        find().unwrap()
    }

    #[test]
    fn says_when_a_speaker_goes_and_comes_back() {
        let r = rig(
            json!(["Lobby", "Office"]),
            Some(vec![listener("Lobby", false)]),
        );
        wait("Lobby online", || speaker(&r, "Lobby").online == Some(true));
        let lobby = speaker(&r, "Lobby");
        // Expected by both zones, named as the first one lists it.
        assert_eq!(lobby.zones, ["Hall", "Yard"]);
        assert!(r.presence.status().known);
        // Office was never there: offline once the wait is over, and said once.
        wait("Office offline", || {
            speaker(&r, "Office").online == Some(false)
        });
        std::thread::sleep(Duration::from_millis(100));
        let ch = r.changes.lock().unwrap().clone();
        assert_eq!(ch.len(), 1, "{ch:?}");
        assert_eq!(
            (ch[0].speaker.name.as_str(), ch[0].online),
            ("Office", false)
        );

        // Lobby says goodbye: offline after the wait, not at once.
        r.fake.st.lock().unwrap().listeners = Some(vec![listener("Lobby", true)]);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(speaker(&r, "Lobby").online, Some(true));
        wait("Lobby offline", || {
            speaker(&r, "Lobby").online == Some(false)
        });
        let since = speaker(&r, "Lobby").since_ms.unwrap();
        assert_eq!(Some(since), speaker(&r, "Lobby").last_seen_ms);

        // Both back.
        r.fake.st.lock().unwrap().listeners =
            Some(vec![listener("lobby", false), listener("Office", false)]);
        wait("both back", || {
            r.presence
                .status()
                .speakers
                .iter()
                .all(|s| s.online == Some(true))
        });
        let ch = r.changes.lock().unwrap().clone();
        let back: Vec<_> = ch
            .iter()
            .filter(|c| c.online)
            .map(|c| c.speaker.name.as_str())
            .collect();
        assert_eq!(back.len(), 2, "{ch:?}");
        assert_eq!(ch.len(), 4, "{ch:?}");
    }

    #[test]
    fn without_the_monitor_nobody_is_offline() {
        let r = rig(json!(["Office"]), None);
        wait("an error", || r.presence.status().error.is_some());
        std::thread::sleep(Duration::from_millis(300));
        let s = r.presence.status();
        assert!(!s.known);
        assert!(s.error.unwrap().contains("monitor is off"));
        assert_eq!(speaker(&r, "Office").online, None);
        assert!(r.changes.lock().unwrap().is_empty());
        // The mcast server away: the same.
        r.fake.st.lock().unwrap().listeners = Some(Vec::new());
        r.fake.st.lock().unwrap().down = true;
        std::thread::sleep(Duration::from_millis(300));
        assert!(r.changes.lock().unwrap().is_empty());
        // Back, and Office still missing: offline only after a full wait.
        r.fake.st.lock().unwrap().down = false;
        wait("known", || r.presence.status().known);
        assert_ne!(speaker(&r, "Office").online, Some(false));
        wait("Office offline", || {
            speaker(&r, "Office").online == Some(false)
        });
    }

    #[test]
    fn follows_the_config() {
        let r = rig(json!(["Office"]), Some(Vec::new()));
        wait("two speakers", || r.presence.status().speakers.len() == 2);
        let path = r.dir.join("intercom.json");
        let mut cfg = store::load(&path).unwrap();
        cfg.zones[0].listeners.clear();
        cfg.zones[1].listeners.clear();
        store::save(&path, &cfg).unwrap();
        r.presence.poke();
        wait("none", || r.presence.status().speakers.is_empty());
    }
}
