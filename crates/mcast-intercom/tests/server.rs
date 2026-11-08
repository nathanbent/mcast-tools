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
    server_watching(None)
}

/// With the monitor on, watching these groups.
fn server_watching(monitor: Option<Vec<std::net::SocketAddr>>) -> Server {
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
        monitor,
        max_upload: 1 << 20,
        tts: Default::default(),
        phonebook: dir.join("phonebook.conf"),
        chimes: dir.join("chimes.json"),
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
        input_gain_db: None,
        normalize: None,
        lead_in_ms: None,
        repeat: 1,
        repeat_gap_s: 0.0,
        zones: vec![zone.into()],
        priority,
        parts: Vec::new(),
        category: None,
        tags: Vec::new(),
        color: None,
        icon: None,
        confirm: false,
    };
    Config {
        zones: vec![
            Zone {
                listeners: Vec::new(),
                site: None,
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
                listeners: Vec::new(),
                site: None,
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
        ..Default::default()
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
    let run = r
        .play(&config(), "ding", Trigger::Manual { by: None })
        .unwrap();
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

    let run = r
        .play(&config(), "polite", Trigger::Manual { by: None })
        .unwrap();
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.targets[0].state, TargetState::Busy);
    assert!(backend.job(tone.id).unwrap().state == "running");

    let run = r
        .play(&config(), "urgent", Trigger::Manual { by: None })
        .unwrap();
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
    let run = r.play(&cfg, "ding", Trigger::Manual { by: None }).unwrap();
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
    cfg.announcements[0].end_chime = Some(json!("foghorn"));
    mcast_intercom::store::save(&path, &cfg).unwrap();
    let e = mcast_intercom::cli::check(&path, &backend).unwrap_err();
    assert!(e.to_string().contains("3 problem(s)"), "{e}");

    cfg.announcements[0].audio = Audio::ChimeOnly;
    cfg.announcements[0].end_chime = Some(json!("church-bell"));
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

    // Sounds uploaded through the panel are checked by the real server.
    // (status, body)
    let put = |name: &str, data: &[u8]| -> (u16, serde_json::Value) {
        match ureq::put(&format!("{panel}/media/{name}")).send_bytes(data) {
            Ok(r) => (r.status(), r.into_json().unwrap()),
            Err(ureq::Error::Status(code, r)) => (code, r.into_json().unwrap()),
            Err(e) => panic!("{e}"),
        }
    };
    let (status, v) = put("bells/notes.wav", b"not audio");
    assert_eq!(status, 422, "{v}");
    assert!(v["error"].as_str().unwrap().contains("isn't audio"), "{v}");
    let mut wav = Vec::new();
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::new(std::io::Cursor::new(&mut wav), spec).unwrap();
        for i in 0..8000 {
            w.write_sample(((i % 40) as i16 - 20) * 500).unwrap();
        }
        w.finalize().unwrap();
    }
    let (_, v) = put("bells/half.wav", &wav);
    assert_eq!(v["audio"]["duration_s"], 0.5, "{v}");
    let lib: serde_json::Value = ureq::get(&format!("{panel}/library"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let f = lib["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "bells/half.wav")
        .unwrap();
    assert_eq!(f["audio"]["rate"], 16_000, "{lib}");
}

/// Push to talk, end to end: a WebSocket into the panel, live senders on a
/// real mcast server, and the page ending when the talking does.
#[test]
fn talks_live_to_two_groups() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let srv = server();
    let path = srv.dir.join("intercom.json");
    let mut cfg = config();
    cfg.announcements.push(Announcement {
        id: "talk".into(),
        name: "Talk".into(),
        audio: Audio::Live,
        chime: Some(json!("beep")),
        end_chime: Some(json!("beep")),
        mode: Some("voice".into()),
        volume_db: None,
        input_gain_db: None,
        normalize: None,
        lead_in_ms: None,
        repeat: 1,
        repeat_gap_s: 0.0,
        zones: vec!["both".into()],
        priority: Priority::Normal,
        parts: Vec::new(),
        category: None,
        tags: Vec::new(),
        color: None,
        icon: None,
        confirm: false,
    });
    std::fs::write(&path, serde_json::to_string(&cfg).unwrap()).unwrap();
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let (_closing_tx, closing) = tokio::sync::watch::channel(false);

    srv._rt.block_on(async {
        let state = mcast_intercom::server::AppState::new(backend, path, None, closing);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = mcast_intercom::server::router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });

        // Not live: refused.
        let url = format!("ws://{addr}/api/intercom/v1/live/ding");
        assert!(tokio_tungstenite::connect_async(url).await.is_err());

        let url = format!("ws://{addr}/api/intercom/v1/live/talk");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        let started: serde_json::Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
        assert_eq!(started["type"], "started", "{started}");
        assert_eq!(started["groups"], 2);
        let run_id = started["run"].as_u64().unwrap();

        // Half a second of tone, in 20 ms chunks, then the end.
        for chunk in 0..25 {
            let pcm: Vec<u8> = (0..960)
                .flat_map(|i| {
                    let t = (chunk * 960 + i) as f32 / 48_000.0;
                    (((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16).to_le_bytes()
                })
                .collect();
            ws.send(Message::Binary(pcm.into())).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(state.runner().get(run_id).unwrap().state, RunState::Playing);
        ws.send(Message::Text("end".into())).await.unwrap();

        let r = state.runner().clone();
        let run = tokio::task::spawn_blocking(move || {
            wait_for(&r, run_id, "the page to end", |r| r.state.is_over())
        })
        .await
        .unwrap();
        assert_eq!(run.state, RunState::Done, "{run:#?}");
        assert!(run
            .targets
            .iter()
            .all(|t| t.state == TargetState::Done && t.played == 1));
    });
}

/// A playlist to one zone and a sound to another, in one run, on a real
/// mcast server: both play to the end.
#[test]
fn plays_a_playlist_and_a_part_together() {
    let srv = server();
    let media = srv.dir.join("media");
    std::fs::create_dir_all(&media).unwrap();
    for (name, rate) in [
        ("one.wav", 16_000),
        ("two.wav", 8_000),
        ("other.wav", 48_000),
    ] {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(media.join(name), spec).unwrap();
        for i in 0..rate / 4 {
            w.write_sample(((i % 50) as i16 - 25) * 300).unwrap();
        }
        w.finalize().unwrap();
    }
    let cfg: Config = serde_json::from_value(json!({
        "zones": [
            {"id": "a", "name": "A", "targets": [{"group": "239.255.77.81:5004", "encoding": "g711u"}]},
            {"id": "b", "name": "B", "targets": [{"group": "239.255.77.82:5004"}]}
        ],
        "playlists": [{"id": "pl", "name": "PL", "gap_ms": 100,
            "items": [{"type": "sound", "media": "one.wav"}, {"type": "sound", "media": "two.wav"}]}],
        "announcements": [{"id": "both", "name": "Both", "audio": {"playlist": "pl"}, "zones": ["a"],
            "parts": [{"zones": ["b"], "audio": {"media": "other.wav"}}]}]
    }))
    .unwrap();
    cfg.validate().unwrap();
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let r = Runner::new(backend, |_| {});
    let run = r.play(&cfg, "both", Trigger::Manual { by: None }).unwrap();
    assert_eq!(run.targets.len(), 2, "{run:#?}");
    assert!(run.targets.iter().all(|t| t.error.is_none()), "{run:#?}");
    let run = wait_for(&r, run.id, "both to end", |r| r.state.is_over());
    assert_eq!(run.state, RunState::Done, "{run:#?}");
    assert!(run.targets.iter().all(|t| t.played == 1), "{run:#?}");
}

#[test]
fn says_which_receivers_heard_a_page() {
    let group: std::net::SocketAddr = "239.255.77.74:5004".parse().unwrap();
    let srv = server_watching(Some(vec![group]));
    let backend = Arc::new(HttpBackend::new(&srv.url, Some("secret".into())));
    let r = Runner::with_listen_timing(
        backend,
        |_| {},
        Duration::from_millis(200),
        Duration::from_secs(3),
    );

    // A speaker called Lobby; Hall is expected too, but isn't running.
    let jobs = mcast_control::Jobs::new(mcast_core::Interface::Any, Arc::new(|_| {}));
    let wav = srv.dir.join("lobby.wav");
    let lobby = jobs
        .start_receiver(
            mcast_control::request::RecvRequest {
                group: Some(group),
                name: Some("Lobby".into()),
                rtcp_interval_s: 0.5,
                sink: mcast_control::request::SinkRequest::Wav { path: wav },
                ..Default::default()
            },
            &[],
        )
        .unwrap();

    let mut cfg = config();
    cfg.zones.push(Zone {
        listeners: vec!["Lobby".into(), "Hall".into()],
        site: None,
        id: "ears".into(),
        name: "Ears".into(),
        targets: vec![Target {
            group: group.to_string(),
            encoding: Some("g711u".into()),
            ..Default::default()
        }],
    });
    let mut ding = cfg.announcements[0].clone();
    ding.id = "ears".into();
    ding.name = "ears".into();
    ding.zones = vec!["ears".into()];
    cfg.announcements.push(ding);

    let run = r.play(&cfg, "ears", Trigger::Manual { by: None }).unwrap();
    assert!(run.listeners.as_ref().unwrap().checking);
    let run = wait_for(&r, run.id, "the check to end", |r| {
        r.state.is_over() && !r.checking()
    });
    assert_eq!(run.state, RunState::Done, "{run:#?}");
    let l = run.listeners.unwrap();
    assert!(l.error.is_none(), "{l:?}");
    let heard: Vec<_> = l.heard.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(heard, ["Lobby"], "{l:?}");
    assert_eq!(l.silent, ["Hall"]);
    jobs.stop(lobby.id, Duration::from_secs(5));
}
