//! Server-sent events: everything that happens, as it happens.

use crate::AppState;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::{Stream, StreamExt};
use mcast_control::Event;
use serde_json::json;
use tokio::sync::broadcast;

fn sse_event(e: &Event) -> SseEvent {
    let value = serde_json::to_value(e).unwrap_or_default();
    let kind = value["type"].as_str().unwrap_or("event").to_string();
    SseEvent::default().event(kind).data(value.to_string())
}

pub(crate) async fn events(
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
