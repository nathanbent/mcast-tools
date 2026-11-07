//! A probe against a real in-process server, with a real page to watch.

use super::*;
use crate::client::Client;
use mcast_control::probes::Verdict;
use std::path::PathBuf;
use std::thread;

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
    server_with(None, None)
}

fn server_with(token: Option<&str>, probe_token: Option<&str>) -> Server {
    let dir = std::env::temp_dir().join(format!("mcast-probe-test-{}", mcast_core::random_u32()));
    let cfg = mcast_server::ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(Into::into),
        media_dir: dir.clone(),
        interface: Interface::Any,
        sap_groups: mcast_core::sap::default_listen_groups(),
        monitor: None,
        max_upload: 1 << 20,
        probe_token: probe_token.map(Into::into),
        probe_state: None,
        beacon: None,
        alert_webhook: None,
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

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn server_addr_from_urls() {
    assert_eq!(
        server_addr("http://127.0.0.1:8650").unwrap(),
        "127.0.0.1:8650".parse().unwrap()
    );
    assert_eq!(
        server_addr("http://127.0.0.1/x").unwrap(),
        "127.0.0.1:80".parse().unwrap()
    );
    assert_eq!(
        server_addr("https://[::1]").unwrap(),
        "[::1]:443".parse().unwrap()
    );
    assert_eq!(server_addr("[::1]:9").unwrap(), "[::1]:9".parse().unwrap());
}

#[test]
fn reports_multicast_and_unicast_side_by_side() {
    let srv = server();
    let admin = Client::new(&srv.url, None);
    let group: SocketAddr = "239.255.77.41:5004".parse().unwrap();
    admin
        .set_config(
            "pi-test",
            &ProbeConfig {
                label: Some("test bench".into()),
                sap: false,
                groups: vec![group],
                report_interval_s: 1.0,
                unicast: true,
            },
        )
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let agent = {
        let stop = stop.clone();
        let cfg = AgentConfig {
            server: super::Server::Url(srv.url.clone()),
            token: None,
            id: "pi-test".into(),
            interface: Interface::Any,
            sap_groups: Vec::new(),
            unicast_port: free_udp_port(),
            advertise: None,
            max_queue: 100,
            verbose: false,
        };
        thread::spawn(move || run(cfg, &stop))
    };

    // Page once the probe has asked for unicast copies.
    let deadline = Instant::now() + Duration::from_secs(10);
    while admin
        .probe("pi-test")
        .map_or(true, |p| !p.config_applied || p.unicast_addr.is_none())
    {
        assert!(
            Instant::now() < deadline,
            "the probe never applied its config"
        );
        thread::sleep(Duration::from_millis(100));
    }
    let page: serde_json::Value = ureq::post(&format!("{}/api/v1/senders", srv.url))
        .send_json(serde_json::json!({
            "group": group, "sap": false, "encoding": "g711u", "rtcp": false,
        }))
        .unwrap()
        .into_json()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let found =
        loop {
            let p = admin.probe("pi-test").unwrap();
            if let Some(c) = p.comparisons.iter().find(|c| {
                c.multicast.as_ref().is_some_and(|m| m.packets > 0) && c.unicast.packets > 0
            }) {
                break c.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no comparison arrived: {:#?}",
                p.latest
            );
            thread::sleep(Duration::from_millis(200));
        };
    assert_eq!(found.group, Some(group));
    assert!(
        matches!(found.verdict, Verdict::Ok | Verdict::BothLoss),
        "{found:?}"
    );
    let info = admin.probe("pi-test").unwrap();
    assert!(info.online);
    assert_eq!(info.config.label.as_deref(), Some("test bench"));
    assert!(info.totals.iter().any(|t| t.unicast && t.packets > 0));
    assert!(!admin.reports("pi-test").unwrap().is_empty());

    ureq::delete(&format!("{}/api/v1/jobs/{}", srv.url, page["id"]))
        .call()
        .unwrap();
    stop.store(true, Ordering::Relaxed);
    agent.join().unwrap().unwrap();
}

/// Announce `url` on `group` until `stop`, like `mcast serve` does.
fn beacon(
    group: SocketAddr,
    name: &str,
    url: &str,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let socket =
        mcast_core::open_sender(&mcast_core::MulticastOptions::new(group).unwrap()).unwrap();
    let payload = serde_json::to_vec(&mcast_control::probes::Beacon {
        service: mcast_control::probes::Beacon::SERVICE.into(),
        v: 1,
        name: name.into(),
        port: 1,
        url: Some(url.into()),
    })
    .unwrap();
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let _ = socket.send(&payload);
            thread::sleep(Duration::from_millis(200));
        }
    })
}

#[test]
fn finds_the_server_that_knows_its_token() {
    let ours = server_with(Some("admin"), Some("probes"));
    let theirs = server_with(Some("other"), None);
    let group: SocketAddr = format!(
        "239.255.86.{}:{}",
        100 + mcast_core::random_u32() % 100,
        free_udp_port()
    )
    .parse()
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    // The wrong one first, so it's heard first.
    let beacons = [
        beacon(group, "theirs", &theirs.url, stop.clone()),
        beacon(group, "ours", &ours.url, stop.clone()),
    ];

    let found = discovery::listen_for(group, Interface::Any, Duration::from_secs(1)).unwrap();
    assert_eq!(found.len(), 2, "{found:?}");

    let agent = {
        let stop = stop.clone();
        let cfg = AgentConfig {
            server: super::Server::Discover { group, name: None },
            token: Some("probes".into()),
            id: "pi-disc".into(),
            interface: Interface::Any,
            sap_groups: Vec::new(),
            unicast_port: free_udp_port(),
            advertise: None,
            max_queue: 100,
            verbose: false,
        };
        thread::spawn(move || run(cfg, &stop))
    };
    let admin = Client::new(&ours.url, Some("admin".into()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while admin.probe("pi-disc").is_err() {
        assert!(Instant::now() < deadline, "the probe never checked in");
        thread::sleep(Duration::from_millis(100));
    }
    let other = Client::new(&theirs.url, Some("other".into()));
    assert!(
        other.probes().unwrap().is_empty(),
        "it went to the wrong server"
    );

    stop.store(true, Ordering::Relaxed);
    agent.join().unwrap().unwrap();
    for b in beacons {
        b.join().unwrap();
    }
}
