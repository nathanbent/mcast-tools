//! The panel's web page: plain HTML, CSS and JavaScript modules in
//! `assets/` (no build step), embedded in the binary. A copy of how
//! mcast-web serves itself, so this crate needs nothing from it.

use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::path::PathBuf;
use std::sync::Arc;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

/// Routes for `/` and the asset files. With `dir`, files come from there
/// instead of the embedded copies.
pub(super) fn router(dir: Option<PathBuf>) -> Router {
    let dir = Arc::new(dir);
    let index = {
        let dir = dir.clone();
        move || async move { asset(&dir, "index.html").await }
    };
    Router::new().route("/", get(index)).route(
        "/{*path}",
        get(move |Path(path): Path<String>| async move { asset(&dir, &path).await }),
    )
}

/// Paths like "js/app.js": no parent, hidden or absolute parts.
fn valid(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|p| !p.is_empty() && !p.starts_with('.') && !p.contains('\\'))
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn asset(dir: &Option<PathBuf>, path: &str) -> Response {
    if !valid(path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body: Vec<u8> = match dir {
        Some(d) => match tokio::fs::read(d.join(path)).await {
            Ok(b) => b,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        },
        None => match embedded::ASSETS.iter().find(|(p, _)| *p == path) {
            Some((_, b)) => b.to_vec(),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let mut r = body.into_response();
    let h = r.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, k, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(h, header::CONTENT_TYPE, content_type(path));
    // Revalidate so an upgraded binary's files are picked up.
    set(h, header::CACHE_CONTROL, "no-cache");
    // This page can page: keep it out of frames and off other origins' scripts.
    set(
        h,
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
    );
    set(h, header::X_FRAME_OPTIONS, "DENY");
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(h, header::REFERRER_POLICY, "no-referrer");
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn get(path: &str) -> Response {
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        router(None).oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn serves_embedded_files() {
        let r = get("/").await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert!(r.headers().contains_key(header::CONTENT_SECURITY_POLICY));
        let r = get("/js/main.js").await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn refuses_paths_outside_the_assets() {
        for p in [
            "/../Cargo.toml",
            "/js/../../Cargo.toml",
            "/.hidden",
            "/nope.js",
            "/js//main.js",
        ] {
            assert_eq!(get(p).await.status(), StatusCode::NOT_FOUND, "{p}");
        }
    }

    #[test]
    fn every_module_import_resolves() {
        // Catch a renamed module before a browser does.
        let names: Vec<&str> = embedded::ASSETS.iter().map(|(p, _)| *p).collect();
        for (path, body) in embedded::ASSETS.iter().filter(|(p, _)| p.ends_with(".js")) {
            let text = std::str::from_utf8(body).unwrap();
            for line in text.lines().filter(|l| l.starts_with("import ")) {
                let spec = line.split(['"', '\'']).nth(1).unwrap();
                let target = spec.strip_prefix("./").expect("relative imports");
                let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
                let full = if dir.is_empty() {
                    target.to_string()
                } else {
                    format!("{dir}/{target}")
                };
                assert!(
                    names.contains(&full.as_str()),
                    "{path} imports missing {spec}"
                );
            }
        }
    }
}
