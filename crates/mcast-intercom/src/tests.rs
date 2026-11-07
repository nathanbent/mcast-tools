//! The model, the store, and the runner against a fake mcast server.

use crate::backend::Backend;
use crate::fake::Fake;
use crate::model::{Announcement, Audio, Config, Priority, Target, Zone};
use crate::runner::{Run, RunId, RunState, Runner, TargetState, Trigger};
use crate::store;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---- helpers ----

fn target(group: &str) -> Target {
    Target {
        group: group.into(),
        ..Default::default()
    }
}

fn announcement(id: &str, zones: &[&str]) -> Announcement {
    Announcement {
        id: id.into(),
        name: format!("{id} name"),
        audio: Audio::Media("lunch.wav".into()),
        chime: None,
        end_chime: None,
        mode: None,
        volume_db: None,
        lead_in_ms: None,
        repeat: 1,
        repeat_gap_s: 0.0,
        zones: zones.iter().map(|z| z.to_string()).collect(),
        priority: Priority::Normal,
    }
}

/// Two zones that share a group (once by phonebook name, once by address),
/// plus one name that isn't in the phonebook.
fn config() -> Config {
    Config {
        zones: vec![
            Zone {
                id: "up".into(),
                name: "Upstairs".into(),
                targets: vec![
                    Target {
                        group: "zone-1".into(),
                        encoding: Some("g711u".into()),
                        volume_db: Some(-3.0),
                        ..Default::default()
                    },
                    target("239.1.1.2:5000"),
                ],
            },
            Zone {
                id: "down".into(),
                name: "Downstairs".into(),
                targets: vec![target("239.1.1.1:5000"), target("nobody")],
            },
            Zone {
                id: "two".into(),
                name: "Just two".into(),
                targets: vec![target("239.1.1.2:5000")],
            },
        ],
        schedules: Vec::new(),
        time_zone: None,
        defaults: Default::default(),
        schedules_paused: false,
        days_off: Vec::new(),
        announcements: vec![
            Announcement {
                volume_db: Some(2.0),
                chime: Some(json!("ding-dong")),
                ..announcement("lunch", &["up", "down"])
            },
            announcement("two", &["two"]),
        ],
    }
}

fn runner(fake: &Arc<Fake>) -> Runner {
    Runner::new(fake.clone(), |_| {})
}

