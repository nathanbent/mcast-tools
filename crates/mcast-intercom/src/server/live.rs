//! Push to talk: the panel's page streams its microphone over a WebSocket,
//! and the panel plays it as a live announcement.
//!
//! `GET /live/{announcement}` (a WebSocket) plays the announcement as any
//! other is played (its zones, priority, chimes; Stop and All stop work),
//! with senders whose source is live, then passes each binary message (16-bit
//! little-endian mono PCM at [`LIVE_RATE`]) on to every group's sender.
//!
//! The panel says, as text messages:
//! - `{"type": "started", "run": ID, "groups": N}` once the senders are
//!   going (audio sent before this is kept and plays after the chime);
//! - `{"type": "ended", "reason"}` when the page is over from this side:
//!   it was stopped, every group ended, or it couldn't start.
//!
//! The page ends when the browser sends `end` (as text) or closes the
//! socket: each sender plays what's left, then its end chime.

use super::{ApiError, ApiResult, AppState};
use crate::model::{Audio, LIVE_RATE};
use crate::runner::{RunState, TargetState, Trigger};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::json;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::time::Duration;

/// Chunks queued for each group's sender: at 20 ms a chunk, several seconds.
const QUEUE: usize = 256;
/// How often to look at the run, to notice a stop.
const CHECK: Duration = Duration::from_millis(250);

/// Only the panel's own page may open a socket (browsers send `Origin`,
/// and a WebSocket isn't held back by the same-origin rules on its own).
fn same_origin(headers: &HeaderMap) -> ApiResult<()> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    let origin_host = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, rest)| rest.trim_end_matches('/'));
    if host.is_some() && origin_host == host {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "live pages come from the panel's own page",
        ))
    }
}

pub(super) async fn live(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    same_origin(&headers)?;
    let cfg = s.load().await?;
    let Some(a) = cfg.announcement(&id) else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no announcement \"{id}\""),
        ));
    };
    if a.audio != Audio::Live {
        return Err(ApiError::bad(format!(
            "\"{}\" isn't live; play it instead",
            a.name
        )));
    }
    let id = a.id.clone();
    Ok(ws.on_upgrade(move |socket| talk(s, cfg, id, socket)))
}

async fn say(socket: &mut WebSocket, v: serde_json::Value) {
    let _ = socket.send(Message::Text(v.to_string().into())).await;
}

async fn talk(s: AppState, cfg: crate::model::Config, id: String, mut socket: WebSocket) {
    let runner = s.0.runner.clone();
    let r = runner.clone();
    let started = tokio::task::spawn_blocking(move || r.play(&cfg, &id, Trigger::Manual)).await;
    let run = match started {
        Ok(Ok(run)) => run,
        Ok(Err(e)) => {
            say(
                &mut socket,
                json!({ "type": "ended", "reason": e.to_string() }),
            )
            .await;
            return;
        }
        Err(_) => return,
    };
    let jobs: Vec<u64> = run
        .targets
        .iter()
        .filter(|t| t.state == TargetState::Playing)
        .filter_map(|t| t.job)
        .collect();
    if jobs.is_empty() {
        let reason = run
            .error
            .clone()
            .unwrap_or_else(|| "no group could be paged".into());
        say(&mut socket, json!({ "type": "ended", "reason": reason })).await;
        return;
    }
    // A thread per group streams into its sender on the mcast server.
    let mut groups: Vec<SyncSender<Vec<u8>>> = Vec::new();
    for job in &jobs {
        let (tx, rx) = sync_channel(QUEUE);
        let backend = s.0.backend.clone();
        let job = *job;
        std::thread::spawn(move || {
            if let Err(e) = backend.stream_live(job, rx) {
                // Stopped or taken over: the run shows it.
                eprintln!("live page to job {job}: {e}");
            }
        });
        groups.push(tx);
    }
    say(
        &mut socket,
        json!({ "type": "started", "run": run.id, "groups": groups.len(), "rate": LIVE_RATE }),
    )
    .await;

    let mut check = tokio::time::interval(CHECK);
    let reason = loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(audio))) => {
                    let audio = audio.to_vec();
                    // A group whose sender has gone (stopped, taken over) drops out;
                    // one that's behind loses this chunk rather than holding up the rest.
                    groups.retain(|g| !matches!(g.try_send(audio.clone()), Err(TrySendError::Disconnected(_))));
                    if groups.is_empty() {
                        break Some("every group has ended");
                    }
                }
                Some(Ok(Message::Text(t))) if t.as_str() == "end" => break None,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break None,
                Some(Ok(_)) => {}
            },
            _ = check.tick() => {
                let over = runner
                    .get(run.id)
                    .map_or(true, |r| r.state.is_over() || r.state == RunState::Stopping);
                if over {
                    break Some("stopped");
                }
            }
        }
    };
    // Closing the queues ends each stream: the senders finish with their end chimes.
    drop(groups);
    if let Some(reason) = reason {
        say(&mut socket, json!({ "type": "ended", "reason": reason })).await;
    }
    let _ = socket.send(Message::Close(None)).await;
}
