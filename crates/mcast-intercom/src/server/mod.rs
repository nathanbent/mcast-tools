//! `mcast intercom serve`: the panel's own HTTP/JSON API under
//! `/api/intercom/v1`, its web page at `/`, and server-sent events.
//!
//! It runs as its own process with its own token, and reaches the mcast
//! server only through a [`Backend`].

mod library;
mod live;
mod routes;
pub mod tls;
mod web;

use crate::backend::Backend;
use crate::model::{Announcement, Config, Schedule, ScheduleGroup, Site, Zone};
use crate::runner::{BackendStatus, Change, Run, Runner};
use crate::scheduler::{Scheduler, GRACE};
use crate::store;
use anyhow::{bail, Context, Result};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use serde::Serialize;
use serde_json::json;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};

/// Where the API lives.
pub const API_PREFIX: &str = "/api/intercom/v1";

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub listen: SocketAddr,
    /// Bearer token clients must send. Required unless listening on loopback.
    pub token: Option<String>,
    /// The intercom config file.
    pub config: PathBuf,
    /// Serve the web page (otherwise the API alone).
    pub web: bool,
    /// Serve the web page from this directory instead of the built-in copy.
    pub web_dir: Option<PathBuf>,
    /// Serve HTTPS with this certificate and key (see [`tls`]).
    pub tls: Option<tls::TlsFiles>,
}

/// Something clients may want to hear about as it happens.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A run started or changed.
    Run { run: Run },
    /// Zones, announcements, schedules or settings changed; fetch them again.
    ConfigChanged,
    /// The connection to the mcast server changed.
    Backend { status: BackendStatus },
    /// Media files or folders changed on the mcast server; fetch them again.
    MediaChanged,
}

pub(crate) struct Inner {
    pub(crate) runner: Runner,
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) config: PathBuf,
    /// Held while changing the config file.
    pub(crate) config_lock: tokio::sync::Mutex<()>,
    pub(crate) events: broadcast::Sender<Event>,
    /// Plays schedules; told when the config changes.
    pub(crate) scheduler: Scheduler,
    token: Option<String>,
    closing: watch::Receiver<bool>,
}

#[derive(Clone)]
pub struct AppState(pub(crate) Arc<Inner>);

impl AppState {
    /// Start the runner and the scheduler. `closing` ends event streams when it becomes true,
    /// so graceful shutdown isn't held up by open clients.
    pub fn new(
        backend: Arc<dyn Backend>,
        config: PathBuf,
        token: Option<String>,
        closing: watch::Receiver<bool>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let runner = {
            let tx = events.clone();
            Runner::new(backend.clone(), move |c| {
                // No subscribers is fine.
                let _ = tx.send(match c {
                    Change::Run(run) => Event::Run { run: run.clone() },
                    Change::Backend(s) => Event::Backend { status: s.clone() },
                    Change::Media => Event::MediaChanged,
                });
            })
        };
        let scheduler = Scheduler::start(config.clone(), runner.clone(), GRACE);
        Self(Arc::new(Inner {
            scheduler,
            runner,
            backend,
            config,
            config_lock: tokio::sync::Mutex::new(()),
            events,
            token: token.filter(|t| !t.is_empty()),
            closing,
        }))
    }

    pub fn runner(&self) -> &Runner {
        &self.0.runner
    }

    pub(crate) async fn load(&self) -> ApiResult<Config> {
        let path = self.0.config.clone();
        tokio::task::spawn_blocking(move || store::load(&path))
            .await?
            .map_err(|e| ApiError::internal(format!("{e:#}")))
    }
}

