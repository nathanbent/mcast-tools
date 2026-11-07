use super::*;
use axum::http::Request;
use tower::ServiceExt;

struct Harness {
    app: Router,
    state: AppState,
    dir: PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.state.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn harness(token: Option<&str>) -> Harness {
    harness_with(token, None)
}

fn harness_with(token: Option<&str>, probe_token: Option<&str>) -> Harness {
    let dir = std::env::temp_dir().join(format!("mcast-server-test-{}", mcast_core::random_u32()));
    let cfg = ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(Into::into),
        media_dir: dir.clone(),
        interface: Interface::Any,
        sap_groups: mcast_core::sap::default_listen_groups(),
        monitor: None,
        max_upload: 1 << 20,
        probe_token: probe_token.map(Into::into),
        probe_state: None,
    };
    let (_tx, rx) = watch::channel(false);
    let state = AppState::start(&cfg, rx).unwrap();
    Harness {
        app: router(state.clone()),
        state,
        dir,
    }
}

async fn call(
    h: &Harness,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    token: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

#[tokio::test(flavor = "multi_thread")]
async fn requires_the_token_when_set() {
    let h = harness(Some("s3cret"));
    assert_eq!(
        call(&h, "GET", "/api/v1/health", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&h, "GET", "/api/v1/health", None, Some("wrong"))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&h, "GET", "/api/v1/health", None, Some("s3cret"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&h, "GET", "/api/v1/jobs?access_token=s3cret", None, None)
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_overlapping_pages() {
    let h = harness(None);
    let page = json!({"group": "239.255.77.21:5004", "sap": false, "encoding": "g711u"});
    let (status, first) = call(&h, "POST", "/api/v1/senders", Some(page.clone()), None).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(first["state"], "running");
    assert_eq!(first["kind"], "send");
    assert!(first["summary"]
        .as_str()
        .unwrap()
        .starts_with("PCMU/8000/1 sending to"));

    let (status, err) = call(&h, "POST", "/api/v1/senders", Some(page.clone()), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["job"], first["id"]);

    let id = first["id"].as_u64().unwrap();
    let (status, stopped) = call(&h, "DELETE", &format!("/api/v1/jobs/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stopped["state"], "stopped");

    let (status, again) = call(&h, "POST", "/api/v1/senders", Some(page), None).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = again["id"].as_u64().unwrap();
    call(&h, "DELETE", &format!("/api/v1/jobs/{id}"), None, None).await;

    let (_, jobs) = call(&h, "GET", "/api/v1/jobs", None, None).await;
    assert_eq!(jobs.as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn explains_bad_requests() {
    let h = harness(None);
    let (status, err) = call(
        &h,
        "POST",
        "/api/v1/senders",
        Some(json!({"grop": "239.1.1.1:5000"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(err["error"].as_str().unwrap().contains("grop"), "{err}");

    for path in ["../../etc/passwd", "/etc/passwd", ".hidden", "missing.wav"] {
        let (status, err) = call(
            &h,
            "POST",
            "/api/v1/senders",
            Some(json!({"source": {"type": "file", "path": path}})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}: {err}");
    }
    let (status, _) = call(
        &h,
        "POST",
        "/api/v1/senders",
        Some(json!({"render": "out.wav", "duration_s": 1})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        call(&h, "GET", "/api/v1/jobs/99", None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&h, "GET", "/api/v1/nope", None, None).await.0,
        StatusCode::NOT_FOUND
    );
}

/// A short 8 kHz mono 16-bit WAV of a 1 kHz tone.
fn wav() -> Vec<u8> {
    let samples: Vec<i16> = (0..1600)
        .map(|i| ((i as f32 * 1000.0 / 8000.0 * std::f32::consts::TAU).sin() * 8000.0) as i16)
        .collect();
    let data = samples.len() as u32 * 2;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&16000u32.to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_and_pages_a_file() {
    let h = harness(None);
    let upload = Request::builder()
        .method("PUT")
        .uri("/api/v1/media/page.wav")
        .body(Body::from(wav()))
        .unwrap();
    let resp = h.app.clone().oneshot(upload).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let (_, media) = call(&h, "GET", "/api/v1/media", None, None).await;
    assert_eq!(media[0]["name"], "page.wav");

    let (status, job) = call(
        &h,
        "POST",
        "/api/v1/senders",
        Some(json!({
            "group": "239.255.77.22:5004", "sap": false, "encoding": "g711u", "mode": "voice",
            "chime": "default", "tail_ms": 0,
            "source": {"type": "file", "path": "page.wav"}
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert!(
        job["notes"][0].as_str().unwrap().starts_with("loudness:"),
        "{job}"
    );

    // Runs to the end on its own.
    let id = job["id"].as_u64().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let done = loop {
        let (_, j) = call(&h, "GET", &format!("/api/v1/jobs/{id}"), None, None).await;
        if j["state"] != "running" || std::time::Instant::now() > deadline {
            break j;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(done["state"], "finished", "{done}");
    assert!(done["stats"]["seconds"].as_f64().unwrap() > 1.0, "{done}");

    assert_eq!(
        call(&h, "DELETE", "/api/v1/media/page.wav", None, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let too_big = Request::builder()
        .method("PUT")
        .uri("/api/v1/media/big.wav")
        .body(Body::from(vec![0u8; 2 << 20]))
        .unwrap();
    let resp = h.app.clone().oneshot(too_big).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!h.dir.join("big.wav").exists());
}

#[tokio::test]
async fn refuses_to_listen_publicly_without_a_token() {
    let cfg = ServerConfig {
        listen: "0.0.0.0:0".parse().unwrap(),
        token: None,
        media_dir: std::env::temp_dir(),
        interface: Interface::Any,
        sap_groups: Vec::new(),
        monitor: None,
        max_upload: 0,
        probe_token: None,
        probe_state: None,
    };
    let err = serve(cfg, Router::new(), async {}).await.unwrap_err();
    assert!(err.to_string().contains("without a token"));
}

fn check_in(unicast_addr: Option<std::net::SocketAddr>, seq: u64) -> serde_json::Value {
    json!({
        "version": "test",
        "hostname": "pi-1",
        "boot": 42,
        "uptime_s": 3.0,
        "unicast_addr": unicast_addr,
        "reports": [{
            "seq": seq,
            "start_ms": 1000,
            "end_ms": 11000,
            "streams": [],
        }],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_probe_token_only_checks_in() {
    let h = harness_with(Some("admin"), Some("probe"));
    let (status, reply) = call(
        &h,
        "POST",
        "/api/v1/probes/pi-1/checkin",
        Some(check_in(None, 1)),
        Some("probe"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["acked_seq"], 1);
    assert_eq!(reply["config"]["report_interval_s"], 10.0);
    for (method, path) in [
        ("GET", "/api/v1/probes"),
        ("GET", "/api/v1/jobs"),
        ("POST", "/api/v1/senders"),
        ("GET", "/api/v1/probes/pi-1/checkin"),
    ] {
        assert_eq!(
            call(&h, method, path, None, Some("probe")).await.0,
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }
    // The admin token works for check-ins too.
    let (status, _) = call(
        &h,
        "POST",
        "/api/v1/probes/pi-2/checkin",
        Some(check_in(None, 1)),
        Some("admin"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, list) = call(&h, "GET", "/api/v1/probes", None, Some("admin")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["online"], true);
    assert_eq!(list[0]["hostname"], "pi-1");
}

#[tokio::test(flavor = "multi_thread")]
async fn configures_probes_and_mirrors_senders_to_them() {
    let h = harness(None);
    let (status, err) = call(
        &h,
        "PUT",
        "/api/v1/probes/pi-1/config",
        Some(json!({"report_interval_s": 0})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");

    // Configured before it ever checks in.
    let (status, info) = call(
        &h,
        "PUT",
        "/api/v1/probes/pi-1/config",
        Some(json!({"label": "IDF 3", "unicast": true, "report_interval_s": 5})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["online"], false);
    assert_eq!(info["custom_config"], true);

    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    probe
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (status, reply) = call(
        &h,
        "POST",
        "/api/v1/probes/pi-1/checkin",
        Some(check_in(Some(probe.local_addr().unwrap()), 1)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["config"]["label"], "IDF 3");
    assert_eq!(reply["config"]["unicast"], true);

    let page = json!({"group": "239.255.77.23:5004", "sap": false, "encoding": "g711u"});
    let (status, job) = call(&h, "POST", "/api/v1/senders", Some(page), None).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let mut buf = [0u8; 1500];
    let n = probe.recv(&mut buf).expect("a unicast copy of the page");
    let rtp = mcast_core::rtp::parse(&buf[..n]).unwrap();
    assert_eq!(rtp.header.payload_type, 0, "PCMU");
    let id = job["id"].as_u64().unwrap();
    call(&h, "DELETE", &format!("/api/v1/jobs/{id}"), None, None).await;

    let (status, reports) = call(&h, "GET", "/api/v1/probes/pi-1/reports", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reports.as_array().unwrap().len(), 1);

    let (status, info) = call(&h, "DELETE", "/api/v1/probes/pi-1/config", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["custom_config"], false);
    assert_eq!(info["config"]["unicast"], false);

    assert_eq!(
        call(&h, "DELETE", "/api/v1/probes/pi-1", None, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&h, "GET", "/api/v1/probes/pi-1", None, None).await.0,
        StatusCode::NOT_FOUND
    );
}
