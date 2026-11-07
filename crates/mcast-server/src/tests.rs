use super::*;
use axum::body::Body;
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
        beacon: None,
        alert_webhook: None,
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
        beacon: None,
        alert_webhook: None,
    };
    let err = serve(cfg, Router::new(), async {}).await.unwrap_err();
    assert!(err.to_string().contains("without a token"));
}

async fn wait_until(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done().await {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn text_channels_over_http() {
    let h = harness(None);
    let g = "239.255.77.41:5004";
    let (status, ear) = call(
        &h,
        "POST",
        "/api/v1/texts",
        Some(json!({"group": g, "send": false, "name": "ear"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{ear}");
    let (status, desk) = call(
        &h,
        "POST",
        "/api/v1/texts",
        Some(json!({"group": g, "name": "desk"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{desk}");
    assert_eq!(desk["kind"], "text");
    let (ear, desk) = (ear["id"].as_u64().unwrap(), desk["id"].as_u64().unwrap());

    // The channel's group is taken for sending, by pages too.
    let (status, err) = call(
        &h,
        "POST",
        "/api/v1/senders",
        Some(json!({"group": g, "sap": false})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["job"], desk);

    let (status, sent) = call(
        &h,
        "POST",
        &format!("/api/v1/jobs/{desk}/text"),
        Some(json!({"text": "fire drill at 3"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent[0]["own"], true);
    wait_until("the line to arrive", async || {
        let (_, lines) = call(&h, "GET", &format!("/api/v1/jobs/{ear}/text"), None, None).await;
        lines.as_array().is_some_and(|l| !l.is_empty())
    })
    .await;
    let (_, lines) = call(&h, "GET", &format!("/api/v1/jobs/{ear}/text"), None, None).await;
    assert_eq!(lines[0]["text"], "fire drill at 3");
    assert_eq!(lines[0]["own"], false);

    let say = || Some(json!({"text": "hi"}));
    assert_eq!(
        call(&h, "POST", &format!("/api/v1/jobs/{ear}/text"), say(), None)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(&h, "POST", "/api/v1/jobs/999/text", say(), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &h,
            "POST",
            &format!("/api/v1/jobs/{desk}/text"),
            Some(json!({"txt": "hi"})),
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(
            &h,
            "POST",
            "/api/v1/texts",
            Some(json!({"stream": "nobody"})),
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    call(&h, "DELETE", &format!("/api/v1/jobs/{desk}"), None, None).await;
    assert_eq!(
        call(
            &h,
            "POST",
            &format!("/api/v1/jobs/{desk}/text"),
            say(),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    call(&h, "DELETE", &format!("/api/v1/jobs/{ear}"), None, None).await;
}

async fn put_media(h: &Harness, name: &str, data: Vec<u8>) -> StatusCode {
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/media/{name}"))
        .body(Body::from(data))
        .unwrap();
    h.app.clone().oneshot(req).await.unwrap().status()
}

async fn get_raw(h: &Harness, uri: &str) -> axum::response::Response {
    let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
    h.app.clone().oneshot(req).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn files_over_http_land_in_media() {
    let h = harness(None);
    let g = "239.255.77.42:5004";
    let pic: Vec<u8> = (0..30_000u32).map(|i| (i * 13 % 256) as u8).collect();
    assert_eq!(
        put_media(&h, "pic.png", pic.clone()).await,
        StatusCode::CREATED
    );

    let (status, rx) = call(
        &h,
        "POST",
        "/api/v1/file-receivers",
        Some(json!({"group": g, "rtcp": false})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rx}");
    let (status, tx) = call(
        &h,
        "POST",
        "/api/v1/file-senders",
        Some(json!({"group": g, "files": ["pic.png"], "sap": false, "rtcp": false, "rate_kbps": 20000, "loop": true})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{tx}");
    assert_eq!(
        tx["request"]["files"][0], "pic.png",
        "clients see media names, not paths"
    );

    wait_until("the copy to show in media", async || {
        let (_, media) = call(&h, "GET", "/api/v1/media", None, None).await;
        media
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "pic-2.png")
    })
    .await;
    let r = get_raw(&h, "/api/v1/media/pic-2.png").await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::CONTENT_TYPE], "image/png");
    let body = axum::body::to_bytes(r.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), pic.as_slice());

    // Anything that isn't a picture or audio downloads instead of rendering.
    assert_eq!(
        put_media(&h, "evil.html", b"<script>alert(1)</script>".to_vec()).await,
        StatusCode::CREATED
    );
    let r = get_raw(&h, "/api/v1/media/evil.html").await;
    assert_eq!(
        r.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    assert!(r.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    assert!(r.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .contains("sandbox"));
    assert_eq!(
        get_raw(&h, "/api/v1/media/missing.png").await.status(),
        StatusCode::NOT_FOUND
    );

    // Bad requests.
    for body in [json!({"files": ["../../etc/passwd"]}), json!({"files": []})] {
        let (status, err) =
            call(&h, "POST", "/api/v1/file-senders", Some(body.clone()), None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {err}");
    }
    // The looping sender still holds its group.
    let again = json!({"files": ["pic.png"], "group": g});
    let (status, err) = call(&h, "POST", "/api/v1/file-senders", Some(again), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["job"], tx["id"]);
    let (status, _) = call(
        &h,
        "POST",
        "/api/v1/file-receivers",
        Some(json!({"out_dir": "/tmp"})),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "clients can't choose where files go"
    );
    let (status, err) = call(
        &h,
        "POST",
        "/api/v1/file-receivers",
        Some(json!({"max_size_mb": 5000})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");

    for j in [&rx, &tx] {
        call(
            &h,
            "DELETE",
            &format!("/api/v1/jobs/{}", j["id"]),
            None,
            None,
        )
        .await;
    }
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

#[tokio::test(flavor = "multi_thread")]
async fn proves_it_knows_the_tokens_without_asking_for_one() {
    let h = harness_with(Some("admin"), Some("probe"));
    let nonce = "00112233445566778899aabb";
    let (status, hello) = call(
        &h,
        "GET",
        &format!("/api/v1/probe-hello?nonce={nonce}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hello}");
    let proofs: Vec<String> = serde_json::from_value(hello["proofs"].clone()).unwrap();
    assert!(proofs.contains(&mcast_control::probes::hello_proof("probe", nonce)));
    assert!(proofs.contains(&mcast_control::probes::hello_proof("admin", nonce)));
    assert!(!proofs.contains(&mcast_control::probes::hello_proof("guess", nonce)));
    let (status, _) = call(&h, "GET", "/api/v1/probe-hello?nonce=zz", None, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // The exemption is for that route only.
    let (status, _) = call(&h, "GET", "/api/v1/probes", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_alerts_and_changes_their_rules() {
    let h = harness(None);
    let lossy = |seq: u64| {
        json!({"seq": seq, "start_ms": seq * 10_000, "end_ms": seq * 10_000 + 10_000, "streams": [{
            "group": "239.69.0.1:5004", "name": "Zone 1", "source": "10.0.0.5:40000", "ssrc": 7,
            "payload_type": 0, "codec": null, "packets": 90, "bytes": 0, "packets_per_sec": 9.0,
            "kbit_per_sec": 0.0, "lost": 10, "late": 0, "loss_percent": 10.0, "jitter_ms": null,
            "max_gap_ms": 0.0, "ptime_ms": null, "ttl": null, "tos": null, "peak_dbfs": null,
            "rms_dbfs": null, "idle_ms": 0.0}]})
    };
    let body = json!({"version": "t", "boot": 1, "reports": [lossy(1), lossy(2)]});
    let (status, _) = call(&h, "POST", "/api/v1/probes/pi-1/checkin", Some(body), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, alerts) = call(&h, "GET", "/api/v1/alerts", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alerts["active"][0]["kind"], "multicast_loss", "{alerts}");
    assert_eq!(alerts["active"][0]["probe"], "pi-1");

    let (status, rules) = call(&h, "GET", "/api/v1/alert-rules", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rules["loss_percent"], 1.0);
    let (status, err) = call(
        &h,
        "PUT",
        "/api/v1/alert-rules",
        Some(json!({"loss_percent": 0})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    // Turning a kind off resolves its alerts.
    let (status, _) = call(
        &h,
        "PUT",
        "/api/v1/alert-rules",
        Some(json!({"multicast_loss": false})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, alerts) = call(&h, "GET", "/api/v1/alerts", None, None).await;
    assert_eq!(alerts["active"].as_array().unwrap().len(), 0);
    assert!(alerts["recent"][0]["message"]
        .as_str()
        .unwrap()
        .ends_with("(alert turned off)"));
}

#[tokio::test]
async fn refuses_a_probe_token_without_an_admin_token() {
    let cfg = ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: None,
        media_dir: std::env::temp_dir(),
        interface: Interface::Any,
        sap_groups: Vec::new(),
        monitor: None,
        max_upload: 0,
        probe_token: Some("probes".into()),
        probe_state: None,
        beacon: None,
        alert_webhook: None,
    };
    let err = serve(cfg, Router::new(), async {}).await.unwrap_err();
    assert!(
        err.to_string().contains("--probe-token without --token"),
        "{err}"
    );
}
