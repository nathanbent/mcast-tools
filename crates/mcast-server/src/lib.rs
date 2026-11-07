//! HTTP/JSON API over [`mcast_control`]: start and stop pages and
//! receivers, text channels and file transfers; list announced streams,
//! read the monitor, manage media files, and follow everything live as
//! server-sent events.
//!
//! All routes are under `/api/v1`. See `docs/api.md` for the reference.

mod chimes;
mod events;
mod files;
mod live;
mod media;
mod phonebook;
mod routes;
mod text;

use chimes::{chime_preview, list_chimes, put_chime, put_chimes};
use events::events;
use files::{start_file_receiver, start_file_sender};
use media::{
    create_folder, delete_folder, delete_media, download_media, list_folders, list_media,
    media_info, move_media, upload_media,
};
use phonebook::{delete_phonebook, list_phonebook, put_phonebook};
use routes::*;
use text::{send_text, start_text, text_history};

use anyhow::{bail, Context, Result};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mcast_control::{Announcements, Event, EventSink, JobId, Jobs, StartError, Watch};
use mcast_core::Interface;
use mcast_monitor::{MonitorConfig, Target};
use serde_json::json;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

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
    /// The phonebook file (named groups), shared with the CLI.
    pub phonebook: PathBuf,
    /// Which built-in chimes are hidden from lists, shared with the CLI.
    pub chimes: PathBuf,
}

struct Inner {
    jobs: Jobs,
    announcements: Announcements,
    watch: Option<Watch>,
    events: broadcast::Sender<Event>,
    token: Option<String>,
    media_dir: PathBuf,
    max_upload: u64,
    phonebook: PathBuf,
    /// Held while changing the phonebook file.
    phonebook_lock: tokio::sync::Mutex<()>,
    chimes: PathBuf,
    /// Held while changing the chimes file.
    chimes_lock: tokio::sync::Mutex<()>,
    /// What each media file holds, as last probed (see `media::audio_info`).
    audio_info: media::AudioCache,
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
                        targets: named_targets(groups, &cfg.phonebook),
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
        Ok(Self(Arc::new(Inner {
            jobs: Jobs::new(cfg.interface, sink),
            announcements,
            watch,
            events,
            token: cfg.token.clone().filter(|t| !t.is_empty()),
            media_dir,
            max_upload: cfg.max_upload,
            phonebook: cfg.phonebook.clone(),
            phonebook_lock: tokio::sync::Mutex::new(()),
            chimes: cfg.chimes.clone(),
            chimes_lock: tokio::sync::Mutex::new(()),
            audio_info: Default::default(),
            closing,
        })))
    }

    /// Stop every job, withdrawing their SAP announcements.
    pub fn shutdown(&self) {
        self.0.jobs.shutdown();
    }
}

/// Monitor targets for `groups`, named from the phonebook where they're in it.
fn named_targets(groups: &[SocketAddr], phonebook: &std::path::Path) -> Vec<Target> {
    let book = mcast_control::Phonebook::load_from(phonebook).ok();
    groups
        .iter()
        .map(|&g| {
            let mut t = Target::new(g);
            t.name = book
                .as_ref()
                .and_then(|b| b.name_of(g))
                .map(|e| e.name.clone());
            t
        })
        .collect()
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
        .route(
            "/media/{*name}",
            get(download_media).put(upload_media).delete(delete_media),
        )
        .route("/media-info/{*name}", get(media_info))
        .route("/media-folders", get(list_folders).post(create_folder))
        .route(
            "/media-folders/{*name}",
            axum::routing::delete(delete_folder),
        )
        .route("/media-move", post(move_media))
        .route("/texts", post(start_text))
        .route("/jobs/{id}/text", get(text_history).post(send_text))
        .route("/jobs/{id}/live", post(live::stream_live))
        .route("/file-senders", post(start_file_sender))
        .route("/file-receivers", post(start_file_receiver))
        .route("/phonebook", get(list_phonebook))
        .route(
            "/phonebook/{name}",
            axum::routing::put(put_phonebook).delete(delete_phonebook),
        )
        .route("/chimes", get(list_chimes).put(put_chimes))
        .route("/chimes/{name}", axum::routing::put(put_chime))
        .route("/chimes/{name}/preview", get(chime_preview))
        .route("/events", get(events))
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

async fn auth(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let Some(token) = &s.0.token else {
        return next.run(req).await;
    };
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
    if bearer
        .or(query)
        .is_some_and(|t| same(t.as_bytes(), token.as_bytes()))
    {
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

#[cfg(test)]
mod tests;
