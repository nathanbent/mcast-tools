//! Uploaded audio and pictures, and files received over multicast, in
//! folders: a media name is a path such as `bells/lunch.wav`.

use crate::{ApiError, ApiResult, AppState};
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use futures_util::StreamExt;
use mcast_control::Event;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

/// Deepest folder nesting accepted.
const MAX_DEPTH: usize = 8;

/// One part of a media path: no hidden files, no separators.
fn valid_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 255
        && !part.starts_with('.')
        && !part
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

/// A media path: parts joined by `/`, none hidden, `.` or `..`.
pub(crate) fn valid_name(name: &str) -> bool {
    name.len() <= 1024 && {
        let parts: Vec<&str> = name.split('/').collect();
        parts.len() <= MAX_DEPTH && parts.iter().all(|p| valid_part(p))
    }
}

fn bad_name(name: &str) -> ApiError {
    ApiError::bad(format!(
        "\"{name}\" isn't a media name: use folder/file names joined by '/', none starting with '.'"
    ))
}

fn not_found(what: &str, name: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no {what} \"{name}\""))
}

impl AppState {
    fn media_changed(&self) {
        let _ = self.0.events.send(Event::MediaChanged);
    }
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

/// Every file and folder under the media directory, by path; hidden
/// entries (and anything deeper than [`MAX_DEPTH`]) are left out.
async fn walk(root: &std::path::Path) -> ApiResult<(Vec<MediaFile>, Vec<String>)> {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut todo = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = todo.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(_) if depth > 0 => continue,
            Err(e) => return Err(ApiError::internal(e)),
        };
        while let Some(e) = entries.next_entry().await.map_err(ApiError::internal)? {
            let Ok(part) = e.file_name().into_string() else {
                continue;
            };
            if !valid_part(&part) {
                continue;
            }
            // Not following links: they could lead out of the media directory.
            let Ok(meta) = tokio::fs::symlink_metadata(e.path()).await else {
                continue;
            };
            let name = format!("{prefix}{part}");
            if meta.is_dir() && depth + 1 < MAX_DEPTH {
                folders.push(name.clone());
                todo.push((e.path(), format!("{name}/"), depth + 1));
            } else if meta.is_file() {
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
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    folders.sort();
    Ok((files, folders))
}

/// Every file, in every folder: `name` is its path.
pub(crate) async fn list_media(State(s): State<AppState>) -> ApiResult<Json<Vec<MediaFile>>> {
    Ok(Json(walk(&s.0.media_dir).await?.0))
}

/// Every folder, empty ones included, by path.
pub(crate) async fn list_folders(State(s): State<AppState>) -> ApiResult<Json<Vec<String>>> {
    Ok(Json(walk(&s.0.media_dir).await?.1))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewFolder {
    name: String,
}

/// `{"name": "bells/old"}`: make a folder (and any above it).
pub(crate) async fn create_folder(
    State(s): State<AppState>,
    body: Result<Json<NewFolder>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let Json(req) = body?;
    let name = req.name.trim_matches('/').to_string();
    if !valid_name(&name) {
        return Err(bad_name(&name));
    }
    let path = s.0.media_dir.join(&name);
    if path.is_file() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("\"{name}\" is a file"),
        ));
    }
    tokio::fs::create_dir_all(&path)
        .await
        .map_err(ApiError::internal)?;
    s.media_changed();
    Ok((StatusCode::CREATED, Json(json!({ "name": name }))))
}

/// Delete a folder; only an empty one, so nothing goes by accident.
pub(crate) async fn delete_folder(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    if !valid_name(&name) {
        return Err(bad_name(&name));
    }
    let path = s.0.media_dir.join(&name);
    if !tokio::fs::symlink_metadata(&path)
        .await
        .is_ok_and(|m| m.is_dir())
    {
        return Err(not_found("folder", &name));
    }
    match tokio::fs::remove_dir(&path).await {
        Ok(()) => {
            s.media_changed();
            Ok(StatusCode::NO_CONTENT)
        }
        Err(_) => Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("folder \"{name}\" isn't empty"),
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Move {
    from: String,
    to: String,
}

/// `{"from", "to"}`: rename or move a file or a folder (with what's in it).
/// Never replaces anything; folders above `to` are made as needed.
pub(crate) async fn move_media(
    State(s): State<AppState>,
    body: Result<Json<Move>, JsonRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let Json(m) = body?;
    let (from, to) = (m.from.trim_matches('/'), m.to.trim_matches('/'));
    for n in [from, to] {
        if !valid_name(n) {
            return Err(bad_name(n));
        }
    }
    if to == from || to.starts_with(&format!("{from}/")) {
        return Err(ApiError::bad(format!("can't move \"{from}\" into itself")));
    }
    let (src, dest) = (s.0.media_dir.join(from), s.0.media_dir.join(to));
    let Ok(meta) = tokio::fs::symlink_metadata(&src).await else {
        return Err(not_found("file or folder", from));
    };
    if !(meta.is_file() || meta.is_dir()) {
        return Err(not_found("file or folder", from));
    }
    if tokio::fs::symlink_metadata(&dest).await.is_ok() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("\"{to}\" already exists"),
        ));
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(ApiError::internal)?;
    }
    tokio::fs::rename(&src, &dest)
        .await
        .map_err(ApiError::internal)?;
    s.media_changed();
    Ok(Json(json!({
        "from": from,
        "to": to,
        "folder": meta.is_dir(),
    })))
}

pub(crate) async fn upload_media(
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Body,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if !valid_name(&name) {
        return Err(bad_name(&name));
    }
    if s.0.media_dir.join(&name).is_dir() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("\"{name}\" is a folder"),
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
    let file_name = name.rsplit('/').next().unwrap_or(&name);
    let tmp =
        s.0.media_dir
            .join(format!(".upload-{}-{file_name}", mcast_core::random_u32()));
    let result = async {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(ApiError::internal)?;
        }
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
        Ok(bytes) => {
            s.media_changed();
            Ok((
                StatusCode::CREATED,
                Json(json!({ "name": name, "bytes": bytes })),
            ))
        }
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
        return Err(bad_name(&name));
    }
    let path = s.0.media_dir.join(&name);
    if !tokio::fs::symlink_metadata(&path)
        .await
        .is_ok_and(|m| m.is_file())
    {
        return Err(not_found("media file", &name));
    }
    tokio::fs::remove_file(path)
        .await
        .map_err(ApiError::internal)?;
    s.media_changed();
    Ok(StatusCode::NO_CONTENT)
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
        return Err(bad_name(&name));
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
                .rsplit('/')
                .next()
                .unwrap_or(&name)
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
