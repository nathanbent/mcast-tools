//! The Sounds tab's API: the mcast server's media files and folders, with
//! which the panel hides and which announcements use each. Renaming or
//! moving a file updates the announcements that use it.

use super::routes::{change, passed_on};
use super::{ApiError, ApiResult, AppState};
use crate::backend::Backend;
use crate::model::valid_media_name;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Largest upload the panel passes on; the mcast server has its own limit.
const MAX_UPLOAD: usize = 256 << 20;

/// Run a call to the mcast server off the async threads.
async fn on_server<T: Send + 'static>(
    s: &AppState,
    f: impl FnOnce(&dyn Backend) -> crate::backend::BackendResult<T> + Send + 'static,
) -> ApiResult<T> {
    let b: Arc<dyn Backend> = s.0.backend.clone();
    tokio::task::spawn_blocking(move || f(b.as_ref()))
        .await?
        .map_err(passed_on)
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim_matches('/');
    if !valid_media_name(name) {
        return Err(ApiError::bad(format!(
            "\"{name}\" isn't a media name: use file and folder names joined by '/', none starting with '.'"
        )));
    }
    Ok(name.to_string())
}

/// `{"files": [{"name", "bytes", "modified_ms", "hidden", "used_by", "audio", "audio_error", "tags"}],
///   "folders": [{"name", "hidden"}], "tags": [{"tag", "files"}]}`
pub(super) async fn library(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    let (files, folders): (Vec<crate::backend::MediaFile>, Vec<String>) =
        on_server(&s, |b| Ok((b.media_files()?, b.media_folders()?))).await?;
    let cfg = s.load().await?;
    let files: Vec<Value> = files
        .into_iter()
        .map(|f| {
            json!({
                "name": f.name,
                "bytes": f.bytes,
                "modified_ms": f.modified_ms,
                "hidden": cfg.is_hidden(&f.name),
                "used_by": cfg.media_users(&f.name),
                "audio": f.audio,
                "audio_error": f.audio_error,
                "tags": cfg.tags_of(&f.name),
            })
        })
        .collect();
    let folders: Vec<Value> = folders
        .into_iter()
        .map(|f| json!({ "name": f, "hidden": cfg.is_hidden(&f) }))
        .collect();
    // Every tag, with how many files have it, for choosing one.
    let mut counts: std::collections::BTreeMap<String, (String, usize)> = Default::default();
    for t in cfg.media_tags.values().flatten() {
        counts.entry(t.to_lowercase()).or_insert((t.clone(), 0)).1 += 1;
    }
    let tags: Vec<Value> = counts
        .into_values()
        .map(|(tag, files)| json!({ "tag": tag, "files": files }))
        .collect();
    Ok(Json(
        json!({ "files": files, "folders": folders, "tags": tags }),
    ))
}

/// Upload a sound (raw body), into its folders. The mcast server checks
/// it's audio it can play (`422` if not, leaving any old file); returns
/// `{"name", "bytes", "audio"}`.
pub(super) async fn upload(
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Body,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = check_name(&name)?;
    let data = axum::body::to_bytes(body, MAX_UPLOAD).await.map_err(|_| {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "uploads through the panel are limited to {} MB",
                MAX_UPLOAD >> 20
            ),
        )
    })?;
    let v = on_server(&s, move |b| b.upload_media(&name, &data)).await?;
    Ok((StatusCode::CREATED, Json(v)))
}

/// A file, to listen to.
pub(super) async fn download(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Response> {
    let name = check_name(&name)?;
    let (kind, data) = on_server(&s, move |b| b.download_media(&name)).await?;
    // Only sounds and pictures play or show; anything else downloads.
    let inline = kind.starts_with("audio/") || kind.starts_with("image/");
    let kind = if inline {
        kind
    } else {
        "application/octet-stream".into()
    };
    Ok((
        [
            (header::CONTENT_TYPE, kind),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".into(),
            ),
            (header::CACHE_CONTROL, "no-cache".into()),
        ],
        data,
    )
        .into_response())
}

