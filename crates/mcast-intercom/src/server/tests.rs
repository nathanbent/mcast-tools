//! The panel's API against the fake mcast server.

use super::*;
use crate::fake::Fake;
use axum::body::Body;
use axum::http::{header, Method, Request};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tower::ServiceExt;

struct Panel {
    app: Router,
    fake: Arc<Fake>,
    path: PathBuf,
    _closing: watch::Sender<bool>,
}

impl Drop for Panel {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}

fn panel(token: Option<&str>) -> Panel {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mcast-intercom-api-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let path = dir.join("intercom.json");
    let fake = Arc::new(Fake::default());
    let (tx, rx) = watch::channel(false);
    let state = AppState::new(fake.clone(), path.clone(), token.map(Into::into), rx);
    Panel {
        app: router(state),
        fake,
        path,
        _closing: tx,
    }
}

impl Panel {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.call_with(method, path, body, None).await
    }

    async fn call_with(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(format!("{API_PREFIX}{path}"));
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
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, v)
    }

    /// Only the status, for routes that don't answer JSON. `path` may carry a query.
    async fn status(&self, method: Method, path: &str, token: Option<&str>) -> StatusCode {
        let mut req = Request::builder()
            .method(method)
            .uri(format!("{API_PREFIX}{path}"));
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = self
            .app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        resp.status()
    }

    /// Make a token through the API (as `admin`); returns its secret.
    async fn token(&self, admin: Option<&str>, body: Value) -> String {
        let (s, v) = self
            .call_with(Method::POST, "/tokens", Some(body), admin)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["secret"].as_str().unwrap().to_string()
    }

    /// A zone and an announcement for playing.
    async fn set_up(&self) {
        let (s, _) = self
            .call(
                Method::POST,
                "/zones",
                Some(json!({"name": "Hall", "targets": [{"group": "zone-1"}]})),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        let (s, _) = self
            .call(
                Method::POST,
                "/announcements",
                Some(json!({"name": "Bell", "audio": "chime_only", "zones": ["hall"]})),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
    }
}

#[tokio::test]
async fn edits_zones_and_announcements() {
    let p = panel(None);
    let (s, v) = p.call(Method::GET, "/config", None).await;
    assert_eq!(
        (s, v),
        (
            StatusCode::OK,
            json!({"zones": [], "announcements": [], "schedules": []})
        )
    );

    // Ids come from names, and stay unique.
    let zone = json!({"name": "Upstairs zone", "targets": [{"group": "239.1.1.1:5000", "encoding": "g711u"}]});
    let (s, v) = p.call(Method::POST, "/zones", Some(zone.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["id"], "upstairs-zone");
    let mut other = zone.clone();
    other["name"] = "UPSTAIRS ZONE".into();
    let (s, v) = p.call(Method::POST, "/zones", Some(other)).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "names are unique too: {v}"
    );
    other = zone.clone();
    other["name"] = "Upstairs  Zone!".into();
    let (_, v) = p.call(Method::POST, "/zones", Some(other)).await;
    assert_eq!(v["id"], "upstairs-zone-2");
    let (s, _) = p
        .call(
            Method::POST,
            "/zones",
            Some(json!({"id": "upstairs-zone", "name": "X", "targets": [{"group": "a"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);

    let ann = json!({"name": "Lunch", "audio": {"media": "lunch.wav"}, "zones": ["upstairs-zone"], "repeat": 2});
    let (s, v) = p.call(Method::POST, "/announcements", Some(ann)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["id"], "lunch");

    // Mistakes are refused with a reason.
    for (body, want) in [
        (
            json!({"name": "Lunch", "audio": "chime_only", "zones": ["nope"]}),
            "no zone \"nope\"",
        ),
        (
            json!({"name": "Lunch", "audio": "chime_only", "zones": ["upstairs-zone"], "repeats": 3}),
            "unknown field `repeats`",
        ),
        (
            json!({"name": "Lunch", "audio": "chime_only", "zones": ["upstairs-zone"], "id": "other"}),
            "id can't be changed",
        ),
        (json!([1]), "expected a JSON object"),
    ] {
        let (s, v) = p
            .call(Method::PUT, "/announcements/lunch", Some(body))
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
        assert!(
            v["error"].as_str().unwrap().contains(want),
            "{v} should say {want}"
        );
    }
    let (s, _) = p
        .call(
            Method::PUT,
            "/announcements/nope",
            Some(json!({"name": "N", "audio": "chime_only", "zones": ["upstairs-zone"]})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A good change, kept on disk.
    let (s, v) = p
        .call(Method::PUT, "/announcements/lunch", Some(json!({"name": "Lunch bell", "audio": "chime_only", "zones": ["upstairs-zone"], "priority": "high"})))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let saved = crate::store::load(&p.path).unwrap();
    assert_eq!(saved.announcements[0].name, "Lunch bell");
    assert_eq!(saved.announcements[0].priority, crate::Priority::High);
    let (_, v) = p.call(Method::GET, "/announcements/lunch", None).await;
    assert_eq!(v["repeat_gap_s"], 2.0);

    // A zone in use can't go.
    let (s, v) = p.call(Method::DELETE, "/zones/upstairs-zone", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"].as_str().unwrap().contains("Lunch bell"), "{v}");
    let (s, _) = p.call(Method::DELETE, "/announcements/lunch", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = p.call(Method::DELETE, "/zones/upstairs-zone", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, v) = p.call(Method::GET, "/zones", None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn keeps_edits_made_by_hand() {
    let p = panel(None);
    p.set_up().await;
    let mut cfg = crate::store::load(&p.path).unwrap();
    cfg.zones[0].name = "Main hall".into();
    crate::store::save(&p.path, &cfg).unwrap();
    p.call(
        Method::POST,
        "/zones",
        Some(json!({"name": "Yard", "targets": [{"group": "239.1.1.9:5000"}]})),
    )
    .await;
    let (_, v) = p.call(Method::GET, "/zones/hall", None).await;
    assert_eq!(v["name"], "Main hall");

    // A file that doesn't load is reported, and left alone.
    std::fs::write(&p.path, "{").unwrap();
    let (s, v) = p.call(Method::GET, "/config", None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        v["error"].as_str().unwrap().contains("intercom.json"),
        "{v}"
    );
    let (s, _) = p.call(Method::DELETE, "/zones/hall", None).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(std::fs::read_to_string(&p.path).unwrap(), "{");
}

#[tokio::test]
async fn plays_and_stops() {
    let p = panel(None);
    p.set_up().await;
    let (s, run) = p.call(Method::POST, "/announcements/bell/play", None).await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    assert_eq!(run["state"], "playing");
    assert_eq!(run["targets"][0]["address"], "239.1.1.1:5000");
    assert_eq!(run["trigger"], json!({"type": "manual"}));
    let id = run["id"].as_u64().unwrap();

    // Busy: a second play finds its group taken.
    let (_, again) = p.call(Method::POST, "/announcements/bell/play", None).await;
    assert_eq!(again["state"], "failed");
    assert_eq!(again["targets"][0]["state"], "busy");

    let (s, v) = p.call(Method::DELETE, &format!("/runs/{id}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["state"], "stopped");
    let (s, _) = p.call(Method::DELETE, "/runs/999", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // All stop.
    p.call(Method::POST, "/announcements/bell/play", None).await;
    let (s, v) = p.call(Method::DELETE, "/runs", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v.as_array()
            .unwrap()
            .iter()
            .all(|r| r["state"] != "playing"),
        "{v}"
    );
    let (_, v) = p.call(Method::GET, "/runs", None).await;
    assert_eq!(v.as_array().unwrap().len(), 3);
    assert_eq!(p.fake.running_on("239.1.1.1:5000"), None);

    let (s, _) = p.call(Method::POST, "/announcements/nope/play", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Only ids: names are for the command line.
    let (s, _) = p.call(Method::POST, "/announcements/Bell/play", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Can't resolve a phonebook name without the server.
    p.fake.st.lock().unwrap().down = true;
    let (s, v) = p.call(Method::POST, "/announcements/bell/play", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    let (s, _) = p.call(Method::GET, "/media", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn passes_on_media_phonebook_and_connection() {
    let p = panel(None);
    let (_, v) = p.call(Method::GET, "/media", None).await;
    assert_eq!(v, json!(["lunch.wav"]));
    let (_, v) = p.call(Method::GET, "/phonebook", None).await;
    assert_eq!(v[0]["name"], "Zone-1");
    let (_, v) = p.call(Method::GET, "/backend", None).await;
    assert_eq!(v["connected"], true, "{v}");
    let (_, v) = p.call(Method::GET, "/health", None).await;
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn lists_hides_and_previews_chimes() {
    let p = panel(None);
    let (status, list) = p.call(Method::GET, "/chimes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["name"], "rising");
    let (status, list) = p
        .call(Method::PUT, "/chimes", Some(json!({"hidden": ["klaxon"]})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[2]["hidden"], true);
    assert_eq!(p.fake.st.lock().unwrap().hidden_chimes, ["klaxon"]);
    let (status, _) = p
        .call(Method::PUT, "/chimes", Some(json!({"hidden": ["foghorn"]})))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let req = Request::builder()
        .uri(format!("{API_PREFIX}/chimes/bell/preview"))
        .body(Body::empty())
        .unwrap();
    let resp = p.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "audio/wav");
    let (status, _) = p.call(Method::GET, "/chimes/foghorn/preview", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    p.fake.st.lock().unwrap().down = true;
    let (status, _) = p.call(Method::GET, "/chimes", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn streams_run_and_config_events() {
    let p = panel(None);
    p.set_up().await;
    let req = Request::builder()
        .uri(format!("{API_PREFIX}/events"))
        .body(Body::empty())
        .unwrap();
    let resp = p.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body().into_data_stream();

    p.call(Method::POST, "/announcements/bell/play", None).await;
    p.call(Method::DELETE, "/announcements/bell", None).await;

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(seen.contains("event: run") && seen.contains("event: config_changed")) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "events so far: {seen}");
        if let Ok(Some(Ok(chunk))) = tokio::time::timeout(left, body.next()).await {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    assert!(seen.contains(r#""type":"run""#), "{seen}");
}

#[tokio::test]
async fn needs_the_token_when_it_has_one() {
    let p = panel(Some("s3cret"));
    let (s, _) = p.call(Method::GET, "/health", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = p
        .call_with(Method::GET, "/health", None, Some("wrong"))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = p
        .call_with(Method::GET, "/health", None, Some("s3cret"))
        .await;
    assert_eq!(s, StatusCode::OK);
    // In the URL only where a browser can't send a header.
    assert_eq!(
        p.status(
            Method::GET,
            "/chimes/bell/preview?access_token=s3cret",
            None
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        p.status(Method::GET, "/health?access_token=s3cret", None)
            .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn wont_listen_on_the_network_without_a_token() {
    let cfg = ServeConfig {
        listen: "0.0.0.0:0".parse().unwrap(),
        token: None,
        config: PathBuf::from("unused.json"),
        tokens: None,
        web: true,
        web_dir: None,
        tls: None,
    };
    let e = serve(cfg, Arc::new(Fake::default()), async {})
        .await
        .unwrap_err();
    assert!(e.to_string().contains("without a token"), "{e}");
}

#[tokio::test]
async fn schedules_and_whats_coming_up() {
    let p = panel(None);
    p.set_up().await;
    let (s, v) = p
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"time_zone": "Europe/London"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effective_time_zone"], "Europe/London");
    let (s, v) = p
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"time_zone": "Mars/Olympus"})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        v["error"].as_str().unwrap().contains("unknown time zone"),
        "{v}"
    );

    // Each setting changes on its own; the others stay.
    let (s, v) = p
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"defaults": {"chime": "bell", "volume_db": -2},
                        "days_off": [{"name": "Holiday", "from": "2026-12-25"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["time_zone"], "Europe/London");
    assert_eq!(v["defaults"], json!({"chime": "bell", "volume_db": -2.0}));
    assert_eq!(v["days_off"][0]["name"], "Holiday");
    assert_eq!(v["schedules_paused"], false);
    let (_, v) = p.call(Method::GET, "/config", None).await;
    assert_eq!(v["defaults"]["chime"], "bell");
    let (s, v) = p
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"days_off": [{"from": "2026-12-25", "to": "2026-12-24"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (s, v) = p
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"defaults": {"chimes": "bell"}})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (_, v) = p.call(Method::GET, "/settings", None).await;
    assert_eq!(v["days_off"].as_array().unwrap().len(), 1, "{v}");
    let (_, v) = p
        .call(
            Method::PUT,
            "/settings",
            Some(json!({"defaults": {}, "days_off": []})),
        )
        .await;
    assert_eq!(v["defaults"], json!({}));
    assert_eq!(v["time_zone"], "Europe/London");

    // Named after what it plays when it has no name of its own.
    let every_day = json!({"announcement": "bell", "when": {"weekly": {
        "days": ["mon", "tue", "wed", "thu", "fri", "sat", "sun"], "times": ["00:00", "12:00"]}}});
    let (s, v) = p.call(Method::POST, "/schedules", Some(every_day)).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["id"], "bell");
    assert_eq!(v["enabled"], true);
    let (s, v) = p
        .call(
            Method::POST,
            "/schedules",
            Some(json!({"announcement": "bell", "when": {"once": "2026-13-01T00:00"}})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("isn't a date and time"),
        "{v}"
    );

    let (s, v) = p.call(Method::GET, "/upcoming?n=3", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["time_zone"], "Europe/London");
    let fires = v["fires"].as_array().unwrap();
    assert_eq!(fires.len(), 3, "{v}");
    assert_eq!(fires[0]["name"], "Bell");
    assert!(fires[0]["at_ms"].as_u64().unwrap() > v["now_ms"].as_u64().unwrap());
    assert_eq!(v["next"]["bell"], fires[0]["at_ms"]);

    // Paused: nothing coming up, until resumed.
    let paused = json!({"schedules_paused": true});
    let (s, v) = p.call(Method::PUT, "/settings", Some(paused)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = p.call(Method::GET, "/upcoming?n=3", None).await;
    assert_eq!(v["paused"], true);
    assert_eq!(v["fires"], json!([]));
    let resumed = json!({"schedules_paused": false});
    p.call(Method::PUT, "/settings", Some(resumed)).await;
    let (_, v) = p.call(Method::GET, "/upcoming?n=3", None).await;
    assert_eq!(v["fires"].as_array().unwrap().len(), 3, "{v}");
    let t = fires[0]["local"].as_str().unwrap();
    assert!(t.ends_with("T00:00:00") || t.ends_with("T12:00:00"), "{t}");

    // Switched off: nothing coming up.
    let (s, _) = p
        .call(Method::PUT, "/schedules/bell", Some(json!({"announcement": "bell", "enabled": false, "when": {"once": "2030-01-01T09:00"}})))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = p.call(Method::GET, "/upcoming", None).await;
    assert_eq!(v["fires"], json!([]));
    assert!(v["next"]["bell"].is_u64(), "still known when off: {v}");
    p.call(
        Method::PUT,
        "/schedules/bell",
        Some(json!({"announcement": "bell", "when": {"once": "2020-01-01T09:00"}})),
    )
    .await;
    let (_, v) = p.call(Method::GET, "/upcoming", None).await;
    assert_eq!(v["next"]["bell"], Value::Null, "passed");

    // An announcement a schedule plays can't go.
    let (s, v) = p.call(Method::DELETE, "/announcements/bell", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"].as_str().unwrap().contains("1 schedule"), "{v}");
    let (s, _) = p.call(Method::DELETE, "/schedules/bell", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = p.call(Method::DELETE, "/announcements/bell", None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn manages_sound_files_and_keeps_announcements_pointing_at_them() {
    let p = panel(None);
    p.set_up().await;
    let raw = |method: Method, path: &str, body: &'static [u8]| {
        Request::builder()
            .method(method)
            .uri(format!("{API_PREFIX}{path}"))
            .body(Body::from(body))
            .unwrap()
    };
    // Upload into a folder.
    let r = p
        .app
        .clone()
        .oneshot(raw(Method::PUT, "/media/bells/ring.wav", b"RIFF..."))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = p
        .app
        .clone()
        .oneshot(raw(Method::GET, "/media/bells%2Fring.wav", b""))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()[header::CONTENT_TYPE], "audio/wav");

    // An announcement uses it, so it can't be deleted.
    let (s, v) = p
        .call(
            Method::PUT,
            "/announcements/bell",
            Some(json!({"name": "Bell", "audio": {"media": "bells/ring.wav"},
                        "chime": {"file": "bells/ring.wav"}, "zones": ["hall"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = p.call(Method::DELETE, "/media/bells/ring.wav", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("announcement Bell"),
        "{v}"
    );

    // Moving the folder moves what points into it.
    let (s, v) = p
        .call(
            Method::POST,
            "/media-move",
            Some(json!({"from": "bells", "to": "old/bells"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["folder"], true);
    assert_eq!(v["updated"], json!(["announcement Bell"]));
    let (_, a) = p.call(Method::GET, "/announcements/bell", None).await;
    assert_eq!(a["audio"]["media"], "old/bells/ring.wav");
    assert_eq!(a["chime"]["file"], "old/bells/ring.wav");
    // And a file rename.
    let (s, _) = p
        .call(
            Method::POST,
            "/media-move",
            Some(json!({"from": "old/bells/ring.wav", "to": "old/bells/bell.wav"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, a) = p.call(Method::GET, "/announcements/bell", None).await;
    assert_eq!(a["audio"]["media"], "old/bells/bell.wav");

    // Hiding a folder hides what's in it; the library says who uses what.
    let (s, _) = p
        .call(
            Method::PUT,
            "/media-hidden",
            Some(json!({"name": "old", "hidden": true})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, lib) = p.call(Method::GET, "/library", None).await;
    let file = |name: &str| {
        lib["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(file("old/bells/bell.wav")["hidden"], true);
    assert_eq!(
        file("old/bells/bell.wav")["used_by"],
        json!(["announcement Bell"])
    );
    assert_eq!(file("lunch.wav")["hidden"], false);
    assert!(lib["folders"]
        .as_array()
        .unwrap()
        .contains(&json!({"name": "old", "hidden": true})));
    // A move keeps it hidden under its new name.
    p.call(
        Method::POST,
        "/media-move",
        Some(json!({"from": "old", "to": "archive"})),
    )
    .await;
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert_eq!(cfg["hidden_media"], json!(["archive"]));

    // Free it, then delete the file and the empty folders.
    p.call(
        Method::PUT,
        "/announcements/bell",
        Some(json!({"name": "Bell", "audio": "chime_only", "zones": ["hall"]})),
    )
    .await;
    let (s, _) = p
        .call(Method::DELETE, "/media/archive/bells/bell.wav", None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = p.call(Method::DELETE, "/media-folders/archive", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "archive/bells is still in it");
    for f in ["archive/bells", "archive"] {
        let (s, v) = p
            .call(Method::DELETE, &format!("/media-folders/{f}"), None)
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{f}: {v}");
    }
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert!(cfg.get("hidden_media").is_none(), "{cfg}");

    let (s, _) = p
        .call(
            Method::POST,
            "/media-move",
            Some(json!({"from": "lunch.wav", "to": "../x.wav"})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn sites_and_schedule_groups() {
    let p = panel(None);
    p.set_up().await;
    let (s, site) = p
        .call(
            Method::POST,
            "/sites",
            Some(json!({"name": "North campus", "time_zone": "America/New_York"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{site}");
    assert_eq!(site["id"], "north-campus");
    let (s, g) = p
        .call(
            Method::POST,
            "/schedule-groups",
            Some(json!({"name": "Exam week"})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    assert_eq!(g["enabled"], true);

    // The hall belongs to the site; a schedule there, in the group.
    let (s, v) = p
        .call(
            Method::PUT,
            "/zones/hall",
            Some(json!({"name": "Hall", "targets": [{"group": "zone-1"}], "site": "north-campus"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = p
        .call(
            Method::POST,
            "/schedules",
            Some(
                json!({"announcement": "bell", "site": "north-campus", "group": "exam-week",
                        "when": {"weekly": {"days": ["mon"], "times": ["09:00"]}}}),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, up) = p
        .call(Method::GET, "/upcoming?site=north-campus", None)
        .await;
    assert_eq!(up["fires"][0]["site"], "north-campus", "{up}");
    assert_eq!(up["sites"]["north-campus"]["time_zone"], "America/New_York");
    let (_, other) = p.call(Method::GET, "/upcoming?site=", None).await;
    assert_eq!(other["fires"], json!([]));

    // Switching the group off takes it out of what's coming up.
    let (s, _) = p
        .call(
            Method::PUT,
            "/schedule-groups/exam-week",
            Some(json!({"name": "Exam week", "enabled": false})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, up) = p.call(Method::GET, "/upcoming", None).await;
    assert_eq!(up["fires"], json!([]));

    // In use: neither can go.
    for path in ["/sites/north-campus", "/schedule-groups/exam-week"] {
        let (s, v) = p.call(Method::DELETE, path, None).await;
        assert_eq!(s, StatusCode::CONFLICT, "{path}: {v}");
    }
    let (s, v) = p
        .call(
            Method::POST,
            "/sites",
            Some(json!({"name": "Bad", "time_zone": "Mars/Base"})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
}

#[tokio::test]
async fn live_announcements_are_talked_on_not_played_or_scheduled() {
    let p = panel(None);
    p.set_up().await;
    let (s, v) = p
        .call(
            Method::POST,
            "/announcements",
            Some(json!({"name": "Talk", "audio": "live", "zones": ["hall"]})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = p.call(Method::POST, "/announcements/talk/play", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(v["error"].as_str().unwrap().contains("push to talk"), "{v}");
    let (s, v) = p
        .call(
            Method::POST,
            "/schedules",
            Some(json!({"announcement": "talk", "when": {"once": "2027-01-01T09:00"}})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        v["error"].as_str().unwrap().contains("can't be scheduled"),
        "{v}"
    );
    let (s, _) = p
        .call(
            Method::PUT,
            "/announcements/talk",
            Some(json!({"name": "Talk", "audio": "live", "zones": ["hall"], "repeat": 2})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---- access tokens ----

/// Every route the router has, as routed. Kept in step with `router`: a
/// route missing here, or with no rule in `access`, fails below.
const ROUTES: &[(&str, &str)] = &[
    ("GET", "/health"),
    ("GET", "/backend"),
    ("GET", "/config"),
    ("GET", "/zones"),
    ("POST", "/zones"),
    ("GET", "/zones/{id}"),
    ("PUT", "/zones/{id}"),
    ("DELETE", "/zones/{id}"),
    ("GET", "/announcements"),
    ("POST", "/announcements"),
    ("GET", "/announcements/{id}"),
    ("PUT", "/announcements/{id}"),
    ("DELETE", "/announcements/{id}"),
    ("POST", "/announcements/{id}/play"),
    ("GET", "/live/{id}"),
    ("GET", "/schedules"),
    ("POST", "/schedules"),
    ("GET", "/schedules/{id}"),
    ("PUT", "/schedules/{id}"),
    ("DELETE", "/schedules/{id}"),
    ("GET", "/sites"),
    ("POST", "/sites"),
    ("GET", "/sites/{id}"),
    ("PUT", "/sites/{id}"),
    ("DELETE", "/sites/{id}"),
    ("GET", "/schedule-groups"),
    ("POST", "/schedule-groups"),
    ("GET", "/schedule-groups/{id}"),
    ("PUT", "/schedule-groups/{id}"),
    ("DELETE", "/schedule-groups/{id}"),
    ("GET", "/categories"),
    ("POST", "/categories"),
    ("GET", "/categories/{id}"),
    ("PUT", "/categories/{id}"),
    ("DELETE", "/categories/{id}"),
    ("GET", "/playlists"),
    ("POST", "/playlists"),
    ("GET", "/playlists/{id}"),
    ("PUT", "/playlists/{id}"),
    ("DELETE", "/playlists/{id}"),
    ("PUT", "/layout/{tab}"),
    ("DELETE", "/layout/{tab}"),
    ("GET", "/upcoming"),
    ("GET", "/settings"),
    ("PUT", "/settings"),
    ("GET", "/runs"),
    ("DELETE", "/runs"),
    ("GET", "/runs/{id}"),
    ("DELETE", "/runs/{id}"),
    ("GET", "/media"),
    ("GET", "/media/{*name}"),
    ("PUT", "/media/{*name}"),
    ("DELETE", "/media/{*name}"),
    ("GET", "/library"),
    ("POST", "/media-folders"),
    ("DELETE", "/media-folders/{*name}"),
    ("POST", "/media-move"),
    ("PUT", "/media-hidden"),
    ("PUT", "/media-tags"),
    ("GET", "/phonebook"),
    ("GET", "/chimes"),
    ("PUT", "/chimes"),
    ("GET", "/chimes/{name}/preview"),
    ("GET", "/events"),
    ("GET", "/tokens"),
    ("POST", "/tokens"),
    ("PUT", "/tokens/{id}"),
    ("DELETE", "/tokens/{id}"),
    ("GET", "/trigger/{which}"),
    ("POST", "/trigger/{which}"),
    ("GET", "/trigger/{which}/stop"),
    ("POST", "/trigger/{which}/stop"),
    ("GET", "/stop-all"),
    ("POST", "/stop-all"),
    ("GET", "/webhooks"),
    ("POST", "/webhooks"),
    ("PUT", "/webhooks/{id}"),
    ("DELETE", "/webhooks/{id}"),
    ("POST", "/webhooks/{id}/test"),
];

#[test]
fn every_route_has_an_access_rule() {
    use crate::tokens::Role;
    for (m, path) in ROUTES {
        let m: Method = m.parse().unwrap();
        let need = access(&m, path).unwrap_or_else(|| panic!("{m} {path} has no access rule"));
        // What each role may do, in short: a few spot checks per role.
        let admin = need.allows(Role::Admin);
        assert!(admin, "{m} {path}: admin can do everything");
        let changes_setup = m != Method::GET
            && !path.starts_with("/media")
            && !path.starts_with("/runs")
            && !path.starts_with("/trigger")
            && *path != "/stop-all"
            && !path.ends_with("/play");
        if changes_setup || path.starts_with("/tokens") {
            for r in [Role::Operator, Role::Trigger, Role::Media] {
                assert!(!need.allows(r), "{m} {path}: only admin, not {r}");
            }
        }
    }
    let can = |m: Method, p: &str, r| access(&m, p).unwrap().allows(r);
    assert!(can(Method::GET, "/config", Role::Operator));
    assert!(!can(Method::GET, "/config", Role::Trigger));
    assert!(!can(Method::GET, "/config", Role::Media));
    assert!(can(Method::POST, "/announcements/{id}/play", Role::Trigger));
    assert!(!can(Method::DELETE, "/runs", Role::Trigger));
    assert!(!can(Method::GET, "/live/{id}", Role::Trigger));
    assert!(can(Method::GET, "/live/{id}", Role::Operator));
    assert!(can(Method::PUT, "/media/{*name}", Role::Media));
    assert!(!can(Method::PUT, "/media/{*name}", Role::Operator));
    assert!(can(Method::GET, "/media/{*name}", Role::Operator));
    assert!(!can(Method::GET, "/media", Role::Trigger));
    assert!(can(Method::GET, "/events", Role::Media));
    assert!(access(&Method::PATCH, "/zones/{id}").is_some_and(|a| !a.allows(Role::Operator)));
    assert_eq!(access(&Method::GET, "/no-such-route"), None);
}

#[tokio::test]
async fn tokens_get_their_roles() {
    let p = panel(Some("s3cret"));
    let admin = Some("s3cret");
    let operator = p
        .token(admin, json!({"name": "Desk", "role": "operator"}))
        .await;
    let trigger = p
        .token(admin, json!({"name": "Doorbell", "role": "trigger"}))
        .await;
    let media = p
        .token(admin, json!({"name": "Recorder", "role": "media"}))
        .await;
    let made_admin = p
        .token(admin, json!({"name": "Ops", "role": "admin"}))
        .await;

    // Who's asking.
    let (_, v) = p
        .call_with(Method::GET, "/health", None, Some(&trigger))
        .await;
    assert_eq!(
        (v["name"].clone(), v["role"].clone()),
        (json!("Doorbell"), json!("trigger"))
    );
    let (_, v) = p.call_with(Method::GET, "/health", None, admin).await;
    assert_eq!(v["name"], STARTUP_TOKEN);

    /// What, how, where, with what, and whether [admin, operator, trigger, media] may.
    type Case<'a> = (&'a str, Method, &'a str, Option<Value>, [bool; 4]);
    let cases: &[Case] = &[
        // [admin, operator, trigger, media]: allowed?
        (
            "config",
            Method::GET,
            "/config",
            None,
            [true, true, false, false],
        ),
        (
            "zones",
            Method::GET,
            "/zones",
            None,
            [true, true, false, false],
        ),
        (
            "new zone",
            Method::POST,
            "/zones",
            Some(json!({"name": "X", "targets": [{"group": "zone-1"}]})),
            [true, false, false, false],
        ),
        (
            "settings",
            Method::PUT,
            "/settings",
            Some(json!({})),
            [true, false, false, false],
        ),
        (
            "runs",
            Method::GET,
            "/runs",
            None,
            [true, true, true, false],
        ),
        (
            "library",
            Method::GET,
            "/library",
            None,
            [true, true, false, true],
        ),
        (
            "folder",
            Method::POST,
            "/media-folders",
            Some(json!({"name": "bells"})),
            [true, false, false, true],
        ),
        (
            "tokens",
            Method::GET,
            "/tokens",
            None,
            [true, false, false, false],
        ),
        (
            "all stop",
            Method::DELETE,
            "/runs",
            None,
            [true, true, false, false],
        ),
    ];
    for (what, m, path, body, allowed) in cases {
        for (i, t) in [&made_admin, &operator, &trigger, &media]
            .into_iter()
            .enumerate()
        {
            let (s, v) = p.call_with(m.clone(), path, body.clone(), Some(t)).await;
            if allowed[i] {
                assert!(s.is_success(), "{what} as token {i}: {s} {v}");
            } else {
                assert_eq!(s, StatusCode::FORBIDDEN, "{what} as token {i}: {v}");
                assert!(v["error"].as_str().unwrap().contains("can't"), "{v}");
            }
        }
    }
}

#[tokio::test]
async fn trigger_tokens_play_only_their_announcements_and_runs_say_who() {
    let p = panel(Some("s3cret"));
    let admin = Some("s3cret");
    for (name, zone) in [("Hall", "zone-1"), ("Yard", "239.1.1.2:5000")] {
        let (s, v) = p
            .call_with(
                Method::POST,
                "/zones",
                Some(json!({"name": name, "targets": [{"group": zone}]})),
                admin,
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    for (name, zone) in [("Bell", "hall"), ("Gate", "yard")] {
        let (s, v) = p
            .call_with(
                Method::POST,
                "/announcements",
                Some(json!({"name": name, "audio": "chime_only", "zones": [zone]})),
                admin,
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let doorbell = p
        .token(
            admin,
            json!({"name": "Doorbell", "role": "trigger", "announcements": ["Bell"]}),
        )
        .await;
    let desk = p
        .token(admin, json!({"name": "Desk", "role": "operator"}))
        .await;

    let (s, run) = p
        .call_with(
            Method::POST,
            "/announcements/bell/play",
            None,
            Some(&doorbell),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    assert_eq!(run["trigger"], json!({"type": "api", "by": "Doorbell"}));
    let (s, v) = p
        .call_with(
            Method::POST,
            "/announcements/gate/play",
            None,
            Some(&doorbell),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");

    let (_, gate) = p
        .call_with(Method::POST, "/announcements/gate/play", None, Some(&desk))
        .await;
    assert_eq!(gate["trigger"], json!({"type": "manual", "by": "Desk"}));
    let (_, v) = p
        .call_with(Method::POST, "/announcements/gate/play", None, admin)
        .await;
    assert_eq!(v["trigger"], json!({"type": "manual", "by": STARTUP_TOKEN}));

    // It sees and stops only runs of its own announcements.
    let (_, runs) = p
        .call_with(Method::GET, "/runs", None, Some(&doorbell))
        .await;
    let runs = runs.as_array().unwrap();
    assert!(
        !runs.is_empty() && runs.iter().all(|r| r["announcement"] == "bell"),
        "{runs:?}"
    );
    let gate_run = format!("/runs/{}", gate["id"]);
    let (s, _) = p
        .call_with(Method::DELETE, &gate_run, None, Some(&doorbell))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = p
        .call_with(Method::GET, &gate_run, None, Some(&doorbell))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = p
        .call_with(
            Method::DELETE,
            &format!("/runs/{}", run["id"]),
            None,
            Some(&doorbell),
        )
        .await;
    assert_eq!((s, v["state"].clone()), (StatusCode::OK, json!("stopped")));

    // Never in the URL (until the GET trigger API).
    assert_eq!(
        p.status(
            Method::GET,
            &format!("/chimes/bell/preview?access_token={doorbell}"),
            None
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn managing_tokens() {
    let p = panel(Some("s3cret"));
    let admin = Some("s3cret");
    let (s, v) = p
        .call_with(
            Method::POST,
            "/tokens",
            Some(json!({"name": "Desk", "role": "operator"})),
            admin,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let secret = v["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("mci_"));
    let id = v["token"]["id"].as_str().unwrap().to_string();
    assert!(v["token"].get("hash").is_none(), "{v}");

    // The secret is shown once: not listed, not in the file.
    p.call_with(Method::GET, "/health", None, Some(&secret))
        .await;
    let (_, list) = p.call_with(Method::GET, "/tokens", None, admin).await;
    assert_eq!(list[0]["name"], "Desk");
    assert_eq!(list[0]["uses"], 1);
    assert_eq!(list[0]["expired"], false);
    assert!(list[0].get("hash").is_none() && !list.to_string().contains(&secret));
    let file = p.path.with_file_name("tokens.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(&secret) && text.contains("sha256:"));

    // Mistakes.
    let bad = [
        json!({"name": "Desk", "role": "admin"}),
        json!({"name": "Other", "role": "boss"}),
        json!({"role": "admin"}),
        json!({"name": "Other", "role": "operator", "announcements": ["x"]}),
        json!({"name": "Other", "role": "trigger", "announcements": ["nope"]}),
        json!({"name": "Other", "role": "admin", "expires": "soon"}),
        json!({"name": "Other", "role": "admin", "expires": "2001-01-01"}),
    ];
    for b in bad {
        let (s, v) = p
            .call_with(Method::POST, "/tokens", Some(b.clone()), admin)
            .await;
        assert!(s.is_client_error(), "{b}: {s} {v}");
    }

    // Rename, set an expiry, then clear it; the role can't change.
    let path = format!("/tokens/{id}");
    let (s, v) = p
        .call_with(
            Method::PUT,
            &path,
            Some(json!({"name": "Front desk", "expires": "2999-12-31"})),
            admin,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["name"].clone(), v["expires"].clone()),
        (json!("Front desk"), json!("2999-12-31"))
    );
    let (_, v) = p
        .call_with(Method::PUT, &path, Some(json!({"expires": null})), admin)
        .await;
    assert_eq!(v["expires"], Value::Null);
    let (s, _) = p
        .call_with(Method::PUT, &path, Some(json!({"role": "admin"})), admin)
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Revoked: refused at once.
    let (s, v) = p.call_with(Method::DELETE, &path, None, admin).await;
    assert_eq!(
        (s, v["name"].clone()),
        (StatusCode::OK, json!("Front desk"))
    );
    let (s, _) = p
        .call_with(Method::GET, "/health", None, Some(&secret))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = p.call_with(Method::DELETE, &path, None, admin).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tokens_changed_on_disk_and_expired_ones() {
    let p = panel(Some("s3cret"));
    let file = p.path.with_file_name("tokens.json");
    // As `mcast intercom token add` would, while the panel runs.
    let cfg = crate::store::load(&p.path).unwrap();
    let mut f = crate::tokens::load(&file).unwrap();
    let new = |name: &str| crate::tokens::NewToken {
        name: name.into(),
        role: crate::tokens::Role::Operator,
        announcements: vec![],
        expires: None,
    };
    let (_, fresh) = f.add(new("Fresh"), &cfg, 1).unwrap();
    let (mut old, stale) = f.add(new("Old"), &cfg, 1).unwrap();
    old.expires = Some("2001-01-01".into());
    old.expires_ms = Some(978_307_200_000);
    f.tokens[1] = old;
    crate::tokens::save(&file, &f).unwrap();

    let (s, _) = p
        .call_with(Method::GET, "/health", None, Some(&fresh))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = p
        .call_with(Method::GET, "/health", None, Some(&stale))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(v["error"].as_str().unwrap().contains("expired"), "{v}");
    let (_, list) = p
        .call_with(Method::GET, "/tokens", None, Some("s3cret"))
        .await;
    assert_eq!(list[1]["expired"], true);

    f.remove("Fresh").unwrap();
    crate::tokens::save(&file, &f).unwrap();
    let (s, _) = p
        .call_with(Method::GET, "/health", None, Some(&fresh))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_open_panel_still_gives_known_tokens_their_role() {
    let p = panel(None);
    p.set_up().await;
    let desk = p
        .token(None, json!({"name": "Desk", "role": "operator"}))
        .await;
    let (s, _) = p.call(Method::DELETE, "/zones/hall", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "no token: admin, as before");
    let (s, _) = p
        .call_with(Method::DELETE, "/zones/hall", None, Some(&desk))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Unknown tokens are ignored, as before.
    let (s, _) = p
        .call_with(Method::GET, "/tokens", None, Some("mci_whatever"))
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn listens_on_the_network_with_an_admin_token_from_the_file() {
    let dir = std::env::temp_dir().join(format!("mcast-intercom-api-admin-{}", std::process::id()));
    let config = dir.join("intercom.json");
    let tokens = dir.join("tokens.json");
    let cfg = || ServeConfig {
        listen: "0.0.0.0:0".parse().unwrap(),
        token: None,
        config: config.clone(),
        tokens: Some(tokens.clone()),
        web: false,
        web_dir: None,
        tls: None,
    };
    let mut f = crate::tokens::TokenFile::default();
    let new = |role| crate::tokens::NewToken {
        name: format!("{role}"),
        role,
        announcements: vec![],
        expires: None,
    };
    f.add(new(crate::tokens::Role::Operator), &Default::default(), 1)
        .unwrap();
    crate::tokens::save(&tokens, &f).unwrap();
    let e = serve(cfg(), Arc::new(Fake::default()), async {})
        .await
        .unwrap_err();
    assert!(e.to_string().contains("without a token"), "{e}");

    f.add(new(crate::tokens::Role::Admin), &Default::default(), 1)
        .unwrap();
    crate::tokens::save(&tokens, &f).unwrap();
    serve(cfg(), Arc::new(Fake::default()), async {})
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

// ---- the trigger API ----

impl Panel {
    /// Zones Hall and Yard; Bell (to both), Gate (to Yard) and Talk (live, to Hall).
    async fn set_up_two(&self, admin: Option<&str>) {
        for (name, zone) in [("Hall", "zone-1"), ("Yard", "239.1.1.2:5000")] {
            let body = json!({"name": name, "targets": [{"group": zone}]});
            let (s, v) = self
                .call_with(Method::POST, "/zones", Some(body), admin)
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
        }
        for (name, audio, zones) in [
            ("Bell", json!("chime_only"), json!(["hall", "yard"])),
            ("Gate", json!("chime_only"), json!(["yard"])),
            ("Talk", json!("live"), json!(["hall"])),
        ] {
            let body = json!({"name": name, "audio": audio, "zones": zones});
            let (s, v) = self
                .call_with(Method::POST, "/announcements", Some(body), admin)
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
        }
    }
}

#[tokio::test]
async fn the_trigger_api_plays_by_name_and_stops() {
    let p = panel(Some("s3cret"));
    let admin = Some("s3cret");
    p.set_up_two(admin).await;
    let doorbell = p
        .token(
            admin,
            json!({"name": "Doorbell", "role": "trigger", "announcements": ["bell"]}),
        )
        .await;
    let bell = Some(doorbell.as_str());

    // By name, any case, with a POST and no body.
    let (s, run) = p.call_with(Method::POST, "/trigger/BELL", None, bell).await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    assert_eq!(run["trigger"], json!({"type": "api", "by": "Doorbell"}));
    assert_eq!(run["targets"].as_array().unwrap().len(), 2);

    // Stop it by name.
    let (s, v) = p
        .call_with(Method::POST, "/trigger/Bell/stop", None, bell)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v[0]["state"], "stopped");
    let (_, v) = p
        .call_with(Method::POST, "/trigger/Bell/stop", None, bell)
        .await;
    assert_eq!(v, json!([]), "nothing left to stop");

    // GET with the token in the URL, only to the yard, twice over.
    let (s, run) = p
        .call(
            Method::GET,
            &format!("/trigger/bell?access_token={doorbell}&zones=Yard&repeat=2"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    assert_eq!(run["targets"].as_array().unwrap().len(), 1);
    assert_eq!(run["rounds"], 2);
    // ...and stop everything it may stop, also by GET.
    let (s, v) = p
        .call(
            Method::GET,
            &format!("/stop-all?access_token={doorbell}"),
            None,
        )
        .await;
    assert_eq!((s, v.as_array().unwrap().len()), (StatusCode::OK, 1));

    // Not its announcement, a zone the announcement doesn't use, a bad repeat,
    // a live one, an unknown one.
    let (s, _) = p.call_with(Method::POST, "/trigger/gate", None, bell).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = p
        .call_with(
            Method::POST,
            "/trigger/gate",
            Some(json!({"zones": ["hall"]})),
            admin,
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert!(v["error"].as_str().unwrap().contains("isn't one of"), "{v}");
    let (s, _) = p
        .call_with(
            Method::POST,
            "/trigger/gate",
            Some(json!({"repeat": 0})),
            admin,
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = p
        .call_with(Method::POST, "/trigger/talk", None, admin)
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = p
        .call_with(Method::POST, "/trigger/nope", None, admin)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A link preview's HEAD, or a PUT, pages nobody.
    assert_eq!(
        p.status(
            Method::HEAD,
            &format!("/trigger/bell?access_token={doorbell}"),
            None
        )
        .await,
        StatusCode::METHOD_NOT_ALLOWED
    );
    let (s, _) = p.call_with(Method::PUT, "/trigger/bell", None, bell).await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    let (_, runs) = p.call_with(Method::GET, "/runs", None, admin).await;
    assert_eq!(runs.as_array().unwrap().len(), 2);

    // Other roles use a header; operator and admin may trigger, media may not.
    let desk = p
        .token(admin, json!({"name": "Desk", "role": "operator"}))
        .await;
    let (s, run) = p
        .call_with(Method::POST, "/trigger/gate", None, Some(&desk))
        .await;
    assert_eq!(
        (s, run["trigger"].clone()),
        (StatusCode::CREATED, json!({"type": "api", "by": "Desk"}))
    );
    let (s, _) = p
        .call(
            Method::GET,
            &format!("/trigger/gate?access_token={desk}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let rec = p
        .token(admin, json!({"name": "Recorder", "role": "media"}))
        .await;
    let (s, _) = p
        .call_with(Method::POST, "/trigger/gate", None, Some(&rec))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = p.call_with(Method::POST, "/stop-all", None, admin).await;
    assert_eq!((s, v.as_array().unwrap().len()), (StatusCode::OK, 1));
}

// ---- webhooks ----

/// The secret receivers check signatures with (one test signs).
static HOOK_SECRET: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// A receiver that keeps what it's sent: (headers, body), answering `status`.
async fn receiver(
    status: u16,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, Value)>>>,
) {
    let got: Arc<std::sync::Mutex<Vec<(axum::http::HeaderMap, Value)>>> = Arc::default();
    let keep = got.clone();
    let app = Router::new().route(
        "/hook",
        post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let keep = keep.clone();
                async move {
                    let v: Value = serde_json::from_slice(&body).unwrap();
                    // The signature is over the raw body, as sent.
                    if let (Some(sig), Some(ts)) = (
                        headers.get("x-mcast-signature"),
                        headers.get("x-mcast-timestamp"),
                    ) {
                        let ts: u64 = ts.to_str().unwrap().parse().unwrap();
                        let secret = HOOK_SECRET.lock().unwrap().clone();
                        let ok =
                            crate::webhooks::signature(&secret, ts, &body) == sig.to_str().unwrap();
                        let mut v = v;
                        v["signature_ok"] = ok.into();
                        keep.lock().unwrap().push((headers, v));
                    } else {
                        keep.lock().unwrap().push((headers, v));
                    }
                    StatusCode::from_u16(status).unwrap()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, got)
}

async fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(t) = f() {
            return t;
        }
        assert!(Instant::now() < until, "timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn webhooks_hear_runs_signed_and_filtered() {
    let p = panel(None);
    p.set_up_two(None).await;
    let (url, got) = receiver(204).await;
    let (other_url, other) = receiver(204).await;

    let (s, v) = p
        .call(
            Method::POST,
            "/webhooks",
            Some(json!({"name": "Logger", "url": url})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let secret = v["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("whsec_"));
    let id = v["webhook"]["id"].as_str().unwrap().to_string();
    // Receivers check signatures with this secret.
    *HOOK_SECRET.lock().unwrap() = secret.clone();
    let (s, v) = p
        .call(
            Method::POST,
            "/webhooks",
            Some(json!({"name": "Failures only", "url": other_url, "events": ["run_failed"], "sign": false})),
        )
        .await;
    assert_eq!((s, v["secret"].clone()), (StatusCode::CREATED, Value::Null));
    let (s, _) = p
        .call(
            Method::POST,
            "/webhooks",
            Some(json!({"name": "Bad", "url": "ftp://x"})),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // The secret isn't listed or readable by others; it's in the file, owner-only.
    let (_, list) = p.call(Method::GET, "/webhooks", None).await;
    assert_eq!(list[0]["signed"], true);
    assert!(!list.to_string().contains(&secret));
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert!(!cfg.to_string().contains(&secret));

    let (_, run) = p.call(Method::POST, "/trigger/bell", None).await;
    let started = wait_for(|| got.lock().unwrap().first().cloned()).await;
    assert_eq!(started.1["event"], "run_started");
    assert_eq!(started.1["run"]["id"], run["id"]);
    assert_eq!(started.1["signature_ok"], true, "{}", started.1);
    assert_eq!(started.0["x-mcast-event"], "run_started");
    assert!(started.0["user-agent"]
        .to_str()
        .unwrap()
        .starts_with("mcast-intercom/"));

    p.call(Method::DELETE, &format!("/runs/{}", run["id"]), None)
        .await;
    let ended = wait_for(|| got.lock().unwrap().get(1).cloned()).await;
    assert_eq!(ended.1["event"], "run_ended");
    assert_eq!(ended.1["run"]["state"], "stopped");

    // A busy group fails the second play: only that goes to "Failures only".
    p.call(Method::POST, "/trigger/gate", None).await;
    let (_, failed) = p.call(Method::POST, "/trigger/gate", None).await;
    assert_eq!(failed["state"], "failed", "{failed}");
    let f = wait_for(|| other.lock().unwrap().first().cloned()).await;
    assert_eq!(f.1["event"], "run_failed");
    assert!(f.0.get("x-mcast-signature").is_none());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(other.lock().unwrap().len(), 1);

    // Switched off: nothing more.
    let before = got.lock().unwrap().len();
    let (s, _) = p
        .call(
            Method::PUT,
            &format!("/webhooks/{id}"),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    p.call(Method::POST, "/stop-all", None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(got.lock().unwrap().len(), before);

    // How the last delivery went shows in the list.
    let (_, list) = p.call(Method::GET, "/webhooks", None).await;
    assert_eq!(list[1]["last_delivery"]["ok"], true, "{list}");
    assert_eq!(list[1]["last_delivery"]["status"], 204);

    let (s, _) = p
        .call(Method::DELETE, &format!("/webhooks/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = p
        .call(Method::DELETE, &format!("/webhooks/{id}"), None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_tests_say_how_it_went() {
    let p = panel(None);
    let (url, got) = receiver(404).await;
    let (_, v) = p
        .call(
            Method::POST,
            "/webhooks",
            Some(json!({"name": "Gone", "url": url, "sign": false})),
        )
        .await;
    let id = v["webhook"]["id"].as_str().unwrap().to_string();
    let (s, d) = p
        .call(Method::POST, &format!("/webhooks/{id}/test"), None)
        .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    // A 4xx isn't retried.
    assert_eq!(
        (d["ok"].clone(), d["status"].clone(), d["attempts"].clone()),
        (json!(false), json!(404), json!(1))
    );
    assert_eq!(got.lock().unwrap()[0].1["event"], "test");

    // Nobody listening: tried twice, then given up.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/hook", l.local_addr().unwrap())
    };
    let (_, v) = p
        .call(
            Method::POST,
            "/webhooks",
            Some(json!({"name": "Dead", "url": dead})),
        )
        .await;
    let id = v["webhook"]["id"].as_str().unwrap().to_string();
    let (_, d) = p
        .call(Method::POST, &format!("/webhooks/{id}/test"), None)
        .await;
    assert_eq!(
        (d["ok"].clone(), d["attempts"].clone()),
        (json!(false), json!(2)),
        "{d}"
    );
    assert!(d["error"].as_str().is_some());
    let (s, _) = p.call(Method::POST, "/webhooks/w_nope/test", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sounds_are_checked_and_media_tokens_upload_them() {
    let p = panel(Some("s3cret"));
    let rec = p
        .token(Some("s3cret"), json!({"name": "Recorder", "role": "media"}))
        .await;
    let put = |name: &str, data: &'static [u8]| {
        let req = Request::builder()
            .method(Method::PUT)
            .uri(format!("{API_PREFIX}/media/{name}"))
            .header(header::AUTHORIZATION, format!("Bearer {rec}"))
            .body(Body::from(data))
            .unwrap();
        p.app.clone().oneshot(req)
    };
    let r = put("daily/message.wav", b"RIFF and some audio")
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let v: Value =
        serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap())
            .unwrap();
    assert_eq!(v["audio"]["rate"], 48000, "{v}");
    let r = put("daily/message.wav", b"not audio, a mistake")
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let (_, lib) = p.call_with(Method::GET, "/library", None, Some(&rec)).await;
    let f = &lib["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "daily/message.wav")
        .unwrap()
        .clone();
    assert_eq!(f["bytes"], 19, "the first upload stays");
    assert!(f["audio"]["duration_s"].as_f64().is_some(), "{f}");
}

#[tokio::test]
async fn tags_follow_files_and_are_set_many_at_once() {
    let p = panel(None);
    for name in ["bells/lunch.wav", "bells/end.wav", "voice/fire.wav"] {
        p.fake.upload_media(name, b"RIFF").unwrap();
    }
    let (s, v) = p
        .call(
            Method::PUT,
            "/media-tags",
            Some(json!({"names": ["bells/lunch.wav", "bells/end.wav"], "add": ["School", " bell  times "]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["bells/lunch.wav"], json!(["bell times", "School"]));
    // Same tag, other case: spelled as it already is, and not twice.
    let (_, v) = p
        .call(
            Method::PUT,
            "/media-tags",
            Some(json!({"names": ["voice/fire.wav", "bells/end.wav"], "add": ["school"]})),
        )
        .await;
    assert_eq!(v["voice/fire.wav"], json!(["School"]));
    assert_eq!(v["bells/end.wav"], json!(["bell times", "School"]));
    for bad in [
        json!({"names": [], "add": ["x"]}),
        json!({"names": ["a.wav"], "add": ["a,b"]}),
        json!({"names": ["../x"], "add": ["x"]}),
        json!({"names": ["a.wav"], "add": ["x".repeat(33)]}),
    ] {
        let (s, _) = p.call(Method::PUT, "/media-tags", Some(bad.clone())).await;
        assert!(s.is_client_error(), "{bad}");
    }

    let (_, lib) = p.call(Method::GET, "/library", None).await;
    assert_eq!(
        lib["tags"],
        json!([{"tag": "bell times", "files": 2}, {"tag": "School", "files": 3}])
    );

    // Moving the folder takes the tags along; deleting a file drops them.
    let (s, _) = p
        .call(
            Method::POST,
            "/media-move",
            Some(json!({"from": "bells", "to": "old/bells"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert_eq!(
        cfg["media_tags"]["old/bells/lunch.wav"],
        json!(["bell times", "School"])
    );
    assert!(cfg["media_tags"].get("bells/lunch.wav").is_none());
    let (s, _) = p
        .call(Method::DELETE, "/media/old/bells/end.wav", None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert!(
        cfg["media_tags"].get("old/bells/end.wav").is_none(),
        "{cfg}"
    );

    // Untagging the last tag drops the file.
    let (_, v) = p
        .call(
            Method::PUT,
            "/media-tags",
            Some(json!({"names": ["voice/fire.wav"], "remove": ["SCHOOL"]})),
        )
        .await;
    assert_eq!(v["voice/fire.wav"], json!([]));
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert!(cfg["media_tags"].get("voice/fire.wav").is_none());
}

// ---- playlists and parts ----

#[tokio::test]
async fn playlists_and_parts_play_together_in_one_run() {
    let p = panel(None);
    for name in ["en/msg.wav", "en/thanks.wav", "es/msg.wav"] {
        p.fake.upload_media(name, b"RIFF").unwrap();
    }
    for (name, group) in [
        ("Building A", "zone-1"),
        ("Building B", "239.1.1.2:5000"),
        ("Yard", "239.1.1.3:5000"),
    ] {
        let (s, v) = p
            .call(
                Method::POST,
                "/zones",
                Some(json!({"name": name, "targets": [{"group": group}]})),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let (s, v) = p
        .call(
            Method::POST,
            "/playlists",
            Some(json!({"name": "English", "gap_ms": 500,
                "items": [{"type": "sound", "media": "en/msg.wav"}, {"type": "sound", "media": "en/thanks.wav"}]})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["id"], "english");
    let (s, _) = p
        .call(Method::POST, "/playlists", Some(json!({"name": "Empty"})))
        .await;
    assert_eq!(s, StatusCode::CREATED);

    // English to A, Spanish to B, chime only to the yard: one run.
    let ann = json!({"name": "Closing", "audio": {"playlist": "english"}, "zones": ["building-a"],
        "parts": [{"zones": ["building-b"], "audio": {"media": "es/msg.wav"}},
                  {"zones": ["yard"], "audio": "chime_only"}]});
    let (s, v) = p
        .call(Method::POST, "/announcements", Some(ann.clone()))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, run) = p
        .call(Method::POST, "/announcements/closing/play", None)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    let parts: Vec<Value> = run["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["part"].clone())
        .collect();
    assert_eq!(parts, [Value::Null, json!(1), json!(2)]);
    let sources: Vec<Value> = p
        .fake
        .requests()
        .iter()
        .map(|r| r["source"].clone())
        .collect();
    assert!(
        sources.contains(
            &json!({"type": "files", "paths": ["en/msg.wav", "en/thanks.wav"], "gap_ms": 500})
        ),
        "{sources:?}"
    );
    assert!(sources.contains(&json!({"type": "file", "path": "es/msg.wav"})));
    assert!(sources.contains(&json!({"type": "chime"})));
    p.call(Method::DELETE, "/runs", None).await;

    // The trigger API narrows across parts: only Building B plays.
    let (s, run) = p
        .call(
            Method::POST,
            "/trigger/closing",
            Some(json!({"zones": ["Building B"]})),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    assert_eq!(run["targets"].as_array().unwrap().len(), 1);
    assert_eq!(run["targets"][0]["address"], "239.1.1.2:5000");
    p.call(Method::DELETE, "/runs", None).await;

    // In use: the playlist and its files can't go; renaming a file follows.
    let (s, v) = p.call(Method::DELETE, "/playlists/english", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Closing"));
    let (s, v) = p.call(Method::DELETE, "/media/en/thanks.wav", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("playlist English"),
        "{v}"
    );
    let (s, _) = p
        .call(
            Method::POST,
            "/media-move",
            Some(json!({"from": "en", "to": "english"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, pl) = p.call(Method::GET, "/playlists/english", None).await;
    assert_eq!(pl["items"][1]["media"], "english/thanks.wav");
    let (_, cfg) = p.call(Method::GET, "/config", None).await;
    assert_eq!(
        cfg["announcements"][0]["parts"][0]["audio"]["media"],
        "es/msg.wav"
    );

    // Mistakes.
    let bad = |change: Value| {
        let mut a = ann.clone();
        a.as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        a["name"] = json!("Other");
        a
    };
    for (what, body) in [
        (
            "an empty playlist",
            bad(json!({"audio": {"playlist": "empty"}})),
        ),
        (
            "no such playlist",
            bad(json!({"audio": {"playlist": "nope"}})),
        ),
        (
            "a zone twice",
            bad(json!({"parts": [{"zones": ["building-a"], "audio": "chime_only"}]})),
        ),
        (
            "a live part",
            bad(json!({"parts": [{"zones": ["yard"], "audio": "live"}]})),
        ),
        ("live with parts", bad(json!({"audio": "live"}))),
        (
            "a part with no zones",
            bad(json!({"parts": [{"zones": [], "audio": "chime_only"}]})),
        ),
    ] {
        let (s, v) = p.call(Method::POST, "/announcements", Some(body)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {v}");
    }
    let (s, _) = p
        .call(
            Method::PUT,
            "/playlists/english",
            Some(json!({"name": "English", "items": []})),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "emptying a playlist in use"
    );
}

#[tokio::test]
async fn admins_arrange_a_tab_for_everyone() {
    let p = panel(Some("s3cret"));
    let admin = Some("s3cret");
    let desk = p
        .token(admin, json!({"name": "Desk", "role": "operator"}))
        .await;
    let order = json!({"order": ["tokens", "defaults", "time-zone"]});
    let (s, v) = p
        .call_with(Method::PUT, "/layout/settings", Some(order.clone()), admin)
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"settings": ["tokens", "defaults", "time-zone"]}));
    // Everyone who can read the config sees it; only admins change it.
    let (_, cfg) = p.call_with(Method::GET, "/config", None, Some(&desk)).await;
    assert_eq!(cfg["layout"]["settings"][0], "tokens");
    let (s, _) = p
        .call_with(Method::PUT, "/layout/settings", Some(order), Some(&desk))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    for bad in [
        json!({"order": ["a", "a"]}),
        json!({"order": ["no spaces"]}),
        json!({"sections": []}),
    ] {
        let (s, _) = p
            .call_with(Method::PUT, "/layout/settings", Some(bad.clone()), admin)
            .await;
        assert!(s.is_client_error(), "{bad}");
    }
    let (s, v) = p
        .call_with(Method::DELETE, "/layout/settings", None, admin)
        .await;
    assert_eq!((s, v), (StatusCode::OK, json!({})));
    let (_, cfg) = p.call_with(Method::GET, "/config", None, admin).await;
    assert!(cfg.get("layout").is_none(), "{cfg}");
}

#[tokio::test]
async fn categories_and_tags_organize_announcements() {
    let p = panel(None);
    p.set_up().await;
    let (s, v) = p
        .call(
            Method::POST,
            "/categories",
            Some(json!({"name": "Daily bells"})),
        )
        .await;
    assert_eq!(
        (s, v["id"].clone()),
        (StatusCode::CREATED, json!("daily-bells"))
    );
    let (s, v) = p
        .call(
            Method::PUT,
            "/announcements/bell",
            Some(
                json!({"name": "Bell", "audio": "chime_only", "zones": ["hall"],
                "category": "daily-bells", "tags": ["Building A", "school"]}),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["tags"], json!(["Building A", "school"]));
    for bad in [
        json!({"category": "nope"}),
        json!({"tags": ["a,b"]}),
        json!({"tags": ["x", "X"]}),
    ] {
        let mut body = json!({"name": "Bell", "audio": "chime_only", "zones": ["hall"]});
        body.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        let (s, _) = p.call(Method::PUT, "/announcements/bell", Some(body)).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
    // A category with announcements in it stays until they move.
    let (s, v) = p
        .call(Method::DELETE, "/categories/daily-bells", None)
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    // The Panel's order: categories, then buttons, through the layout.
    let (s, _) = p
        .call(
            Method::PUT,
            "/layout/panel",
            Some(json!({"order": ["daily-bells"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = p
        .call(
            Method::PUT,
            "/layout/panel-buttons",
            Some(json!({"order": ["bell"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
}
