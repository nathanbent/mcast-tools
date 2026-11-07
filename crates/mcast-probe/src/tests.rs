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
    let dir = std::env::temp_dir().join(format!("mcast-probe-test-{}", mcast_core::random_u32()));
    let cfg = mcast_server::ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: None,
        media_dir: dir.clone(),
        interface: Interface::Any,
        sap_groups: mcast_core::sap::default_listen_groups(),
        monitor: None,
        max_upload: 1 << 20,
        probe_token: None,
        probe_state: None,
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
            server: srv.url.clone(),
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
