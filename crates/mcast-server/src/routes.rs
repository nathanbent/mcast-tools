//! Status, jobs, and starting audio senders and receivers.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mcast_control::request::{ChimeRequest, FileItem, SinkRequest, SourceRequest};
use mcast_control::{JobId, RecvRequest, SendRequest};
use serde::Serialize;
use serde_json::json;
use std::time::Duration;

/// How long `DELETE /jobs/{id}` waits for the job to wind down.
const STOP_WAIT: Duration = Duration::from_secs(3);

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Serialize)]
struct Device {
    name: String,
    kind: &'static str,
    default: bool,
}

pub(crate) async fn devices() -> ApiResult<Json<serde_json::Value>> {
    let (host, list) = tokio::task::spawn_blocking(mcast_audio::device::devices)
        .await?
        .map_err(ApiError::internal)?;
    let devices: Vec<Device> = list
        .into_iter()
        .map(|d| Device {
            name: d.name,
            kind: if d.input { "input" } else { "output" },
            default: d.default,
        })
        .collect();
    Ok(Json(json!({ "host": host, "devices": devices })))
}

pub(crate) async fn streams(State(s): State<AppState>) -> Json<Vec<mcast_control::Announced>> {
    Json(s.0.announcements.list())
}

pub(crate) async fn monitor(
    State(s): State<AppState>,
) -> ApiResult<Json<mcast_control::MonitorSnapshot>> {
    match &s.0.watch {
        Some(w) => Ok(Json(w.snapshot())),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "the monitor is off (started with --no-monitor)",
        )),
    }
}

pub(crate) async fn list_jobs(State(s): State<AppState>) -> Json<Vec<mcast_control::JobInfo>> {
    Json(s.0.jobs.list())
}

pub(crate) fn no_job(id: JobId) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no job {id}"))
}

pub(crate) async fn get_job(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
) -> ApiResult<Json<mcast_control::JobInfo>> {
    s.0.jobs.get(id).map(Json).ok_or_else(|| no_job(id))
}

pub(crate) async fn stop_job(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
) -> ApiResult<Json<mcast_control::JobInfo>> {
    tokio::task::spawn_blocking(move || s.0.jobs.stop(id, STOP_WAIT))
        .await?
        .map(Json)
        .ok_or_else(|| no_job(id))
}

pub(crate) async fn start_sender(
    State(s): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<mcast_control::JobInfo>)> {
    let mut req: SendRequest = s.named(body)?;
    let shown = req.clone();
    if req.render.is_some() {
        return Err(ApiError::bad(
            "render is only available from the command line",
        ));
    }
    let mut speak = Vec::new();
    match &mut req.source {
        SourceRequest::File { path, .. } => *path = s.media_path(path)?,
        SourceRequest::Files { paths, .. } => {
            for (i, item) in paths.iter_mut().enumerate() {
                match item {
                    FileItem::Path(path) => *path = s.media_path(path)?,
                    FileItem::Say { say, voice } => {
                        if s.0.tts.is_none() {
                            return Err(ApiError::bad(
                                "text to speech isn't available: install Piper and a voice, or eSpeak NG",
                            ));
                        }
                        speak.push((i, say.clone(), voice.clone()));
                    }
                    FileItem::Silence { .. } => {}
                }
            }
        }
        _ => {}
    }
    if !speak.is_empty() {
        let tts = s.0.tts.clone().expect("checked above");
        let spoken = tokio::task::spawn_blocking(move || {
            speak
                .into_iter()
                .map(|(i, text, voice)| Ok((i, tts.speak(&text, voice.as_deref())?)))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .await?
        .map_err(|e| ApiError::bad(format!("speaking: {e:#}")))?;
        if let SourceRequest::Files { paths, .. } = &mut req.source {
            for (i, file) in spoken {
                paths[i] = FileItem::Path(file);
            }
        }
    }
    for chime in [&mut req.chime, &mut req.end_chime] {
        if let Some(ChimeRequest::File(path)) = chime {
            *path = s.media_path(path)?;
        }
    }
    let info = tokio::task::spawn_blocking(move || s.0.jobs.start_sender_as(req, shown)).await??;
    Ok((StatusCode::CREATED, Json(info)))
}

/// The text-to-speech engine, and the voices `{"say": ..., "voice": ...}` can name.
pub(crate) async fn voices(State(s): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let Some(tts) = s.0.tts.clone() else {
        return Ok(Json(
            json!({ "engine": null, "voices": [], "default_voice": null }),
        ));
    };
    let info = tokio::task::spawn_blocking(move || tts.describe()).await?;
    Ok(Json(serde_json::to_value(info).unwrap_or_default()))
}

pub(crate) async fn start_receiver(
    State(s): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<mcast_control::JobInfo>)> {
    let req: RecvRequest = s.named(body)?;
    if matches!(req.sink, SinkRequest::Wav { .. }) {
        return Err(ApiError::bad(
            "recording to WAV is only available from the command line",
        ));
    }
    let info = tokio::task::spawn_blocking(move || {
        let known = s.0.announcements.streams();
        s.0.jobs.start_receiver(req, &known)
    })
    .await??;
    Ok((StatusCode::CREATED, Json(info)))
}