fn in_use(what: &str, users: &[String]) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        format!("{what} is used by {}; change them first", users.join(", ")),
    )
}

/// Delete a file, unless something uses it.
pub(super) async fn delete(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let name = check_name(&name)?;
    let users = s.load().await?.media_users(&name);
    if !users.is_empty() {
        return Err(in_use(&format!("\"{name}\""), &users));
    }
    let n = name.clone();
    on_server(&s, move |b| b.delete_media(&n)).await?;
    forget(&s, name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Drop a deleted file or folder's hidden mark and tags, if it has any.
async fn forget(s: &AppState, name: String) -> ApiResult<()> {
    let cfg = s.load().await?;
    let mut after = cfg.clone();
    after.media_deleted(&name);
    if after != cfg {
        change(s, move |c| {
            c.media_deleted(&name);
            Ok(())
        })
        .await?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Folder {
    name: String,
}

pub(super) async fn create_folder(
    State(s): State<AppState>,
    body: Result<Json<Folder>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(f) = body?;
    let name = check_name(&f.name)?;
    on_server(&s, move |b| b.create_folder(&name)).await?;
    Ok(StatusCode::CREATED)
}

/// Delete an empty folder.
pub(super) async fn delete_folder(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let name = check_name(&name)?;
    let n = name.clone();
    on_server(&s, move |b| b.delete_folder(&n)).await?;
    forget(&s, name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Move {
    from: String,
    to: String,
}

/// `{"from", "to"}`: rename or move a file or folder on the mcast server,
/// then point the announcements (and defaults, and hidden list) that used
/// it at its new name.
pub(super) async fn move_media(
    State(s): State<AppState>,
    body: Result<Json<Move>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(m) = body?;
    let (from, to) = (check_name(&m.from)?, check_name(&m.to)?);
    // Under the config lock, so no edit slips in between the move and the
    // update; if the move fails, the config is left as it was.
    let (f, t) = (from.clone(), to.clone());
    let backend = s.0.backend.clone();
    let (folder, updated) = change(&s, move |c| {
        let folder = backend.move_media(&f, &t).map_err(passed_on)?;
        let users = c.media_users(&f);
        c.media_moved(&f, &t);
        Ok((folder, users))
    })
    .await?;
    Ok(Json(json!({
        "from": from, "to": to, "folder": folder, "updated": updated,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Hide {
    name: String,
    hidden: bool,
}

/// `{"name", "hidden"}`: hide a file or folder from the panel's lists, or
/// show it again.
pub(super) async fn hide(
    State(s): State<AppState>,
    body: Result<Json<Hide>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(h) = body?;
    let name = check_name(&h.name)?;
    change(&s, move |c| {
        c.hidden_media.retain(|x| *x != name);
        if h.hidden {
            c.hidden_media.push(name);
            c.hidden_media.sort();
        }
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tags {
    names: Vec<String>,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

/// `{"names", "add", "remove"}`: tag files, or untag them, many at once.
/// Returns each file's tags afterwards: `{name: [tags]}`.
pub(super) async fn tag(
    State(s): State<AppState>,
    body: Result<Json<Tags>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(t) = body?;
    if t.names.is_empty() {
        return Err(ApiError::bad("names: give at least one file"));
    }
    let names = t
        .names
        .iter()
        .map(|n| check_name(n))
        .collect::<ApiResult<Vec<_>>>()?;
    let after = change(&s, move |c| {
        c.retag(&names, &t.add, &t.remove)
            .map_err(|e| ApiError::bad(format!("{e:#}")))?;
        Ok(names
            .iter()
            .map(|n| (n.clone(), json!(c.tags_of(n))))
            .collect::<serde_json::Map<_, _>>())
    })
    .await?;
    Ok(Json(Value::Object(after)))
}
