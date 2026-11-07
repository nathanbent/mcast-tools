//! HTTP/JSON API over [`mcast_control`]: start and stop pages and
//! receivers, list announced streams, read the monitor, manage uploaded
//! audio, and follow everything live as server-sent events.
//!
//! All routes are under `/api/v1`. See `docs/api.md` for the reference.

use anyhow::{bail, Context, Result};
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use mcast_control::probes::{CheckIn, CheckInReply, IntervalReport, ProbeError};
use mcast_control::request::{ChimeRequest, SinkRequest, SourceRequest};
use mcast_control::{
    Announcements, Event, EventSink, JobId, Jobs, ProbeConfig, ProbeInfo, Probes, RecvRequest,
    SendRequest, StartError, Watch,
};
use mcast_core::Interface;
use mcast_monitor::{MonitorConfig, Target};
use serde::Serialize;
use serde_json::json;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, watch};

/// How long `DELETE /jobs/{id}` waits for the job to wind down.
const STOP_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    /// Bearer token clients must send. Required unless listening on loopback.
    pub token: Option<String>,
    /// Where uploaded audio lives; requests name files relative to it.
    pub media_dir: PathBuf,
    /// Default interface for jobs, discovery and monitoring.
    pub interface: Interface,
    /// SAP groups to listen on.
    pub sap_groups: Vec<SocketAddr>,
    /// Extra groups for the monitor to watch besides announced streams;
    /// `None` turns the monitor off.
    pub monitor: Option<Vec<SocketAddr>>,
    /// Largest upload accepted, in bytes.
    pub max_upload: u64,
    /// A second token that only lets probes check in (so a token copied
    /// onto many Pis can't be used to page). Probes may use `token` too.
    pub probe_token: Option<String>,
    /// Where to keep the known probes and their configurations; `None`
    /// keeps them in memory only.
    pub probe_state: Option<PathBuf>,
}

struct Inner {
    jobs: Jobs,
    probes: Probes,
    announcements: Announcements,
    watch: Option<Watch>,
    events: broadcast::Sender<Event>,
    token: Option<String>,
    probe_token: Option<String>,
    media_dir: PathBuf,
    max_upload: u64,
    closing: watch::Receiver<bool>,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    /// Start the background services. `closing` ends event streams when it
    /// becomes true, so graceful shutdown isn't held up by open clients.
    pub fn start(cfg: &ServerConfig, closing: watch::Receiver<bool>) -> Result<Self> {
        std::fs::create_dir_all(&cfg.media_dir)
            .with_context(|| format!("creating media directory {}", cfg.media_dir.display()))?;
        let media_dir = cfg.media_dir.canonicalize()?;
        let (events, _) = broadcast::channel(1024);
        let sink: EventSink = {
            let tx = events.clone();
            Arc::new(move |e| {
                // No subscribers is fine.
                let _ = tx.send(e);
            })
        };
        let announcements = Announcements::start(&cfg.sap_groups, cfg.interface, sink.clone())
            .context("listening for SAP announcements")?;
        let watch = match &cfg.monitor {
            Some(groups) => Some(
                Watch::start(
                    MonitorConfig {
                        targets: groups.iter().map(|&g| Target::new(g)).collect(),
                        sap: Some(cfg.sap_groups.clone()),
                        interface: cfg.interface,
                        default_spec: None,
                    },
                    Duration::from_secs(1),
                    sink.clone(),
                )
                .context("starting the monitor")?,
            ),
            None => None,
        };
        let jobs = Jobs::new(cfg.interface, sink.clone());
        let probes = Probes::new(sink, jobs.mirror(), cfg.probe_state.clone())?;
        Ok(Self(Arc::new(Inner {
            jobs,
            probes,
            announcements,
            watch,
            events,
            token: cfg.token.clone().filter(|t| !t.is_empty()),
            probe_token: cfg.probe_token.clone().filter(|t| !t.is_empty()),
            media_dir,
            max_upload: cfg.max_upload,
            closing,
        })))
    }

    /// Stop every job, withdrawing their SAP announcements.
    pub fn shutdown(&self) {
        self.0.jobs.shutdown();
    }
}

