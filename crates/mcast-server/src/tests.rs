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
    let dir = std::env::temp_dir().join(format!("mcast-server-test-{}", mcast_core::random_u32()));
    let cfg = ServerConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        token: token.map(Into::into),
        media_dir: dir.clone(),
        interface: Interface::Any,
        sap_groups: mcast_core::sap::default_listen_groups(),
        monitor: None,
        max_upload: 1 << 20,
        phonebook: dir.join("phonebook.conf"),
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
        phonebook: PathBuf::from("phonebook.conf"),
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

#[tokio::test(flavor = "multi_thread")]
async fn phonebook_names_stand_in_for_groups() {
    let h = harness(None);
    let (_, list) = call(&h, "GET", "/api/v1/phonebook", None, None).await;
    assert_eq!(list, json!([]));

    let entry = json!({"group": "239.255.77.31:5004", "description": "Test zone"});
    let (status, e) = call(&h, "PUT", "/api/v1/phonebook/Zone%201", Some(entry), None).await;
    assert_eq!(status, StatusCode::CREATED, "{e}");
    assert_eq!(e["name"], "Zone 1");

    // Replace (same name, any case), then rename.
    let entry = json!({"group": "239.255.77.32:5004", "description": "Moved", "name": "zone-1"});
    let (status, e) = call(&h, "PUT", "/api/v1/phonebook/zone%201", Some(entry), None).await;
    assert_eq!(status, StatusCode::OK, "{e}");
    let (_, list) = call(&h, "GET", "/api/v1/phonebook", None, None).await;
    assert_eq!(
        list,
        json!([{"name": "zone-1", "group": "239.255.77.32:5004", "description": "Moved"}])
    );

    for bad in [
        json!({"group": "nope"}),
        json!({"group": "239.1.1.1:5", "description": "two\nlines"}),
        json!({"group": "239.1.1.1:5", "colour": "red"}),
    ] {
        let (status, err) = call(&h, "PUT", "/api/v1/phonebook/x", Some(bad), None).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    }
    let (status, _) = call(
        &h,
        "PUT",
        "/api/v1/phonebook/239.1.1.1:5",
        Some(json!({"group": "239.1.1.1:5"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A page by name goes to the saved group.
    let page = json!({"group": "ZONE-1", "sap": false, "encoding": "g711u"});
    let (status, job) = call(&h, "POST", "/api/v1/senders", Some(page), None).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!(job["group"], "239.255.77.32:5004");
    let id = job["id"].as_u64().unwrap();
    call(&h, "DELETE", &format!("/api/v1/jobs/{id}"), None, None).await;

    let (status, err) = call(
        &h,
        "POST",
        "/api/v1/senders",
        Some(json!({"group": "kitchen"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(err["error"].as_str().unwrap().contains("kitchen"), "{err}");

    let (status, _) = call(&h, "DELETE", "/api/v1/phonebook/zone-1", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&h, "DELETE", "/api/v1/phonebook/zone-1", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
