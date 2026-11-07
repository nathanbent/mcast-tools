//! Probes: the registry and check-ins (see `mcast_control::probes`), the
//! multicast beacon that lets probes find this server, and the hello that
//! proves to them it knows their token.

use crate::{ApiError, ApiResult, AppState, BeaconConfig};
use anyhow::Result;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use mcast_control::probes::{
    hello_proof, Beacon, CheckIn, CheckInReply, Hello, IntervalReport, ProbeError,
};
use mcast_control::{ProbeConfig, ProbeInfo};
use mcast_core::Interface;
use std::sync::Arc;
use std::time::Duration;

/// Multicasts a [`Beacon`] every few seconds until dropped.
pub(crate) struct BeaconSender {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for BeaconSender {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(crate) fn start_beacon(
    b: &BeaconConfig,
    interface: Interface,
    port: u16,
) -> Result<BeaconSender> {
    let mut opts = mcast_core::MulticastOptions::new(b.group)?;
    opts.interface = interface;
    opts.ttl = b.ttl;
    let socket = mcast_core::open_sender(&opts)?;
    let payload = serde_json::to_vec(&Beacon {
        service: Beacon::SERVICE.into(),
        v: 1,
        name: mcast_control::request::hostname(),
        port,
        url: b.url.clone(),
    })?;
    eprintln!("announcing to probes on {}", b.group);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("mcast-beacon".into())
            .spawn(move || {
                let tick = Duration::from_millis(200);
                let every = Duration::from_secs(mcast_control::probes::BEACON_INTERVAL_S);
                let mut next = std::time::Instant::now();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if std::time::Instant::now() >= next {
                        // A missing route now may be there later; keep trying.
                        let _ = socket.send(&payload);
                        next += every;
                    }
                    std::thread::sleep(tick);
                }
            })?
    };
    Ok(BeaconSender {
        stop,
        thread: Some(thread),
    })
}

#[derive(serde::Deserialize)]
pub(crate) struct HelloQuery {
    nonce: String,
}

/// Prove to a probe that found us by beacon that we know its token.
pub(crate) async fn probe_hello(
    State(s): State<AppState>,
    Query(q): Query<HelloQuery>,
) -> ApiResult<Json<Hello>> {
    if !mcast_control::probes::valid_nonce(&q.nonce) {
        return Err(ApiError::bad("nonce must be 16-128 hex digits"));
    }
    let proofs = [&s.0.probe_token, &s.0.token]
        .into_iter()
        .flatten()
        .map(|t| hello_proof(t, &q.nonce))
        .collect();
    Ok(Json(Hello {
        name: mcast_control::request::hostname(),
        proofs,
    }))
}

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

pub(crate) async fn list_probes(State(s): State<AppState>) -> Json<Vec<ProbeInfo>> {
    Json(s.0.probes.list())
}

pub(crate) async fn get_probe(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProbeInfo>> {
    s.0.probes.get(&id).map(Json).ok_or_else(|| no_probe(&id))
}

pub(crate) async fn remove_probe(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    s.0.probes.remove(&id).map_err(|_| no_probe(&id))?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn probe_check_in(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CheckIn>, JsonRejection>,
) -> ApiResult<Json<CheckInReply>> {
    let Json(c) = body?;
    Ok(Json(s.0.probes.check_in(&id, c)?))
}

#[derive(serde::Deserialize)]
pub(crate) struct ReportsQuery {
    /// Only reports that ended after this time (ms since the epoch).
    after_ms: Option<u64>,
}

pub(crate) async fn probe_reports(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ReportsQuery>,
) -> ApiResult<Json<Vec<IntervalReport>>> {
    s.0.probes
        .reports(&id, q.after_ms)
        .map(Json)
        .ok_or_else(|| no_probe(&id))
}

pub(crate) async fn set_probe_config(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<ProbeConfig>, JsonRejection>,
) -> ApiResult<Json<ProbeInfo>> {
    let Json(c) = body?;
    Ok(Json(s.0.probes.set_config(&id, Some(c))?))
}

pub(crate) async fn reset_probe_config(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProbeInfo>> {
    if s.0.probes.get(&id).is_none() {
        return Err(no_probe(&id));
    }
    Ok(Json(s.0.probes.set_config(&id, None)?))
}

pub(crate) async fn probe_defaults(State(s): State<AppState>) -> Json<ProbeConfig> {
    Json(s.0.probes.defaults())
}

pub(crate) async fn set_probe_defaults(
    State(s): State<AppState>,
    body: Result<Json<ProbeConfig>, JsonRejection>,
) -> ApiResult<Json<ProbeConfig>> {
    let Json(c) = body?;
    Ok(Json(s.0.probes.set_defaults(c)?))
}
