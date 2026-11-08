//! The TUI against a real in-process server, drawn into a test terminal.

use crate::api::{Client, Feed};
use crate::app::{App, Mode, Msg, Tab};
use crate::form::FormKind;
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
    assert_eq!(app.mode, Mode::Form(FormKind::Page));
    app.forms.page.set("source", "tone");
    app.forms.page.set("group", "239.255.77.61:5004");
    app.forms.page.set("chime", "none");
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
fn a_job_list_older_than_a_job_event_is_fetched_again() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    pump(&mut app, &rx, "the event stream", |a| a.connection.is_ok());
    let asked = app.job_changes;
    // A job starts while the list (from before it) is on its way.
    app.handle(Msg::Feed(Feed::Event(serde_json::json!({
        "type": "job_started",
        "job": {"id": 7, "kind": "sender", "state": "running"},
    }))));
    app.handle(Msg::Jobs(asked, Ok(Vec::new())));
    assert!(app.jobs.contains_key(&7), "the stale list was used");
    // The list asked for again replaces it (this server has no job 7).
    pump(&mut app, &rx, "the list again", |a| a.jobs.is_empty());
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
        Mode::Form(FormKind::Page),
        Mode::Form(FormKind::Text),
        Mode::Form(FormKind::SendFiles),
        Mode::Form(FormKind::ReceiveFiles),
        Mode::Compose("hello".into()),
        Mode::Help,
        Mode::Upload("/tmp/x".into()),
        Mode::ConfirmDelete("a.wav".into()),
        Mode::Form(FormKind::Book),
        Mode::ConfirmRemove("zone-1".into()),
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
    pump(&mut app, &rx, "the upload", |a| {
        !a.busy && !a.media.is_empty()
    });
    std::fs::remove_file(&wav).unwrap();
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Form(FormKind::Page));
    assert_eq!(app.forms.page.get("file"), app.media[0].name);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::Char('y'));
    pump(&mut app, &rx, "the delete", |a| {
        !a.busy && a.media.is_empty()
    });
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

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        key(app, KeyCode::Char(c));
    }
}

#[test]
fn chats_and_sends_files_by_keyboard() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    pump(&mut app, &rx, "the event stream", |a| a.connection.is_ok());

    // Join a channel from the Text tab (j), as a listener and as a talker.
    app.tab = Tab::Text;
    assert!(screen(&app, 100, 12).contains("No text channel open"));
    let group = "239.255.77.62:5004";
    app.forms.text.set("group", group);
    app.forms.text.set("name", "ear");
    app.forms.text.set("listen", "true");
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.mode, Mode::Form(FormKind::Text));
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the listener", |a| {
        !a.busy && a.channels().len() == 1
    });
    let ear = app.channels()[0].id;
    app.forms.text.set("name", "desk");
    app.forms.text.set("listen", "false");
    key(&mut app, KeyCode::Char('j'));
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the talker", |a| {
        !a.busy && a.channels().len() == 2
    });
    assert_eq!(app.tab, Tab::Text);
    let desk = app.current_channel().unwrap().id;
    assert_ne!(desk, ear);

    // Type a message: Enter sends and stays ready for the next.
    key(&mut app, KeyCode::Char('i'));
    type_text(&mut app, "hello from the tui");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Compose(String::new()));
    key(&mut app, KeyCode::Esc);
    // The talker's own copy and the listener's arrive separately, and the
    // listener may hear the line before the talker's name (then relabelled).
    pump(&mut app, &rx, "the line, said and heard", |a| {
        let said = a.texts.get(&desk).is_some_and(|l| !l.is_empty());
        let heard = a
            .texts
            .get(&ear)
            .is_some_and(|l| l.first().is_some_and(|l| l.from == "desk"));
        said && heard
    });
    assert_eq!(app.texts[&ear][0].text, "hello from the tui");
    let s = screen(&app, 100, 12);
    assert!(
        s.contains("desk: hello from the tui") && s.contains("2 channels"),
        "{s}"
    );
    // Switch to the listener: it can't be typed into.
    key(&mut app, KeyCode::Char(']'));
    assert_eq!(app.current_channel().unwrap().id, ear);
    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.mode, Mode::Normal);
    let s = screen(&app, 100, 12);
    assert!(s.contains("desk: hello from the tui"), "{s}");

    // Files: upload, receive (r), send the selected file (f).
    let src = srv
        .dir
        .join("..")
        .join(format!("mcast-tui-pic-{}.png", mcast_core::random_u32()));
    std::fs::write(&src, vec![7u8; 20_000]).unwrap();
    app.tab = Tab::Media;
    key(&mut app, KeyCode::Char('u'));
    type_text(&mut app, src.to_str().unwrap());
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the upload", |a| {
        !a.busy && a.media.len() == 1
    });
    std::fs::remove_file(&src).unwrap();
    // Pictures can't be paged.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Normal);
    let files_group = "239.255.77.63:5004";
    key(&mut app, KeyCode::Char('r'));
    assert_eq!(app.mode, Mode::Form(FormKind::ReceiveFiles));
    app.forms.receive.set("group", files_group);
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the receiver", |a| {
        !a.busy && a.mode == Mode::Normal
    });
    app.tab = Tab::Media;
    key(&mut app, KeyCode::Char('f'));
    assert_eq!(app.mode, Mode::Form(FormKind::SendFiles));
    assert_eq!(app.forms.send.get("file"), app.media[0].name);
    app.forms.send.set("group", files_group);
    app.forms.send.set("rate", "20000");
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the copy to arrive", |a| {
        !a.busy && a.media.len() == 2
    });
    let copy = app
        .media
        .iter()
        .find(|m| m.name.contains("-2.png"))
        .map(|m| m.name.clone());
    assert!(
        copy.is_some(),
        "received copy saved under a new name: {:?}",
        app.media
    );
    let s = screen(&app, 120, 30);
    assert!(s.contains(&copy.unwrap()), "{s}");
}

