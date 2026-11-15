//! Real-time text channels.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mcast_control::{JobId, JobInfo, PageRequest, TextError, TextLine, TextRequest};
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
    /// Send it as a page: lines that go up together, with a name and a
    /// priority, on displays that know pages.
    #[serde(default)]
    page: Option<PageRequest>,
}

/// Send text on a channel, a line per line; with `page`, as one page.
pub(crate) async fn send_text(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
    body: Result<Json<Say>, JsonRejection>,
) -> ApiResult<Json<Vec<TextLine>>> {
    let Json(say) = body?;
    Ok(Json(match &say.page {
        Some(page) => s.0.jobs.send_page(id, &say.text, page)?,
        None => s.0.jobs.send_text(id, &say.text)?,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Clear {
    /// The page's id.
    page: String,
    /// At once, rather than after the page's hold.
    #[serde(default)]
    now: bool,
}

/// Take a page down on the displays that put it up.
pub(crate) async fn clear_page(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
    body: Result<Json<Clear>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(clear) = body?;
    s.0.jobs.clear_page(id, &clear.page, clear.now)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Recent lines on a channel, oldest first.
pub(crate) async fn text_history(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
) -> ApiResult<Json<Vec<TextLine>>> {
    Ok(Json(s.0.jobs.text_history(id)?))
}