fn wait_for(r: &Runner, id: RunId, what: &str, done: impl Fn(&Run) -> bool) -> Run {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let run = r.get(id).expect("run");
        if done(&run) {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {run:#?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn states(run: &Run) -> Vec<TargetState> {
    run.targets.iter().map(|t| t.state).collect()
}

// ---- the runner ----

#[test]
fn plays_to_each_group_once() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let run = r.play(&config(), "LUNCH name", Trigger::Manual).unwrap();
    assert_eq!(run.announcement, "lunch");
    let groups: Vec<_> = run.targets.iter().map(|t| t.group.as_str()).collect();
    assert_eq!(groups, ["zone-1", "239.1.1.2:5000", "nobody"]);
    assert_eq!(
        states(&run),
        [
            TargetState::Playing,
            TargetState::Playing,
            TargetState::Failed
        ]
    );
    assert_eq!(run.targets[0].address.as_deref(), Some("239.1.1.1:5000"));
    assert!(run.targets[2].error.as_ref().unwrap().contains("phonebook"));

    let mut reqs = fake.requests();
    reqs.sort_by_key(|r| r["group"].as_str().unwrap().to_string());
    assert_eq!(
        reqs[0],
        json!({
            "group": "239.1.1.1:5000", "name": "lunch name",
            "source": {"type": "file", "path": "lunch.wav"},
            "chime": "ding-dong", "encoding": "g711u", "volume_db": -1.0,
        })
    );
    assert_eq!(reqs[1]["volume_db"], json!(2.0));
    assert!(reqs[1].get("encoding").is_none());

    fake.finish_all();
    let run = wait_for(&r, run.id, "the end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done);
    assert_eq!(run.targets[0].played, 1);
    assert!(run.ended_ms.is_some());
}

#[test]
fn normal_priority_skips_busy_groups() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let other = fake
        .start_sender(&json!({"group": "239.1.1.2:5000"}))
        .unwrap();
    let run = r.play(&config(), "two", Trigger::Manual).unwrap();
    assert_eq!(states(&run), [TargetState::Busy]);
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.error.as_deref(), Some("nothing played: 1 busy"));
    assert_eq!(fake.running_on("239.1.1.2:5000"), Some(other.id));

    let run = r.play(&config(), "lunch", Trigger::Manual).unwrap();
    assert_eq!(
        states(&run),
        [TargetState::Playing, TargetState::Busy, TargetState::Failed]
    );
    fake.end(other.id, "finished");
    fake.finish_all();
    let run = wait_for(&r, run.id, "the end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done);
}

#[test]
fn higher_priorities_take_groups_over() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let mut cfg = config();
    let ann = |id: &str, p| Announcement {
        priority: p,
        ..announcement(id, &["two"])
    };
    cfg.announcements = vec![
        ann("normal", Priority::Normal),
        ann("high", Priority::High),
        ann("high2", Priority::High),
        ann("emergency", Priority::Emergency),
    ];

    // High takes over from something that isn't ours...
    let outside = fake
        .start_sender(&json!({"group": "239.1.1.2:5000"}))
        .unwrap();
    let high = r.play(&cfg, "high", Trigger::Manual).unwrap();
    assert_eq!(states(&high), [TargetState::Playing]);
    assert_eq!(fake.job(outside.id).unwrap().state, "stopped");

    // ...but not from another high one, and normal never does.
    for id in ["high2", "normal"] {
        let run = r.play(&cfg, id, Trigger::Manual).unwrap();
        assert_eq!(states(&run), [TargetState::Busy], "{id}");
    }

    // Emergency does.
    let emergency = r.play(&cfg, "emergency", Trigger::Manual).unwrap();
    assert_eq!(states(&emergency), [TargetState::Playing]);
    let high = wait_for(&r, high.id, "the takeover", |r| r.state.is_over());
    assert_eq!(states(&high), [TargetState::Preempted]);
    assert_eq!(high.state, RunState::Failed);
    assert_eq!(r.list()[0].id, emergency.id, "newest first");
}

#[test]
fn repeats_every_group_each_round() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let mut cfg = config();
    cfg.announcements[0].repeat = 3;
    let run = r.play(&cfg, "lunch", Trigger::Manual).unwrap();
    for round in 1..=3 {
        let run = wait_for(&r, run.id, "a round to start", |r| {
            r.round == round && r.targets[0].state == TargetState::Playing
        });
        assert_eq!(run.state, RunState::Playing);
        fake.finish_all();
    }
    let run = wait_for(&r, run.id, "the end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done);
    assert_eq!(run.targets[0].played, 3);
    assert_eq!(fake.requests().len(), 6);
}

#[test]
fn stops_playing_and_waiting_runs() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let mut cfg = config();
    let run = r.play(&cfg, "lunch", Trigger::Manual).unwrap();
    let run = r.stop(run.id).unwrap();
    assert_eq!(run.state, RunState::Stopped);
    assert_eq!(
        states(&run),
        [
            TargetState::Stopped,
            TargetState::Stopped,
            TargetState::Failed
        ]
    );
    assert_eq!(fake.running_on("239.1.1.1:5000"), None);

    cfg.announcements[1].repeat = 2;
    cfg.announcements[1].repeat_gap_s = 60.0;
    let run = r.play(&cfg, "two", Trigger::Manual).unwrap();
    fake.finish_all();
    wait_for(&r, run.id, "the gap", |r| r.state == RunState::Waiting);
    let run = r.stop(run.id).unwrap();
    assert_eq!(run.state, RunState::Stopped);
    assert_eq!(run.round, 1);
}

#[test]
fn stop_all_stops_everything() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let a = r.play(&config(), "two", Trigger::Manual).unwrap();
    let mut cfg = config();
    cfg.announcements[0].zones = vec!["down".into()];
    let b = r.play(&cfg, "lunch", Trigger::Manual).unwrap();
    r.stop_all();
    for id in [a.id, b.id] {
        assert_eq!(r.get(id).unwrap().state, RunState::Stopped);
    }
}

