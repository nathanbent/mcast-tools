//! `mcast intercom serve`: the panel's own HTTP/JSON API under
//! `/api/intercom/v1`, its web page at `/`, and server-sent events.
//!
//! It runs as its own process with its own token, and reaches the mcast
//! server only through a [`Backend`].

mod arrange;
mod auth;
mod backup;
mod history;
mod hooks;
mod library;
mod live;
mod routes;
pub mod tls;
mod trigger;
mod web;

use crate::backend::Backend;
use crate::model::{
    Announcement, Category, Config, Input, Playlist, Schedule, ScheduleGroup, Site, Zone,
};
use crate::runner::{BackendStatus, Change, Run, Runner};
use crate::scheduler::{Scheduler, GRACE};
use crate::store;
use crate::tokens::TokenStore;
use anyhow::{bail, Context, Result};
pub use auth::{access, url_token_ok, Access, AuthConfig, Caller, STARTUP_TOKEN};
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
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
    /// The startup token: admin. Required unless listening on loopback or
    /// `tokens.json` has an admin token.
    pub token: Option<String>,
    /// The intercom config file.
    pub config: PathBuf,
    /// The access tokens' file; `tokens.json` beside the config by default.
    pub tokens: Option<PathBuf>,
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
    /// The GPIO inputs' states changed: all of them, as `GET /input-status`.
    Inputs {
        inputs: Vec<crate::inputs::InputStatus>,
    },
    /// Expected speakers' states changed (as `GET /speakers`), with any
    /// that went offline or came back.
    Speakers {
        status: crate::presence::Speakers,
        changes: Vec<crate::presence::SpeakerChange>,
    },
    /// A text page went up on displays, or was given its end (`until_ms`).
    Display { page: crate::displays::Page },
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
    pub(crate) auth: AuthConfig,
    pub(crate) tokens: Arc<TokenStore>,
    /// Outbound webhooks, in `webhooks.json` beside the config.
    pub(crate) webhooks: Arc<crate::webhooks::Webhooks>,
    /// Runs that ended, on disk (`history/` beside the config).
    pub(crate) history: Arc<crate::history::History>,
    /// Buttons and contacts on GPIO pins.
    pub(crate) inputs: crate::inputs::Inputs,
    /// Whether expected speakers are there between plays.
    pub(crate) presence: crate::presence::Presence,
    /// Text pages on displays.
    pub(crate) displays: Arc<crate::displays::Displays>,
    closing: watch::Receiver<bool>,
}

#[derive(Clone)]
pub struct AppState(pub(crate) Arc<Inner>);

impl AppState {
    /// [`AppState::with_auth`] with only a startup token (or none: open)
    /// and `tokens.json` beside the config. Panics if that file is broken.
    pub fn new(
        backend: Arc<dyn Backend>,
        config: PathBuf,
        token: Option<String>,
        closing: watch::Receiver<bool>,
    ) -> Self {
        let token = token.filter(|t| !t.is_empty());
        let auth = AuthConfig {
            open: token.is_none(),
            token,
            tokens: crate::tokens::default_path(&config),
        };
        let tokens = TokenStore::open(auth.tokens.clone()).expect("reading tokens.json");
        Self::with_auth(backend, config, auth, Arc::new(tokens), closing)
    }

    /// [`AppState::new`] reading GPIO inputs from `pins` (for tests).
    #[doc(hidden)]
    pub fn with_pins(
        backend: Arc<dyn Backend>,
        config: PathBuf,
        pins: Arc<dyn crate::inputs::Pins>,
        closing: watch::Receiver<bool>,
    ) -> Self {
        let auth = AuthConfig {
            open: true,
            token: None,
            tokens: crate::tokens::default_path(&config),
        };
        let tokens = TokenStore::open(auth.tokens.clone()).expect("reading tokens.json");
        Self::build(backend, config, auth, Arc::new(tokens), closing, pins)
    }

    /// Start the runner and the scheduler. `closing` ends event streams when it becomes true,
    /// so graceful shutdown isn't held up by open clients.
    pub fn with_auth(
        backend: Arc<dyn Backend>,
        config: PathBuf,
        auth: AuthConfig,
        tokens: Arc<TokenStore>,
        closing: watch::Receiver<bool>,
    ) -> Self {
        let pins = crate::inputs::system_pins();
        Self::build(backend, config, auth, tokens, closing, pins)
    }

