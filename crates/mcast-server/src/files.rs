//! Sending media files over multicast, and receiving them into it.

use crate::{ApiError, ApiResult, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use mcast_control::{FileRecvRequest, FileSendRequest, JobInfo};

/// Send uploaded media files; `files` are media names.
pub(crate) async fn start_file_sender(
    State(s): State<AppState>,
    body: Result<Json<FileSendRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<JobInfo>)> {
    let Json(mut req) = body?;
    let shown = req.clone();
    for f in &mut req.files {
        *f = s.media_path(f)?;
    }
    let info =
        tokio::task::spawn_blocking(move || s.0.jobs.start_file_sender_as(req, shown)).await??;
    Ok((StatusCode::CREATED, Json(info)))
}

/// Receive files into the media directory, where they can be listed,
/// downloaded, and paged or sent on.
pub(crate) async fn start_file_receiver(
    State(s): State<AppState>,
    body: Result<Json<FileRecvRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<JobInfo>)> {
    let Json(mut req) = body?;
    // No bigger than an upload could be.
    let cap = (s.0.max_upload / (1 << 20)).max(1);
    match req.max_size_mb {
        Some(n) if n > cap => {
            return Err(ApiError::bad(format!(
                "max_size_mb can be at most {cap} (the server's --max-upload-mb)"
            )))
        }
        Some(_) => {}
        None => req.max_size_mb = Some(FileRecvRequest::DEFAULT_MAX_SIZE_MB.min(cap)),
    }
    req.out_dir = s.0.media_dir.clone();
    let info = tokio::task::spawn_blocking(move || {
        let known = s.0.announcements.file_streams();
        s.0.jobs.start_file_receiver(req, &known)
    })
    .await??;
    Ok((StatusCode::CREATED, Json(info)))
}