#[test]
fn an_end_reported_before_the_start_reply_counts() {
    let fake = Arc::new(Fake::default());
    fake.st.lock().unwrap().end_at_once = true;
    let r = runner(&fake);
    let run = r.play(&config(), "two", Trigger::Manual).unwrap();
    let run = wait_for(&r, run.id, "the end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done);
}

#[test]
fn reconcile_catches_missed_events() {
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    let run = r.play(&config(), "two", Trigger::Manual).unwrap();
    fake.st.lock().unwrap().quiet = true;
    fake.finish_all();
    assert_eq!(r.get(run.id).unwrap().state, RunState::Playing);
    r.reconcile();
    assert_eq!(r.get(run.id).unwrap().state, RunState::Done);
}

#[test]
fn unknown_announcements_are_an_error() {
    let fake = Arc::new(Fake::default());
    let e = runner(&fake)
        .play(&config(), "fire drill", Trigger::Manual)
        .unwrap_err();
    assert_eq!(e.to_string(), "no announcement \"fire drill\"");
}

// ---- model and store ----

#[test]
fn reads_the_documented_format() {
    let cfg: Config = serde_json::from_value(json!({
        "zones": [{"id": "up", "name": "Upstairs", "targets": [
            {"group": "zone-1", "encoding": "g711u", "tos": 184},
            {"group": "239.192.2.2:10000", "volume_db": -3}
        ]}],
        "announcements": [
            {"id": "lunch", "name": "Lunch bell", "audio": {"media": "lunch.wav"},
             "chime": "default", "mode": "voice", "repeat": 2, "zones": ["up"]},
            {"id": "fire", "name": "Fire", "audio": "chime_only",
             "chime": {"file": "alarm.wav"}, "zones": ["up"], "priority": "emergency"}
        ]
    }))
    .unwrap();
    cfg.validate().unwrap();
    let fire = cfg.announcement("FIRE").unwrap();
    assert_eq!(fire.priority, Priority::Emergency);
    assert_eq!(fire.repeat, 1);
    assert_eq!(fire.repeat_gap_s, 2.0);
    assert_eq!(
        fire.send_request(&cfg.zones[0].targets[0], "239.1.1.1:5000"),
        json!({
            "group": "239.1.1.1:5000", "name": "Fire", "source": {"type": "chime"},
            "chime": {"file": "alarm.wav"}, "encoding": "g711u", "tos": 184,
        })
    );
    // Unknown fields are refused, so typos don't go unnoticed.
    let typo = json!({"id": "a", "name": "A", "audio": "chime_only", "zones": [], "repeats": 2});
    assert!(serde_json::from_value::<Announcement>(typo).is_err());
}

#[test]
fn announcements_use_the_defaults_they_leave_out() {
    let cfg: Config = serde_json::from_value(json!({
        "zones": [{"id": "up", "name": "Upstairs", "targets": [{"group": "239.1.1.1:5000"}]}],
        "defaults": {"chime": "ding-dong", "end_chime": "falling", "mode": "voice",
                     "volume_db": -3, "lead_in_ms": 300},
        "announcements": [
            {"id": "plain", "name": "Plain", "audio": {"media": "a.wav"}, "zones": ["up"]},
            {"id": "own", "name": "Own", "audio": {"media": "a.wav"}, "zones": ["up"],
             "chime": "bell", "end_chime": "none", "mode": "music", "volume_db": 0}
        ]
    }))
    .unwrap();
    cfg.validate().unwrap();
    let request = |id: &str| {
        cfg.announcement(id)
            .unwrap()
            .with_defaults(&cfg.defaults)
            .send_request(&cfg.zones[0].targets[0], "239.1.1.1:5000")
    };
    assert_eq!(
        request("plain"),
        json!({
            "group": "239.1.1.1:5000", "name": "Plain", "source": {"type": "file", "path": "a.wav"},
            "chime": "ding-dong", "end_chime": "falling", "mode": "voice",
            "volume_db": -3.0, "lead_in_ms": 300.0,
        })
    );
    // Its own settings win; "none" turns a default chime off.
    assert_eq!(
        request("own"),
        json!({
            "group": "239.1.1.1:5000", "name": "Own", "source": {"type": "file", "path": "a.wav"},
            "chime": "bell", "mode": "music", "volume_db": 0.0, "lead_in_ms": 300.0,
        })
    );
    // Saved as written: the defaults aren't copied into announcements.
    let saved = serde_json::to_value(&cfg).unwrap();
    assert!(saved["announcements"][0].get("chime").is_none());
    assert_eq!(saved["announcements"][1]["end_chime"], "none");

    // Without defaults, nothing changes from before.
    let mut bare = cfg.clone();
    bare.defaults = Default::default();
    let plain = bare.announcement("plain").unwrap();
    assert_eq!(plain.with_defaults(&bare.defaults), *plain);
    assert!(serde_json::to_value(&bare)
        .unwrap()
        .get("defaults")
        .is_none());

    let mut bad = cfg.clone();
    bad.defaults.chime = Some(json!({"file": "../x.wav"}));
    let e = bad.validate().unwrap_err().to_string();
    assert!(e.contains("defaults: chime"), "{e}");
}

#[test]
fn zone_groups_use_the_default_codec_unless_they_choose_one() {
    let mut cfg = config();
    cfg.defaults.encoding = Some("g722".into());
    cfg.validate().unwrap();
    let (own, plain) = (&cfg.zones[0].targets[0], &cfg.zones[0].targets[1]);
    assert_eq!(cfg.defaults.target(own).encoding.as_deref(), Some("g711u"));
    assert_eq!(cfg.defaults.target(plain).encoding.as_deref(), Some("g722"));

    // And the runner sends it.
    let fake = Arc::new(Fake::default());
    let r = runner(&fake);
    r.play(&cfg, "lunch", Trigger::Manual).unwrap();
    let mut codecs: Vec<_> = fake
        .requests()
        .iter()
        .map(|q| {
            (
                q["group"].as_str().unwrap().to_string(),
                q["encoding"].clone(),
            )
        })
        .collect();
    codecs.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        codecs,
        [
            ("239.1.1.1:5000".to_string(), json!("g711u")),
            ("239.1.1.2:5000".to_string(), json!("g722")),
        ]
    );
    fake.finish_all();

    cfg.defaults.encoding = Some("mp3".into());
    let e = cfg.validate().unwrap_err().to_string();
    assert!(e.contains("unknown encoding \"mp3\""), "{e}");
    cfg.defaults.encoding = None;
    cfg.zones[0].targets[0].encoding = Some("G711U".into());
    cfg.validate().unwrap();
}

