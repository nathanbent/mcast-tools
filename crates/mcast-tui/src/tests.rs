//! The TUI against a real in-process server, drawn into a test terminal.

use crate::api::{Client, Feed};
use crate::app::{App, Mode, Msg, ProbeField, Tab};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
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

fn server(token: Option<&str>) -> Server {
    let dir = std::env::temp_dir().join(format!("mcast-tui-test-{}", mcast_core::random_u32()));
    let cfg = mcast_server::ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(Into::into),
        media_dir: dir.clone(),
        interface: mcast_core::Interface::Any,
        sap_groups: mcast_core::sap::default_listen_groups(),
        monitor: None,
        max_upload: 1 << 20,
        probe_token: None,
        probe_state: None,
        beacon: None,
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

fn app(url: &str) -> (App, Receiver<Msg>) {
    let (tx, rx) = mpsc::channel();
    let client = Client::new(url, None);
    let app = App::new(client.clone(), tx.clone());
    client.follow(move |f| tx.send(Msg::Feed(f)).is_ok());
    (app, rx)
}

/// Handle messages until `done` holds (or fail after a few seconds).
fn pump(app: &mut App, rx: &Receiver<Msg>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for {what}");
        if let Ok(m) = rx.recv_timeout(left.min(Duration::from_millis(100))) {
            app.handle(m);
        }
    }
}

fn key(app: &mut App, code: KeyCode) {
    crate::keys::handle(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn screen(app: &App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = t.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn pages_and_sees_the_overlap() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    pump(&mut app, &rx, "the event stream", |a| a.connection.is_ok());

    // Page a tone from the form.
    key(&mut app, KeyCode::Char('p'));
    assert_eq!(app.mode, Mode::Page);
    app.form.set("source", "tone");
    app.form.set("group", "239.255.77.61:5004");
    app.form.set("chime", "false");
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the page to start", |a| !a.busy);
    assert_eq!(app.mode, Mode::Normal, "{:?}", app.status);
    assert_eq!(app.tab, Tab::Jobs);
    let id = *app.jobs.keys().next().unwrap();
    pump(&mut app, &rx, "stats", |a| {
        a.jobs[&id].stats_line().contains("packets")
            && !a.jobs[&id].stats_line().starts_with("0:00 · 0")
    });
    let s = screen(&app, 100, 20);
    assert!(
        s.contains("Paging \"") && s.contains("running") && s.contains("1 Now playing"),
        "{s}"
    );
    assert!(s.contains("sending to 239.255.77.61:5004"), "{s}");

    // The same group again is refused, and the blocking job is marked.
    key(&mut app, KeyCode::Char('p'));
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the conflict", |a| !a.busy);
    assert_eq!(app.conflict, Some(id));
    let s = screen(&app, 120, 20);
    assert!(s.contains("blocking your page"), "{s}");
    assert!(s.contains("already being sent to"), "{s}");

    // x stops it.
    key(&mut app, KeyCode::Char('x'));
    pump(&mut app, &rx, "the job to stop", |a| {
        a.jobs[&id].state == "stopped"
    });
}

#[test]
fn draws_every_screen_at_any_size() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    app.refresh_all();
    pump(&mut app, &rx, "the lists", |a| {
        a.monitor.is_some() && a.connection.is_ok()
    });
    for mode in [
        Mode::Normal,
        Mode::Page,
        Mode::Help,
        Mode::Upload("/tmp/x".into()),
        Mode::ConfirmDelete("a.wav".into()),
        Mode::ConfirmForget("pi-1".into()),
        Mode::ProbeInput {
            id: "pi-1".into(),
            field: ProbeField::Groups,
            text: "239.1.1.1:5004".into(),
        },
    ] {
        app.mode = mode;
        for tab in Tab::ALL {
            app.tab = tab;
            for (w, h) in [(120, 40), (80, 24), (30, 8), (1, 1)] {
                screen(&app, w, h);
            }
        }
    }
    app.mode = Mode::Normal;
    app.tab = Tab::Monitor;
    assert!(screen(&app, 80, 10).contains("monitor is off"));
}

#[test]
fn uploads_and_pages_from_the_media_tab() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    pump(&mut app, &rx, "the event stream", |a| a.connection.is_ok());
    let wav = srv
        .dir
        .join("..")
        .join(format!("mcast-tui-upload-{}.wav", mcast_core::random_u32()));
    std::fs::write(&wav, b"RIFF").unwrap();
    app.tab = Tab::Media;
    key(&mut app, KeyCode::Char('u'));
    for c in wav.to_str().unwrap().chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the upload", |a| !a.media.is_empty());
    std::fs::remove_file(&wav).unwrap();
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Page);
    assert_eq!(app.form.get("file"), app.media[0].name);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Char('y'));
    pump(&mut app, &rx, "the delete", |a| a.media.is_empty());
}