#[test]
fn help_fits_a_normal_terminal() {
    let (tx, _rx) = mpsc::channel();
    let mut app = App::new(Client::new("http://127.0.0.1:1", None), tx);
    app.mode = Mode::Help;
    let s = screen(&app, 140, 24);
    // Both ends of the list are on screen.
    assert!(s.contains("switch tab") && s.contains("Esc closes"), "{s}");
}

#[test]
fn keeps_the_phonebook_and_pages_by_name() {
    let srv = server(None);
    let (mut app, rx) = app(&srv.url);
    pump(&mut app, &rx, "the event stream", |a| a.connection.is_ok());
    key(&mut app, KeyCode::Char('6'));
    assert_eq!(app.tab, Tab::Book);
    let s = screen(&app, 80, 10);
    assert!(
        s.contains("No saved groups") && s.contains("6 Phonebook"),
        "{s}"
    );

    // Add one from the form.
    key(&mut app, KeyCode::Char('a'));
    assert_eq!(app.mode, Mode::Form(FormKind::Book));
    type_text(&mut app, "zone 1");
    key(&mut app, KeyCode::Down);
    type_text(&mut app, "239.255.77.71:5004");
    key(&mut app, KeyCode::Down);
    type_text(&mut app, "Ground floor");
    key(&mut app, KeyCode::Enter);
    // The reply closes the form; the event brings the entry. Wait for both.
    pump(&mut app, &rx, "the entry", |a| {
        !a.busy && a.phonebook.len() == 1
    });
    assert_eq!(app.mode, Mode::Normal, "{:?}", app.status);
    let s = screen(&app, 100, 10);
    assert!(
        s.contains("zone 1") && s.contains("239.255.77.71:5004") && s.contains("Ground floor"),
        "{s}"
    );

    // A bad group keeps the form open with the server's reason.
    key(&mut app, KeyCode::Char('a'));
    type_text(&mut app, "bad");
    key(&mut app, KeyCode::Down);
    type_text(&mut app, "nowhere");
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the refusal", |a| !a.busy);
    assert_eq!(app.mode, Mode::Form(FormKind::Book));
    key(&mut app, KeyCode::Esc);

    // Edit: rename it.
    key(&mut app, KeyCode::Char('e'));
    assert_eq!(app.forms.book.get("group"), "239.255.77.71:5004");
    app.forms.book.set("name", "Zone-1");
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the rename", |a| {
        !a.busy && a.phonebook.first().is_some_and(|e| e.name == "Zone-1")
    });
    assert_eq!(app.phonebook.len(), 1);

    // Enter pages it by name; ←→ on Group picks names too.
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Form(FormKind::Page));
    assert_eq!(app.forms.page.get("group"), "Zone-1");
    app.forms.page.set("group", "239.1.1.1:5");
    app.forms.page.selected = 0;
    key(&mut app, KeyCode::Right);
    assert_eq!(app.forms.page.get("group"), "Zone-1");
    assert!(screen(&app, 100, 30).contains("→ 239.255.77.71:5004"));
    app.forms.page.set("source", "tone");
    app.forms.page.set("chime", "none");
    key(&mut app, KeyCode::Enter);
    pump(&mut app, &rx, "the page", |a| !a.busy);
    let job = app.jobs.values().next().expect("a job").clone();
    assert_eq!(job.group.as_deref(), Some("239.255.77.71:5004"));
    app.stop(job.id);

    // Remove it, after a y.
    app.tab = Tab::Book;
    key(&mut app, KeyCode::Char('d'));
    assert_eq!(app.mode, Mode::ConfirmRemove("Zone-1".into()));
    key(&mut app, KeyCode::Char('y'));
    pump(&mut app, &rx, "the removal", |a| {
        !a.busy && a.phonebook.is_empty()
    });
}
