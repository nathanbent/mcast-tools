//! The panel's API against the fake mcast server.

use super::*;
use crate::fake::Fake;
use axum::body::Body;
use axum::http::{Method, Request};
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
        (StatusCode::OK, json!({"zones": [], "announcements": []}))
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
    let (s, _) = p
        .call(Method::GET, "/health?access_token=s3cret", None)
        .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn wont_listen_on_the_network_without_a_token() {
    let cfg = ServeConfig {
        listen: "0.0.0.0:0".parse().unwrap(),
        token: None,
        config: PathBuf::from("unused.json"),
        web: true,
        web_dir: None,
    };
    let e = serve(cfg, Arc::new(Fake::default()), async {})
        .await
        .unwrap_err();
    assert!(e.to_string().contains("without a token"), "{e}");
}
