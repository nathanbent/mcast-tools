//! Uploaded audio and pictures, and files received over multicast.

use crate::{ApiError, ApiResult, AppState};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

/// A plain file name: no directories, no hidden files.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

impl AppState {
    /// Requests name media files; turn the name into a path inside the media directory.
    pub(crate) fn media_path(&self, name: &std::path::Path) -> ApiResult<PathBuf> {
        let n = name.to_str().filter(|n| valid_name(n)).ok_or_else(|| {
            ApiError::bad(format!(
                "\"{}\" isn't a media file name (upload files with PUT /api/v1/media/NAME)",
                name.display()
            ))
        })?;
        let path = self.0.media_dir.join(n);
        if !path.is_file() {
            return Err(ApiError::bad(format!("no media file \"{n}\"")));
        }
        Ok(path)
    }
}

#[derive(Serialize)]
pub(crate) struct MediaFile {
    name: String,
    bytes: u64,
    modified_ms: Option<u64>,
}

pub(crate) async fn list_media(State(s): State<AppState>) -> ApiResult<Json<Vec<MediaFile>>> {
    let mut dir = tokio::fs::read_dir(&s.0.media_dir)
        .await
        .map_err(ApiError::internal)?;
    let mut files = Vec::new();
    while let Some(e) = dir.next_entry().await.map_err(ApiError::internal)? {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let Ok(meta) = e.metadata().await else {
            continue;
        };
        if !valid_name(&name) || !meta.is_file() {
            continue;
        }
        files.push(MediaFile {
            name,
            bytes: meta.len(),
            modified_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64),
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(files))
}

pub(crate) async fn upload_media(
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Body,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if !valid_name(&name) {
        return Err(ApiError::bad(
            "media names are plain file names, not starting with '.'",
        ));
    }
    let max = s.0.max_upload;
    let too_large = || {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("uploads are limited to {} MB", max / (1 << 20)),
        )
    };
    let dest = s.0.media_dir.join(&name);
    // Write beside it and rename, so a job reading the old file is undisturbed.
    let tmp =
        s.0.media_dir
            .join(format!(".upload-{}-{name}", mcast_core::random_u32()));
    let result = async {
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(ApiError::internal)?;
        let mut stream = body.into_data_stream();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                ApiError::new(StatusCode::BAD_REQUEST, format!("upload failed: {e}"))
            })?;
            total += chunk.len() as u64;
            if total > max {
                return Err(too_large());
            }
            file.write_all(&chunk).await.map_err(ApiError::internal)?;
        }
        file.flush().await.map_err(ApiError::internal)?;
        file.sync_all().await.map_err(ApiError::internal)?;
        tokio::fs::rename(&tmp, &dest)
            .await
            .map_err(ApiError::internal)?;
        Ok::<_, ApiError>(total)
    }
    .await;
    match result {
        Ok(bytes) => Ok((
            StatusCode::CREATED,
            Json(json!({ "name": name, "bytes": bytes })),
        )),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

pub(crate) async fn delete_media(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    if !valid_name(&name) {
        return Err(ApiError::bad("not a media file name"));
    }
    match tokio::fs::remove_file(s.0.media_dir.join(&name)).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no media file \"{name}\""),
        )),
        Err(e) => Err(ApiError::internal(e)),
    }
}

/// Types a browser may show inline. Anything else (HTML, SVG, scripts,
/// text...) is downloaded instead: received files come from the network.
fn inline_type(name: &str) -> Option<&'static str> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        _ => return None,
    })
}

/// Download a media file (uploaded, or received with a file receiver).
pub(crate) async fn download_media(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<axum::response::Response> {
    use axum::http::{header, HeaderValue};
    use axum::response::IntoResponse;
    if !valid_name(&name) {
        return Err(ApiError::bad("not a media file name"));
    }
    let data = match tokio::fs::read(s.0.media_dir.join(&name)).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                format!("no media file \"{name}\""),
            ))
        }
        Err(e) => return Err(ApiError::internal(e)),
    };
    let mut r = data.into_response();
    let h = r.headers_mut();
    match inline_type(&name) {
        Some(t) => {
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(t));
        }
        None => {
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            // Plain ASCII fallback name; the client has the real one.
            let ascii: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || ".-_ ".contains(c) {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{ascii}\"")) {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
        }
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(r)
}
