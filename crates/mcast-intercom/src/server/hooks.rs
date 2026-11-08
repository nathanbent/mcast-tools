//! `/webhooks` (admin): manage outbound webhooks and send test events.

use super::{ApiError, ApiResult, AppState, Event};
use crate::now_ms;
use crate::webhooks::{Delivery, HookChange, Webhook};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

/// A hook for the API: never its secret.
fn view(w: &Webhook, last: Option<&Delivery>) -> Value {
    json!({
        "id": w.id,
        "name": w.name,
        "url": w.url,
        "events": w.events,
        "enabled": w.enabled,
        "sites": w.sites,
        "signed": w.secret.is_some(),
        "created_ms": w.created_ms,
        "last_delivery": last,
    })
}

fn bad(e: anyhow::Error) -> ApiError {
    let m = format!("{e:#}");
    if m.starts_with("no webhook") {
        ApiError::new(StatusCode::NOT_FOUND, m)
    } else if m.contains("already") {
        ApiError::new(StatusCode::CONFLICT, m)
    } else {
        ApiError::bad(m)
    }
}

fn change(v: &Value) -> ApiResult<HookChange> {
    if !v.is_object() {
        return Err(ApiError::bad("expected a JSON object"));
    }
    let string = |k: &str| match v.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ApiError::bad(format!("{k}: expected a string"))),
    };
    let list = |k: &str| -> ApiResult<Option<Vec<String>>> {
        let wrong = || ApiError::bad(format!("{k}: expected a list of names"));
        match v.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(a)) => a
                .iter()
                .map(|e| e.as_str().map(String::from).ok_or_else(wrong))
                .collect::<ApiResult<Vec<_>>>()
                .map(Some),
            Some(_) => Err(wrong()),
        }
    };
    let events = list("events")?;
    let sites = list("sites")?;
    let enabled = match v.get("enabled") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err(ApiError::bad("enabled: expected true or false")),
    };
    Ok(HookChange {
        name: string("name")?,
        url: string("url")?,
        events,
        enabled,
        sites,
    })
}

/// The sites a change names must be the config's.
async fn known_sites(s: &AppState, c: &HookChange) -> ApiResult<()> {
    let Some(sites) = &c.sites else {
        return Ok(());
    };
    let cfg = s.load().await?;
    for id in sites {
        if cfg.site(id.trim()).is_none() {
            return Err(ApiError::bad(format!("sites: no site \"{}\"", id.trim())));
        }
    }
    Ok(())
}

pub(super) async fn list(State(s): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let hooks = s.0.webhooks.clone();
    let list = tokio::task::spawn_blocking(move || hooks.list()).await?;
    Ok(Json(
        list.iter().map(|(w, d)| view(w, d.as_ref())).collect(),
    ))
}

/// `{name, url, events?, enabled?, sign?}` → `{webhook, secret}`; the only
/// time the signing secret is shown (`null` with `"sign": false`).
pub(super) async fn create(
    State(s): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(v) = body?;
    let c = change(&v)?;
    known_sites(&s, &c).await?;
    let sign = match v.get("sign") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ApiError::bad("sign: expected true or false")),
    };
    let hooks = s.0.webhooks.clone();
    let w = tokio::task::spawn_blocking(move || hooks.modify(|f| f.add(c, sign, now_ms())))
        .await?
        .map_err(bad)?;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "webhook": view(&w, None), "secret": w.secret })),
    ))
}

/// `{name?, url?, events?, enabled?}`: change those given.
pub(super) async fn put(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(v) = body?;
    let c = change(&v)?;
    known_sites(&s, &c).await?;
    let hooks = s.0.webhooks.clone();
    let w = tokio::task::spawn_blocking(move || hooks.modify(|f| f.change(&id, c)))
        .await?
        .map_err(bad)?;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok(Json(view(&w, None)))
}

pub(super) async fn delete(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let hooks = s.0.webhooks.clone();
    let w = tokio::task::spawn_blocking(move || {
        let w = hooks.modify(|f| f.remove(&id))?;
        hooks.forget(&w.id);
        Ok::<_, anyhow::Error>(w)
    })
    .await?
    .map_err(bad)?;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok(Json(view(&w, None)))
}

/// A hook's recent deliveries, newest first: `[{"delivery", "event",
/// "about", "at_ms", "ok", "status", "error", "attempts", "took_ms"}]`.
pub(super) async fn deliveries(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Delivery>>> {
    let hooks = s.0.webhooks.clone();
    tokio::task::spawn_blocking(move || {
        if !hooks.list().iter().any(|(w, _)| w.id == id) {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                format!("no webhook \"{id}\""),
            ));
        }
        Ok(Json(hooks.deliveries(&id)))
    })
    .await?
}

/// Send a `test` event now and say how it went.
pub(super) async fn test(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Delivery>> {
    let hooks = s.0.webhooks.clone();
    tokio::task::spawn_blocking(move || hooks.test(&id))
        .await?
        .map(Json)
        .map_err(bad)
}