#[test]
fn validation_finds_mistakes() {
    let bad = |f: &dyn Fn(&mut Config), msg: &str| {
        let mut c = config();
        f(&mut c);
        let e = c.validate().unwrap_err().to_string();
        assert!(e.contains(msg), "{e:?} should mention {msg:?}");
    };
    config().validate().unwrap();
    bad(&|c| c.zones[1].id = "up".into(), "two zones have the id");
    bad(
        &|c| c.zones[1].name = "UPSTAIRS".into(),
        "two zones are called",
    );
    bad(&|c| c.zones[0].id = "a b".into(), "invalid zone id");
    bad(&|c| c.zones[0].targets.clear(), "has no targets");
    bad(
        &|c| c.announcements[0].zones.push("attic".into()),
        "no zone \"attic\"",
    );
    bad(&|c| c.announcements[0].zones.clear(), "has no zones");
    bad(
        &|c| c.announcements[1].name = "Lunch Name".into(),
        "two announcements are called",
    );
    bad(&|c| c.announcements[0].repeat = 0, "repeat must be");
    bad(&|c| c.announcements[0].repeat_gap_s = -1.0, "repeat_gap_s");
    bad(
        &|c| c.announcements[0].audio = Audio::Media("../etc/passwd".into()),
        "isn't a media file name",
    );
    bad(
        &|c| c.announcements[0].volume_db = Some(f64::NAN),
        "volume_db",
    );
}

#[test]
fn saves_and_loads() {
    let dir = std::env::temp_dir().join(format!("mcast-intercom-test-{}", std::process::id()));
    let path = dir.join("sub").join("intercom.json");
    assert_eq!(store::load(&path).unwrap(), Config::default());
    store::save(&path, &config()).unwrap();
    assert_eq!(store::load(&path).unwrap(), config());

    let mut bad = config();
    bad.zones.clear();
    assert!(store::save(&path, &bad).is_err());
    assert_eq!(store::load(&path).unwrap(), config(), "unchanged");

    std::fs::write(&path, "{\"zones\": [").unwrap();
    assert!(store::load(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