/// Serve the API, plus `extra` routes outside `/api/v1` (such as a web
/// front end), until `shutdown` resolves; then stop every job.
pub async fn serve(
    cfg: ServerConfig,
    extra: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    if !cfg.listen.ip().is_loopback() && cfg.token.as_deref().map_or(true, str::is_empty) {
        bail!(
            "refusing to listen on {} without a token: anyone who can reach it could page; \
             set --token / MCAST_TOKEN, or listen on 127.0.0.1",
            cfg.listen
        );
    }
    let (close_tx, close_rx) = watch::channel(false);
    let state = {
        let cfg = cfg.clone();
        tokio::task::spawn_blocking(move || AppState::start(&cfg, close_rx)).await??
    };
    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("listening on {}", cfg.listen))?;
    eprintln!(
        "mcast on http://{} (API under /api/v1, media in {}){}",
        listener.local_addr()?,
        state.0.media_dir.display(),
        if state.0.token.is_some() {
            ", token required"
        } else {
            ""
        }
    );
    let app = router(state.clone()).merge(extra);
    {
        // Notice probes that stop checking in.
        let state = state.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(1));
            loop {
                t.tick().await;
                state.0.probes.tick();
            }
        });
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = close_tx.send(true);
        })
        .await?;
    eprintln!("stopping jobs...");
    tokio::task::spawn_blocking(move || state.shutdown()).await?;
    Ok(())
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/devices", get(devices))
        .route("/streams", get(streams))
        .route("/monitor", get(monitor))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(get_job).delete(stop_job))
        .route("/senders", post(start_sender))
        .route("/receivers", post(start_receiver))
        .route("/media", get(list_media))
        .route("/media/{name}", put(upload_media).delete(delete_media))
        .route("/events", get(events))
        .route("/probes", get(list_probes))
        .route("/probes/{id}", get(get_probe).delete(remove_probe))
        .route("/probes/{id}/checkin", post(probe_check_in))
        .route("/probes/{id}/reports", get(probe_reports))
        .route(
            "/probes/{id}/config",
            put(set_probe_config).delete(reset_probe_config),
        )
        .route(
            "/probe-defaults",
            get(probe_defaults).put(set_probe_defaults),
        )
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "no such route") })
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state);
    // No fallback outside /api/v1, so other routers can be merged in.
    Router::new().nest("/api/v1", api)
}

// ---- errors ----

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    job: Option<JobId>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            job: None,
        }
    }

    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message });
        if let Some(job) = self.job {
            body["job"] = job.into();
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<StartError> for ApiError {
    fn from(e: StartError) -> Self {
        match e {
            StartError::Conflict { job, .. } => ApiError {
                status: StatusCode::CONFLICT,
                message: e.to_string(),
                job: Some(job),
            },
            StartError::Invalid(m) => ApiError::bad(m),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        ApiError::new(e.status(), e.body_text())
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        ApiError::internal(e)
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ---- auth ----

/// Compare without leaking how much of the token matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The one route the probe token opens: `POST /probes/{id}/checkin`.
fn is_check_in(req: &Request) -> bool {
    // The path is relative to /api/v1 here, but allow for it being whole.
    let parts: Vec<&str> = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .collect();
    req.method() == axum::http::Method::POST
        && matches!(parts.as_slice(), [.., "probes", _, "checkin"])
}

async fn auth(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if s.0.token.is_none() {
        return next.run(req).await;
    }
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // Browsers' EventSource can't set headers, so the token may come as a query parameter.
    let query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("access_token=")));
    let given = bearer.or(query);
    let matches = |t: &Option<String>| {
        t.as_ref()
            .zip(given)
            .is_some_and(|(want, got)| same(got.as_bytes(), want.as_bytes()))
    };
    let allowed = matches(&s.0.token) || (is_check_in(&req) && matches(&s.0.probe_token));
    if allowed {
        next.run(req).await
    } else {
        let mut r =
            ApiError::new(StatusCode::UNAUTHORIZED, "missing or wrong token").into_response();
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            "Bearer".parse().expect("valid header"),
        );
        r
    }
}

// ---- handlers ----

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Serialize)]
struct Device {
    name: String,
    kind: &'static str,
    default: bool,
}

async fn devices() -> ApiResult<Json<serde_json::Value>> {
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

async fn streams(State(s): State<AppState>) -> Json<Vec<mcast_control::Announced>> {
    Json(s.0.announcements.list())
}

async fn monitor(State(s): State<AppState>) -> ApiResult<Json<mcast_control::MonitorSnapshot>> {
    match &s.0.watch {
        Some(w) => Ok(Json(w.snapshot())),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "the monitor is off (started with --no-monitor)",
        )),
    }
}

async fn list_jobs(State(s): State<AppState>) -> Json<Vec<mcast_control::JobInfo>> {
    Json(s.0.jobs.list())
}

fn no_job(id: JobId) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no job {id}"))
}

async fn get_job(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
) -> ApiResult<Json<mcast_control::JobInfo>> {
    s.0.jobs.get(id).map(Json).ok_or_else(|| no_job(id))
}

async fn stop_job(
    State(s): State<AppState>,
    Path(id): Path<JobId>,
) -> ApiResult<Json<mcast_control::JobInfo>> {
    tokio::task::spawn_blocking(move || s.0.jobs.stop(id, STOP_WAIT))
        .await?
        .map(Json)
        .ok_or_else(|| no_job(id))
}