    fn build(
        backend: Arc<dyn Backend>,
        config: PathBuf,
        auth: AuthConfig,
        tokens: Arc<TokenStore>,
        closing: watch::Receiver<bool>,
        pins: Arc<dyn crate::inputs::Pins>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        let webhooks = Arc::new(crate::webhooks::Webhooks::open(
            crate::webhooks::default_path(&config),
        ));
        let history = Arc::new(crate::history::History::new(
            crate::history::default_dir(&config),
            store::load(&config).map_or(crate::history::DEFAULT_DAYS, |c| c.history_days()),
        ));
        let displays = {
            let tx = events.clone();
            Arc::new(crate::displays::Displays::start(
                backend.clone(),
                move |page| {
                    let _ = tx.send(Event::Display { page: page.clone() });
                },
            ))
        };
        let runner = {
            let tx = events.clone();
            let hooks = webhooks.clone();
            let past = history.clone();
            let shown = displays.clone();
            Runner::new(backend.clone(), move |c| {
                hooks.observe(&c);
                past.observe(&c);
                shown.observe(&c);
                // No subscribers is fine.
                let _ = tx.send(match c {
                    Change::Run(run) => Event::Run { run: run.clone() },
                    Change::Backend(s) => Event::Backend { status: s.clone() },
                    Change::Media => Event::MediaChanged,
                    // As a `display` event, from the displays.
                    Change::Shown(_) => return,
                });
            })
        };
        let scheduler = Scheduler::start(config.clone(), runner.clone(), GRACE);
        let inputs = {
            let tx = events.clone();
            crate::inputs::Inputs::start(config.clone(), runner.clone(), pins, move |inputs| {
                let _ = tx.send(Event::Inputs { inputs });
            })
        };
        let presence = {
            let (tx, hooks) = (events.clone(), webhooks.clone());
            crate::presence::Presence::start(
                config.clone(),
                backend.clone(),
                crate::presence::POLL,
                crate::presence::OFFLINE_AFTER,
                move |status, changes| {
                    hooks.speakers(changes);
                    let _ = tx.send(Event::Speakers {
                        status: status.clone(),
                        changes: changes.to_vec(),
                    });
                },
            )
        };
        Self(Arc::new(Inner {
            scheduler,
            runner,
            backend,
            config,
            config_lock: tokio::sync::Mutex::new(()),
            events,
            auth,
            tokens,
            webhooks,
            history,
            inputs,
            presence,
            displays,
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
    let token = cfg.token.clone().filter(|t| !t.is_empty());
    let tokens_path = cfg
        .tokens
        .clone()
        .unwrap_or_else(|| crate::tokens::default_path(&cfg.config));
    let tokens = {
        let p = tokens_path.clone();
        Arc::new(tokio::task::spawn_blocking(move || TokenStore::open(p)).await??)
    };
    let loopback = cfg.listen.ip().is_loopback();
    if !loopback && token.is_none() && !tokens.has_admin(crate::now_ms()) {
        bail!(
            "refusing to listen on {} without a token: anyone who can reach it could page; \
             set --panel-token / MCAST_INTERCOM_TOKEN, make an admin token \
             (mcast intercom token add NAME --role admin), or listen on 127.0.0.1",
            cfg.listen
        );
    }
    let auth = AuthConfig {
        open: loopback && token.is_none(),
        token,
        tokens: tokens_path,
    };
    // Refuse to start with a config that doesn't load, rather than serve errors.
    let path = cfg.config.clone();
    let loaded = tokio::task::spawn_blocking(move || store::load(&path)).await??;
    let hooks_path = crate::webhooks::default_path(&cfg.config);
    tokio::task::spawn_blocking(move || crate::webhooks::load(&hooks_path)).await??;
    let (close_tx, close_rx) = watch::channel(false);
    let state = {
        let (backend, config, tokens) = (backend.clone(), cfg.config.clone(), tokens.clone());
        tokio::task::spawn_blocking(move || {
            AppState::with_auth(backend, config, auth, tokens, close_rx)
        })
        .await?
    };
    // What the connection was while the panel wasn't running isn't known.
    state.0.history.panel_up_or_down();
    // Write out how much tokens were used now and then, not on every request.
    let flusher = {
        let tokens = tokens.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
            every.tick().await;
            loop {
                every.tick().await;
                let t = tokens.clone();
                if let Ok(Err(e)) = tokio::task::spawn_blocking(move || t.flush()).await {
                    eprintln!("mcast intercom: saving token use: {e:#}");
                }
            }
        })
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
        if state.0.auth.open {
            ""
        } else {
            ", token required"
        }
    );
    let served = server.await;
    state.0.history.panel_up_or_down();
    flusher.abort();
    let t = tokens.clone();
    if let Ok(Err(e)) = tokio::task::spawn_blocking(move || t.flush()).await {
        eprintln!("mcast intercom: saving token use: {e:#}");
    }
    served?;
    Ok(())
}

/// The API under [`API_PREFIX`].
pub fn router(state: AppState) -> Router {
    use routes::*;
    let api = Router::new()
        .route("/health", get(auth::health))
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
        .route("/zones/{id}/test", post(test_zone))
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
        .route(
            "/categories",
            get(list::<Category>).post(create::<Category>),
        )
        .route(
            "/categories/{id}",
            get(get_one::<Category>)
                .put(put::<Category>)
                .delete(delete::<Category>),
        )
        .route("/playlists", get(list::<Playlist>).post(create::<Playlist>))
        .route(
            "/playlists/{id}",
            get(get_one::<Playlist>)
                .put(put::<Playlist>)
                .delete(delete::<Playlist>),
        )
        .route(
            "/panel-layout",
            get(arrange::get_layout).put(arrange::put_layout),
        )
        .route(
            "/layout/{tab}",
            axum::routing::put(put_layout).delete(delete_layout),
        )
        .route("/history", get(history::list))
        .route("/history.csv", get(history::csv))
        .route("/stats", get(history::stats))
        .route("/monitor", get(history::monitor))
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
        .route("/media-tags", axum::routing::put(library::tag))
        .route("/phonebook", get(phonebook))
        .route("/listeners", get(routes::listeners))
        .route("/inputs", get(list::<Input>).post(create::<Input>))
        .route(
            "/inputs/{id}",
            get(get_one::<Input>)
                .put(put::<Input>)
                .delete(delete::<Input>),
        )
        .route("/inputs/{id}/test", post(routes::test_input))
        .route("/input-status", get(routes::input_status))
        .route("/backup", get(backup::download))
        .route("/restore", post(backup::restore))
        .route("/speakers", get(routes::speakers))
        .route("/display", get(routes::display))
        .route("/voices", get(routes::voices))
        .route("/chimes", get(chimes).put(put_chimes))
        .route("/chimes/{name}/preview", get(chime_preview))
        .route("/events", get(events))
        .route("/tokens", get(auth::list_tokens).post(auth::create_token))
        .route(
            "/tokens/{id}",
            axum::routing::put(auth::put_token).delete(auth::delete_token),
        )
        .route("/trigger/{which}", axum::routing::any(trigger::trigger))
        .route(
            "/trigger/{which}/stop",
            axum::routing::any(trigger::trigger_stop),
        )
        .route("/stop-all", axum::routing::any(trigger::stop_all))
        .route("/webhooks", get(hooks::list).post(hooks::create))
        .route(
            "/webhooks/{id}",
            axum::routing::put(hooks::put).delete(hooks::delete),
        )
        .route("/webhooks/{id}/test", post(hooks::test))
        .route("/webhooks/{id}/deliveries", get(hooks::deliveries))
        // Once the route is known: may this caller use it?
        .route_layer(middleware::from_fn(auth::authorize))
        .fallback(|| async { ApiError::new(StatusCode::NOT_FOUND, "no such route") })
        // Every request, unknown routes too: who's calling?
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
        .with_state(state);
    // No fallback outside the API, so the web page can be merged in.
    Router::new().nest(API_PREFIX, api)
}

// ---- events ----

/// Server-sent events. A token limited to some announcements hears only
/// their runs, as `/runs` lists only them.
async fn events(
    State(s): State<AppState>,
    axum::Extension(caller): axum::Extension<Caller>,
) -> Sse<impl Stream<Item = Result<SseEvent, std::convert::Infallible>>> {
    let rx = s.0.events.subscribe();
    let mut closing = s.0.closing.clone();
    // The config the filter uses: read once, and again when it changes.
    let cfg = if caller.is_limited() {
        Some(s.load().await.unwrap_or_default())
    } else {
        None
    };
    let state = (rx, cfg, s, caller);
    let stream = futures_util::stream::unfold(state, |(mut rx, mut cfg, s, caller)| async move {
        let e = loop {
            match rx.recv().await {
                Ok(e) => {
                    // Inputs are set up by admins, and only they see them.
                    if matches!(e, Event::Inputs { .. })
                        && caller.role != crate::tokens::Role::Admin
                    {
                        continue;
                    }
                    if let Some(c) = cfg.as_mut() {
                        match &e {
                            Event::ConfigChanged => *c = s.load().await.unwrap_or_default(),
                            Event::Run { run } if !caller.may_play(c, &run.announcement) => {
                                continue
                            }
                            Event::Display { page } if !caller.may_play(c, &page.announcement) => {
                                continue
                            }
                            _ => {}
                        }
                    }
                    let value = serde_json::to_value(&e).unwrap_or_default();
                    let kind = value["type"].as_str().unwrap_or("event").to_string();
                    break SseEvent::default().event(kind).data(value.to_string());
                }
                // A slow client missed some; tell it so it can refetch.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    break SseEvent::default()
                        .event("lagged")
                        .data(json!({ "type": "lagged", "missed": n }).to_string())
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        };
        Some((Ok(e), (rx, cfg, s, caller)))
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

#[cfg(test)]
mod tests;
