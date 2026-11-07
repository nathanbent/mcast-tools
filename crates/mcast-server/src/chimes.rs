//! The built-in chimes: list them, hide or show them, and preview them.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use mcast_audio::chime::BuiltIn;
use mcast_control::chimes::{ChimeInfo, ChimeSettings};
use mcast_control::Event;
use serde::Deserialize;

impl AppState {
    fn load_chimes(&self) -> ApiResult<ChimeSettings> {
        ChimeSettings::load_from(&self.0.chimes).map_err(|e| ApiError::internal(format!("{e:#}")))
    }
}

fn built_in(name: &str) -> ApiResult<BuiltIn> {
    name.parse()
        .map_err(|e: String| ApiError::new(StatusCode::NOT_FOUND, e))
}

/// Every built-in chime, hidden ones included (marked `"hidden": true`).
pub(crate) async fn list_chimes(State(s): State<AppState>) -> ApiResult<Json<Vec<ChimeInfo>>> {
    Ok(Json(s.load_chimes()?.list()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutChime {
    hidden: bool,
}

/// Hide a chime from lists, or show it again.
pub(crate) async fn put_chime(
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Result<Json<PutChime>, JsonRejection>,
) -> ApiResult<Json<ChimeInfo>> {
    let b = built_in(&name)?;
    let Json(req) = body?;
    let _write = s.0.chimes_lock.lock().await;
    let mut settings = s.load_chimes()?;
    if settings.set_hidden(b, req.hidden) {
        settings
            .save_to(&s.0.chimes)
            .map_err(|e| ApiError::internal(format!("{e:#}")))?;
        let _ = s.0.events.send(Event::ChimesChanged);
    }
    let info = settings
        .list()
        .into_iter()
        .find(|c| c.name == b.name())
        .expect("every built-in is listed");
    Ok(Json(info))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutChimes {
    hidden: Vec<String>,
}

/// Set which chimes are hidden all at once: exactly those named.
pub(crate) async fn put_chimes(
    State(s): State<AppState>,
    body: Result<Json<PutChimes>, JsonRejection>,
) -> ApiResult<Json<Vec<ChimeInfo>>> {
    let Json(req) = body?;
    let hidden = req
        .hidden
        .iter()
        .map(|n| n.parse::<BuiltIn>().map_err(ApiError::bad))
        .collect::<ApiResult<Vec<_>>>()?;
    let _write = s.0.chimes_lock.lock().await;
    let mut settings = s.load_chimes()?;
    let before = settings.clone();
    for &b in BuiltIn::ALL {
        settings.set_hidden(b, hidden.contains(&b));
    }
    if settings != before {
        settings
            .save_to(&s.0.chimes)
            .map_err(|e| ApiError::internal(format!("{e:#}")))?;
        let _ = s.0.events.send(Event::ChimesChanged);
    }
    Ok(Json(settings.list()))
}

/// The chime as a WAV file, to listen to before choosing it.
pub(crate) async fn chime_preview(Path(name): Path<String>) -> ApiResult<impl IntoResponse> {
    let b = built_in(&name)?;
    let wav = tokio::task::spawn_blocking(move || mcast_audio::chime::preview_wav(b))
        .await?
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok((
        [
            (header::CONTENT_TYPE, "audio/wav"),
            // Built-ins only change with a new version of mcast.
            (header::CACHE_CONTROL, "max-age=3600"),
        ],
        wav,
    ))
}
