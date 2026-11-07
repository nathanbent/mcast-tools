//! The intercom against a real, in-process `mcast serve`.

use mcast_intercom::backend::Backend;
use mcast_intercom::{
    Announcement, Audio, Config, HttpBackend, Priority, Run, RunState, Runner, Target, TargetState,
    Trigger, Zone,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Server {
    url: String,
    state: mcast_server::AppState,
    dir: PathBuf,
    _rt: tokio::runtime::Runtime,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.state.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn server() -> Server {
    let dir = std::env::temp_dir().join(format!(
        "mcast-intercom-server-test-{}",
        mcast_core::random_u32()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("phonebook.conf"),
        "[zone-1]\ngroup = 239.255.77.71:5004\n",
    )
    .unwrap();
    let cfg = mcast_server::ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: Some("secret".into()),
        media_dir: dir.join("media"),
        interface: mcast_core::Interface::Any,
        sap_groups: mcast_core::sap::default_listen_groups(),
        monitor: None,
        max_upload: 1 << 20,
        phonebook: dir.join("phonebook.conf"),
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let state = mcast_server::AppState::start(&cfg, rx).unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = mcast_server::router(state.clone());
    rt.spawn(async move { axum::serve(listener, app).await });
    Server {
        url,
        state,
        dir,
        _rt: rt,
    }
}

fn config() -> Config {
    let target = |g: &str| Target {
        group: g.into(),
        encoding: Some("g711u".into()),
        ..Default::default()
    };
    let chime = |id: &str, zone: &str, priority| Announcement {
        id: id.into(),
        name: id.into(),
        audio: Audio::ChimeOnly,
        chime: Some(json!("ding-dong")),
        end_chime: None,
        mode: None,
        volume_db: None,
        lead_in_ms: None,
        repeat: 1,
        repeat_gap_s: 0.0,
        zones: vec![zone.into()],
        priority,
    };
    Config {
        zones: vec![
            Zone {
                id: "both".into(),
                name: "Both".into(),
                // The same group by name and by address plays once.
                targets: vec![
                    target("zone-1"),
                    target("239.255.77.72:5004"),
                    target("239.255.77.71:5004"),
                ],
            },
            Zone {
                id: "busy".into(),
                name: "Busy".into(),
                targets: vec![target("239.255.77.73:5004")],
            },
        ],
        schedules: Vec::new(),
        time_zone: None,
        announcements: vec![
            chime("ding", "both", Priority::Normal),
            chime("polite", "busy", Priority::Normal),
            chime("urgent", "busy", Priority::Emergency),
        ],
    }
}

fn wait_for(r: &Runner, id: u64, what: &str, done: impl Fn(&Run) -> bool) -> Run {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let run = r.get(id).unwrap();
        if done(&run) {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {run:#?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn plays_a_chime_to_two_groups() {
    let srv = server();
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let r = Runner::new(backend, |_| {});
    let run = r.play(&config(), "ding", Trigger::Manual).unwrap();
    assert_eq!(run.targets.len(), 2, "{run:#?}");
    assert_eq!(
        run.targets[0].address.as_deref(),
        Some("239.255.77.71:5004")
    );
    let run = wait_for(&r, run.id, "the chime to end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done, "{run:#?}");
    assert!(run.targets.iter().all(|t| t.played == 1), "{run:#?}");
}

#[test]
fn takes_over_a_busy_group_only_for_emergencies() {
    let srv = server();
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let tone = backend
        .start_sender(&json!({"group": "239.255.77.73:5004", "source": {"type": "tone"}}))
        .unwrap();
    let r = Runner::new(backend.clone(), |_| {});

    let run = r.play(&config(), "polite", Trigger::Manual).unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.targets[0].state, TargetState::Busy);
    assert!(backend.job(tone.id).unwrap().state == "running");

    let run = r.play(&config(), "urgent", Trigger::Manual).unwrap();
    assert_eq!(run.targets[0].state, TargetState::Playing, "{run:#?}");
    assert_eq!(backend.job(tone.id).unwrap().state, "stopped");
    let run = wait_for(&r, run.id, "the chime to end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done, "{run:#?}");
}

#[test]
fn stops_a_run() {
    let srv = server();
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let r = Runner::new(backend.clone(), |_| {});
    let mut cfg = config();
    cfg.announcements[0].repeat = 5;
    cfg.announcements[0].repeat_gap_s = 0.5;
    let run = r.play(&cfg, "ding", Trigger::Manual).unwrap();
    let job = run.targets[0].job.unwrap();
    let run = r.stop(run.id).unwrap();
    let run = wait_for(&r, run.id, "the stop", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Stopped, "{run:#?}");
    assert!(backend.job(job).unwrap().state != "running");
}

#[test]
fn check_reports_missing_media_and_names() {
    let srv = server();
    let backend = HttpBackend::new(&srv.url, Some("secret".into()));
    let path = srv.dir.join("intercom.json");
    let mut cfg = config();
    cfg.announcements[0].audio = Audio::Media("lunch.wav".into());
    cfg.zones[1].targets[0].group = "attic".into();
    mcast_intercom::store::save(&path, &cfg).unwrap();
    let e = mcast_intercom::cli::check(&path, &backend).unwrap_err();
    assert!(e.to_string().contains("2 problem(s)"), "{e}");

    cfg.announcements[0].audio = Audio::ChimeOnly;
    cfg.zones[1].targets[0].group = "zone-1".into();
    mcast_intercom::store::save(&path, &cfg).unwrap();
    mcast_intercom::cli::check(&path, &backend).unwrap();

    // A wrong token is an error, not a skipped check.
    let wrong = HttpBackend::new(&srv.url, Some("nope".into()));
    assert!(mcast_intercom::cli::check(&path, &wrong).is_err());
}

#[test]
fn the_panel_api_plays_through_a_real_server() {
    use mcast_intercom::server::{router, AppState};
    let srv = server();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let state = AppState::new(backend, srv.dir.join("intercom.json"), None, rx);
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let panel = format!("http://{}/api/intercom/v1", listener.local_addr().unwrap());
    rt.spawn(async move { axum::serve(listener, router(state)).await });

    let post = |path: &str, body: serde_json::Value| -> serde_json::Value {
        ureq::post(&format!("{panel}{path}"))
            .send_json(body)
            .unwrap()
            .into_json()
            .unwrap()
    };
    post(
        "/zones",
        json!({"name": "Hall", "targets": [{"group": "zone-1", "encoding": "g722"}]}),
    );
    post(
        "/announcements",
        json!({"name": "Chime", "audio": "chime_only", "chime": "ding-dong", "zones": ["hall"]}),
    );
    let run = post("/announcements/chime/play", json!({}));
    assert_eq!(run["state"], "playing", "{run}");
    let id = run["id"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let run: serde_json::Value = ureq::get(&format!("{panel}/runs/{id}"))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        if run["state"] == "done" {
            assert_eq!(run["targets"][0]["address"], "239.255.77.71:5004");
            break;
        }
        assert!(Instant::now() < deadline, "{run}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let b: serde_json::Value = ureq::get(&format!("{panel}/backend"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(b["connected"], true, "{b}");
}
