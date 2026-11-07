//! HTTP/JSON API over [`mcast_control`]: start and stop pages and
//! receivers, text channels and file transfers; list announced streams,
//! read the monitor, manage media files, and follow everything live as
//! server-sent events.
//!
//! All routes are under `/api/v1`. See `docs/api.md` for the reference.

mod events;
mod files;
mod media;
mod probes;
mod routes;
mod text;

use events::events;
use files::{start_file_receiver, start_file_sender};
use media::{delete_media, download_media, list_media, upload_media};
use probes::*;
use routes::*;
use text::{send_text, start_text, text_history};

use anyhow::{bail, Context, Result};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use mcast_control::{Announcements, Event, EventSink, JobId, Jobs, Probes, StartError, Watch};
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
    /// A second token that only lets probes check in (so a token copied
    /// onto many Pis can't be used to page). Probes may use `token` too.
    pub probe_token: Option<String>,
    /// Where to keep the known probes and their configurations; `None`
    /// keeps them in memory only.
    pub probe_state: Option<PathBuf>,
    /// Announce the server to probes by multicast; `None` doesn't.
    pub beacon: Option<BeaconConfig>,
}

#[derive(Debug, Clone)]
pub struct BeaconConfig {
    pub group: SocketAddr,
    /// URL for probes to use instead of this host's address and port.
    pub url: Option<String>,
    pub ttl: u32,
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
    let beacon = match &cfg.beacon {
        Some(b) => {
            let port = listener.local_addr()?.port();
            Some(start_beacon(b, cfg.interface, port).context("starting the probe beacon")?)
        }
        None => None,
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = close_tx.send(true);
        })
        .await?;
    drop(beacon);
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
            "/media/{name}",
            get(download_media).put(upload_media).delete(delete_media),
        )
        .route("/texts", post(start_text))
        .route("/jobs/{id}/text", get(text_history).post(send_text))
        .route("/file-senders", post(start_file_sender))
        .route("/file-receivers", post(start_file_receiver))
        .route("/events", get(events))
        .route("/probe-hello", get(probe_hello))
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
    // Probes ask this before they have reason to trust the server with a token.
    if s.0.token.is_none() || req.uri().path().ends_with("/probe-hello") {
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

#[cfg(test)]
mod tests;
