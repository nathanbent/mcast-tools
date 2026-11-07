//! Real-time text channels.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mcast_control::{JobId, JobInfo, TextError, TextLine, TextRequest};
use serde::Deserialize;

impl From<TextError> for ApiError {
    fn from(e: TextError) -> Self {
        let status = match e {
            TextError::NoJob => StatusCode::NOT_FOUND,
            TextError::Ended => StatusCode::CONFLICT,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        ApiError::new(status, e.to_string())
    }
}

/// Join a text channel (or listen to one).
pub(crate) async fn start_text(
    State(s): State<AppState>,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<JobInfo>)> {
    let req: TextRequest = s.named(body)?;
    let info = tokio::task::spawn_blocking(move || {
        let known = s.0.announcements.text_streams();
        s.0.jobs.start_text(req, &known)
    })
    .await??;
    Ok((StatusCode::CREATED, Json(info)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Say {
    text: String,
}

/// Send text on a channel; each line goes out as typed.
pub(crate) async fn send_text(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
    body: Result<Json<Say>, JsonRejection>,
) -> ApiResult<Json<Vec<TextLine>>> {
    let Json(say) = body?;
    Ok(Json(s.0.jobs.send_text(id, &say.text)?))
}

/// Recent lines on a channel, oldest first.
pub(crate) async fn text_history(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
) -> ApiResult<Json<Vec<TextLine>>> {
    Ok(Json(s.0.jobs.text_history(id)?))
}
