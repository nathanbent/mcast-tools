//! `/backup` and `/restore` (admin): the config, optionally with the mcast
//! server's sounds, as a download; and putting one back (`crate::backup`).

use super::routes::passed_on;
use super::{ApiError, ApiResult, AppState, Event};
use crate::backup::{self, ApplyError, Backup, Plan};
use crate::store;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// Whether the query has `key=1` (or `true`, or just `key`).
fn flag(query: &Option<String>, key: &str) -> bool {
    query.as_deref().unwrap_or("").split('&').any(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, "1"));
        k == key && matches!(v, "1" | "true" | "yes")
    })
}

fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

/// The config as `intercom.json`; with `?sounds=1`, a `.tar.gz` with every
/// sound file on the mcast server too.
pub(super) async fn download(
    State(s): State<AppState>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    let cfg = s.load().await?;
    if !flag(&q, "sounds") {
        let body = serde_json::to_vec_pretty(&cfg).map_err(ApiError::internal)?;
        return Ok((
            [
                (header::CONTENT_TYPE, "application/json".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"intercom-{}.json\"", today()),
                ),
            ],
            body,
        )
            .into_response());
    }
    let b = s.0.backend.clone();
    let archive = tokio::task::spawn_blocking(move || {
        let files = b.media_files().map_err(passed_on)?;
        let total: u64 = files.iter().map(|f| f.bytes).sum();
        if total > backup::MAX_BYTES {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "the sounds come to {} MiB; a backup holds at most {} MiB",
                    total >> 20,
                    backup::MAX_BYTES >> 20
                ),
            ));
        }
        let mut sounds = Vec::with_capacity(files.len());
        for f in files {
            let (_, data) = b.download_media(&f.name).map_err(passed_on)?;
            sounds.push((f.name, data));
        }
        let mut out = Vec::new();
        backup::write_archive(
            &cfg,
            sounds.iter().map(|(n, d)| (n.as_str(), d.as_slice())),
            crate::now_ms(),
            &mut out,
        )
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok(out)
    })
    .await??;
    Ok((
        [
            (header::CONTENT_TYPE, "application/gzip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"intercom-{}.tar.gz\"", today()),
            ),
        ],
        archive,
    )
        .into_response())
}

/// Put a backup back (the raw body: a `.tar.gz` or an `intercom.json`).
/// With `?dry_run=1`, only say what would change: `{"plan", "applied":
/// false}`. Otherwise upload the sounds the mcast server lacks (or has at
/// another size), then replace the config, keeping the old one as
/// `intercom.json.bak`: `{"plan", "applied": true, "uploaded", "kept"}`.
pub(super) async fn restore(
    State(s): State<AppState>,
    RawQuery(q): RawQuery,
    body: Body,
) -> ApiResult<Json<Value>> {
    let dry_run = flag(&q, "dry_run");
    let bytes = axum::body::to_bytes(body, backup::MAX_BYTES as usize)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("a backup can be at most {} MiB", backup::MAX_BYTES >> 20),
            )
        })?;
    let b: Backup = tokio::task::spawn_blocking(move || backup::read(&bytes))
        .await?
        .map_err(|e| ApiError::bad(format!("{e:#}")))?;

    // One change at a time, as with any edit.
    let _held = s.0.config_lock.lock().await;
    let (path, backend) = (s.0.config.clone(), s.0.backend.clone());
    let need_server = !b.sounds.is_empty() && !dry_run;
    let (plan, server_error, uploaded, kept, history_days) =
        tokio::task::spawn_blocking(move || -> ApiResult<_> {
            let now = store::load(&path).map_err(ApiError::internal)?;
            let files = backend.media_files();
            let server_error = files.as_ref().err().map(|e| e.message.clone());
            let plan: Plan = backup::plan(&now, &b, files.as_deref().ok());
            if dry_run {
                return Ok((plan, server_error, Vec::new(), None, None));
            }
            if need_server {
                if let Err(e) = files {
                    return Err(passed_on(e));
                }
            }
            let done = backup::apply(&path, &b, &plan, backend.as_ref()).map_err(|e| {
                let status = match &e {
                    // The server's own 4xx (not audio, too big), else 502.
                    ApplyError::Upload { error, .. } => StatusCode::from_u16(error.status)
                        .ok()
                        .filter(|s| s.is_client_error())
                        .unwrap_or(StatusCode::BAD_GATEWAY),
                    ApplyError::Write(_) => StatusCode::INTERNAL_SERVER_ERROR,
                };
                ApiError::new(status, e.to_string())
            })?;
            let kept = done
                .kept
                .and_then(|k| k.file_name().map(|n| n.to_string_lossy().into_owned()));
            Ok((
                plan,
                server_error,
                done.uploaded,
                kept,
                Some(b.config.history_days()),
            ))
        })
        .await??;
    let applied = !dry_run;
    if applied {
        if let Some(days) = history_days {
            s.0.history.set_keep_days(days);
        }
        s.0.scheduler.poke();
        s.0.inputs.poke();
        let _ = s.0.events.send(Event::ConfigChanged);
    }
    Ok(Json(json!({
        "plan": plan,
        "applied": applied,
        "uploaded": uploaded,
        "kept": kept,
        // The mcast server couldn't list its sounds: what's missing isn't known.
        "server_error": server_error,
    })))
}