async fn start_sender(
    State(s): State<AppState>,
    body: Result<Json<SendRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<mcast_control::JobInfo>)> {
    let Json(mut req) = body?;
    let shown = req.clone();
    if req.render.is_some() {
        return Err(ApiError::bad(
            "render is only available from the command line",
        ));
    }
    if let SourceRequest::File { path, .. } = &mut req.source {
        *path = s.media_path(path)?;
    }
    if let Some(ChimeRequest::File(path)) = &mut req.chime {
        *path = s.media_path(path)?;
    }
    let info = tokio::task::spawn_blocking(move || s.0.jobs.start_sender_as(req, shown)).await??;
    Ok((StatusCode::CREATED, Json(info)))
}

async fn start_receiver(
    State(s): State<AppState>,
    body: Result<Json<RecvRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<mcast_control::JobInfo>)> {
    let Json(req) = body?;
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

// ---- probes ----

impl From<ProbeError> for ApiError {
    fn from(e: ProbeError) -> Self {
        match e {
            ProbeError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "no such probe"),
            ProbeError::Invalid(m) => ApiError::bad(m),
        }
    }
}

fn no_probe(id: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no probe \"{id}\""))
}

async fn list_probes(State(s): State<AppState>) -> Json<Vec<ProbeInfo>> {
    Json(s.0.probes.list())
}

async fn get_probe(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProbeInfo>> {
    s.0.probes.get(&id).map(Json).ok_or_else(|| no_probe(&id))
}

async fn remove_probe(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    s.0.probes.remove(&id).map_err(|_| no_probe(&id))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_check_in(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CheckIn>, JsonRejection>,
) -> ApiResult<Json<CheckInReply>> {
    let Json(c) = body?;
    Ok(Json(s.0.probes.check_in(&id, c)?))
}

#[derive(serde::Deserialize)]
struct ReportsQuery {
    /// Only reports that ended after this time (ms since the epoch).
    after_ms: Option<u64>,
}

async fn probe_reports(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ReportsQuery>,
) -> ApiResult<Json<Vec<IntervalReport>>> {
    s.0.probes
        .reports(&id, q.after_ms)
        .map(Json)
        .ok_or_else(|| no_probe(&id))
}

async fn set_probe_config(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<ProbeConfig>, JsonRejection>,
) -> ApiResult<Json<ProbeInfo>> {
    let Json(c) = body?;
    Ok(Json(s.0.probes.set_config(&id, Some(c))?))
}

async fn reset_probe_config(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProbeInfo>> {
    if s.0.probes.get(&id).is_none() {
        return Err(no_probe(&id));
    }
    Ok(Json(s.0.probes.set_config(&id, None)?))
}

async fn probe_defaults(State(s): State<AppState>) -> Json<ProbeConfig> {
    Json(s.0.probes.defaults())
}

async fn set_probe_defaults(
    State(s): State<AppState>,
    body: Result<Json<ProbeConfig>, JsonRejection>,
) -> ApiResult<Json<ProbeConfig>> {
    let Json(c) = body?;
    Ok(Json(s.0.probes.set_defaults(c)?))
}

// ---- media ----

/// A plain file name: no directories, no hidden files.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}

impl AppState {
    /// Requests name media files; turn the name into a path inside the media directory.
    fn media_path(&self, name: &std::path::Path) -> ApiResult<PathBuf> {
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
struct MediaFile {
    name: String,
    bytes: u64,
    modified_ms: Option<u64>,
}

async fn list_media(State(s): State<AppState>) -> ApiResult<Json<Vec<MediaFile>>> {
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

async fn upload_media(
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

async fn delete_media(
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

// ---- events ----

fn sse_event(e: &Event) -> SseEvent {
    let value = serde_json::to_value(e).unwrap_or_default();
    let kind = value["type"].as_str().unwrap_or("event").to_string();
    SseEvent::default().event(kind).data(value.to_string())
}

async fn events(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = s.0.events.subscribe();
    let mut closing = s.0.closing.clone();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        let e = match rx.recv().await {
            Ok(e) => sse_event(&e),
            // A slow client missed some; tell it so it can refetch.
            Err(broadcast::error::RecvError::Lagged(n)) => SseEvent::default()
                .event("lagged")
                .data(json!({ "type": "lagged", "missed": n }).to_string()),
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        Some((Ok(e), rx))
    })
    .take_until(async move {
        // End on shutdown only: a dropped sender (no shutdown wired up) isn't one.
        if closing.wait_for(|c| *c).await.is_err() {
            std::future::pending::<()>().await;
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests;