#[test]
fn needs_the_token_before_starting() {
    let srv = server(Some("s3cret"));
    let err = crate::run(&srv.url, None).unwrap_err().to_string();
    assert!(err.contains("wants a token"), "{err}");
    let err = crate::run("http://127.0.0.1:1", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is `mcast serve` running"), "{err}");
    // The right token gets past the check (no terminal here, so stop at /health).
    assert!(Client::new(&srv.url, Some("s3cret".into()))
        .get::<serde_json::Value>("/health")
        .is_ok());
}

#[test]
fn reports_a_lost_server() {
    let (tx, rx) = mpsc::channel();
    Client::new("http://127.0.0.1:1", None).follow(move |f| tx.send(f).is_ok() && false);
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(10)),
        Ok(Feed::Disconnected(_))
    ));
}

/// Check in as a probe that sees loss on multicast but not on its unicast copy.
fn fake_probe(url: &str, id: &str) {
    let row = |group: &str, packets: u64, lost: u64| {
        serde_json::json!({
            "group": group, "name": "Zone 1", "source": "10.0.0.5:40000", "ssrc": 7,
            "payload_type": 0, "codec": "PCMU", "packets": packets, "bytes": packets * 160,
            "packets_per_sec": packets as f64 / 10.0, "kbit_per_sec": 64.0, "lost": lost,
            "late": 0, "loss_percent": lost as f64 * 100.0 / (packets + lost) as f64,
            "jitter_ms": 0.5, "max_gap_ms": 120.0, "ptime_ms": 20.0, "ttl": 15, "tos": null,
            "peak_dbfs": null, "rms_dbfs": null, "idle_ms": 0.0,
        })
    };
    let reports: Vec<serde_json::Value> = (1..=3)
        .map(|seq| {
            serde_json::json!({
                "seq": seq, "start_ms": seq * 10_000, "end_ms": seq * 10_000 + 10_000,
                "streams": [row("239.255.77.62:5004", 495, if seq == 3 { 5 } else { 0 }),
                            row("0.0.0.0:5104", 500, 0)],
            })
        })
        .collect();
    ureq::post(&format!("{url}/api/v1/probes/{id}/checkin"))
        .send_json(serde_json::json!({
            "version": "test", "hostname": "pi", "boot": 1, "uptime_s": 60.0,
            "unicast_addr": "127.0.0.1:5104", "reports": reports,
        }))
        .unwrap();
}

#[test]
fn shows_and_configures_probes() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    app.refresh_all();
    pump(&mut app, &rx, "the event stream", |a| a.connection.is_ok());
    fake_probe(&srv.url, "idf-3");
    pump(&mut app, &rx, "the probe", |a| a.probes.len() == 1);

    key(&mut app, KeyCode::Char('5'));
    assert_eq!(app.tab, Tab::Probes);
    pump(&mut app, &rx, "its reports", |a| {
        a.probe_reports().is_some_and(|r| r.len() == 3)
    });
    let s = screen(&app, 120, 30);
    assert!(s.contains("idf-3") && s.contains("5 Probes 1"), "{s}");
    assert!(s.contains("1 of 3 intervals with multicast loss"), "{s}");
    assert!(s.contains("5 lost (1.00 %)"), "{s}");

    // Turn unicast copies on: the server compares, and says multicast is at fault.
    key(&mut app, KeyCode::Char('u'));
    pump(&mut app, &rx, "unicast on", |a| {
        !a.busy && a.probes[0].flag("unicast")
    });
    fake_probe(&srv.url, "idf-3");
    pump(&mut app, &rx, "the comparison", |a| {
        !a.probes[0].comparisons.is_empty()
    });
    let s = screen(&app, 120, 30);
    assert!(s.contains("[MULTICAST LOSS] Zone 1"), "{s}");
    assert!(s.contains("IGMP snooping"), "{s}");

    // Typed settings.
    key(&mut app, KeyCode::Char('l'));
    for c in "IDF 3".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the label", |a| {
        a.probes[0].label() == Some("IDF 3")
    });
    key(&mut app, KeyCode::Char('g'));
    for c in "10.0.0.1:5004".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    assert!(app.status().unwrap().0.contains("isn't a multicast group"));

    // Back to the defaults, then forget it.
    key(&mut app, KeyCode::Char('D'));
    pump(&mut app, &rx, "the defaults", |a| {
        !a.probes[0].custom_config
    });
    key(&mut app, KeyCode::Char('f'));
    key(&mut app, KeyCode::Char('y'));
    pump(&mut app, &rx, "the probe to go", |a| a.probes.is_empty());
    assert!(screen(&app, 120, 10).contains("No probes yet"));
}