/// Serve the panel until `shutdown` resolves. Runs started here are left to
/// finish on the mcast server; repeats still to come are dropped.
pub async fn serve(
    cfg: ServeConfig,
    backend: Arc<dyn Backend>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    if !cfg.listen.ip().is_loopback() && cfg.token.as_deref().map_or(true, str::is_empty) {
        bail!(
            "refusing to listen on {} without a token: anyone who can reach it could page; \
             set --panel-token / MCAST_INTERCOM_TOKEN, or listen on 127.0.0.1",
            cfg.listen
        );
    }
    // Refuse to start with a config that doesn't load, rather than serve errors.
    let path = cfg.config.clone();
    let loaded = tokio::task::spawn_blocking(move || store::load(&path)).await??;
    let (close_tx, close_rx) = watch::channel(false);
    let state = {
        let (backend, config, token) = (backend.clone(), cfg.config.clone(), cfg.token.clone());
        tokio::task::spawn_blocking(move || AppState::new(backend, config, token, close_rx)).await?
    };
    let mut app = router(state.clone());
    if cfg.web {
        app = app.merge(web::router(cfg.web_dir));
    }
    let shutdown = async move {
        shutdown.await;
        let _ = close_tx.send(true);
    };
    let (addr, server) = match &cfg.tls {
        None => {
            let listener = tokio::net::TcpListener::bind(cfg.listen)
                .await
                .with_context(|| format!("listening on {}", cfg.listen))?;
            let addr = format!("http://{}", listener.local_addr()?);
            let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown);
            (
                addr,
                Box::pin(async move { serve.await })
                    as std::pin::Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>,
            )
        }
        Some(files) => {
            let listener = tls::TlsListener::bind(cfg.listen, files).await?;
            let addr = format!("https://{}", axum::serve::Listener::local_addr(&listener)?);
            let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown);
            (addr, Box::pin(async move { serve.await }) as _)
        }
    };
    eprintln!(
        "mcast intercom on {addr} ({} zones, {} announcements from {}; sending through {}){}",
        loaded.zones.len(),
        loaded.announcements.len(),
        cfg.config.display(),
        backend.describe(),
        if state.0.token.is_some() {
            ", token required"
        } else {
            ""
        }
    );
    server.await?;
    Ok(())
}

/// The API under [`API_PREFIX`].
pub fn router(state: AppState) -> Router {
    use routes::*;
    let api = Router::new()
        .route("/health", get(health))
        .route("/backend", get(backend_status))
        .route("/config", get(get_config))
        .route("/zones", get(list::<Zone>).post(create::<Zone>))
        .route(
            "/zones/{id}",
            get(get_one::<Zone>).put(put::<Zone>).delete(delete::<Zone>),
        )
        .route(
            "/announcements",
            get(list::<Announcement>).post(create::<Announcement>),
        )
        .route(
            "/announcements/{id}",
            get(get_one::<Announcement>)
                .put(put::<Announcement>)
                .delete(delete::<Announcement>),
        )
        .route("/announcements/{id}/play", post(play))
        .route("/live/{id}", get(live::live))
        .route("/schedules", get(list::<Schedule>).post(create::<Schedule>))
        .route(
            "/schedules/{id}",
            get(get_one::<Schedule>)
                .put(put::<Schedule>)
                .delete(delete::<Schedule>),
        )
        .route("/sites", get(list::<Site>).post(create::<Site>))
        .route(
            "/sites/{id}",
            get(get_one::<Site>).put(put::<Site>).delete(delete::<Site>),
        )
        .route(
            "/schedule-groups",
            get(list::<ScheduleGroup>).post(create::<ScheduleGroup>),
        )
        .route(
            "/schedule-groups/{id}",
            get(get_one::<ScheduleGroup>)
                .put(put::<ScheduleGroup>)
                .delete(delete::<ScheduleGroup>),
        )
        .route("/upcoming", get(upcoming))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/runs", get(list_runs).delete(stop_all))
        .route("/runs/{id}", get(get_run).delete(stop_run))
        .route("/media", get(media))
        .route(
            "/media/{*name}",
            get(library::download)
                .put(library::upload)
                .delete(library::delete),
        )
        .route("/library", get(library::library))
        .route("/media-folders", post(library::create_folder))
        .route(
            "/media-folders/{*name}",
            axum::routing::delete(library::delete_folder),
        )
        .route("/media-move", post(library::move_media))
        .route("/media-hidden", axum::routing::put(library::hide))
        .route("/phonebook", get(phonebook))
        .route("/chimes", get(chimes).put(put_chimes))
        .route("/chimes/{name}/preview", get(chime_preview))
        .route("/events", get(events))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "no such route") })
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state);
    // No fallback outside the API, so the web page can be merged in.
    Router::new().nest(API_PREFIX, api)
}

// ---- events ----

async fn events(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = s.0.events.subscribe();
    let mut closing = s.0.closing.clone();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        let e = match rx.recv().await {
            Ok(e) => {
                let value = serde_json::to_value(&e).unwrap_or_default();
                let kind = value["type"].as_str().unwrap_or("event").to_string();
                SseEvent::default().event(kind).data(value.to_string())
            }
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

// ---- errors ----

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    pub(crate) fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<axum::extract::rejection::JsonRejection> for ApiError {
    fn from(e: axum::extract::rejection::JsonRejection) -> Self {
        ApiError::new(e.status(), e.body_text())
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        ApiError::internal(e)
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

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
