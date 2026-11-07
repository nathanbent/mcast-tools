//! Streaming audio into a sender with a live source.

use crate::{ApiError, ApiResult, AppState};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use futures_util::StreamExt;
use mcast_control::{JobId, LiveError};
use serde_json::{json, Value};

impl From<LiveError> for ApiError {
    fn from(e: LiveError) -> Self {
        let status = match e {
            LiveError::NoJob => StatusCode::NOT_FOUND,
            LiveError::NotLive => StatusCode::UNPROCESSABLE_ENTITY,
            LiveError::Ended => StatusCode::CONFLICT,
        };
        ApiError::new(status, e.to_string())
    }
}

/// `POST /jobs/{id}/live`: the request body, as it arrives, is the page's
/// audio (16-bit little-endian PCM at the source's rate and channels). When
/// the body ends, so does the page, with its end chime; if the job is
/// stopped first, the reply comes then.
pub(crate) async fn stream_live(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
    body: Body,
) -> ApiResult<Json<Value>> {
    let feed = s.0.jobs.live_feed(id)?;
    let mut stream = body.into_data_stream();
    let mut bytes = 0u64;
    let result = loop {
        if feed.is_closed() {
            break Err(ApiError::from(LiveError::Ended));
        }
        // Look up now and then, so a stop is noticed while nothing arrives.
        let next = tokio::time::timeout(std::time::Duration::from_millis(500), stream.next());
        match next.await {
            Err(_) => continue,
            Ok(Some(Ok(chunk))) => {
                bytes += chunk.len() as u64;
                feed.push_s16le(&chunk);
            }
            // The caller went away: the page is over all the same.
            Ok(Some(Err(_)) | None) => break Ok(()),
        }
    };
    feed.close();
    result?;
    Ok(Json(json!({ "bytes": bytes })))
}
