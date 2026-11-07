//! The API's handlers: zones and announcements (the same way for both),
//! playing and stopping, and what the panel needs from the mcast server.

use super::{ApiError, ApiResult, AppState, Event};
use crate::backend::Entry;
use crate::model::{Announcement, Config, Zone};
use crate::runner::{PlayError, Run, Trigger};
use crate::store;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

pub(super) async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

/// Whether the mcast server is reachable, and where it is.
pub(super) async fn backend_status(State(s): State<AppState>) -> Json<Value> {
    let mut v = serde_json::to_value(s.0.runner.backend_status()).unwrap_or_default();
    v["url"] = s.0.backend.describe().into();
    Json(v)
}

pub(super) async fn get_config(State(s): State<AppState>) -> ApiResult<Json<Config>> {
    s.load().await.map(Json)
}

// ---- zones and announcements ----

/// What zones and announcements have in common for the API.
pub(super) trait Item: Serialize + DeserializeOwned + Clone + Send + 'static {
    /// For messages: "zone".
    const KIND: &'static str;
    fn id(&self) -> &str;
    fn all(c: &Config) -> &Vec<Self>;
    fn all_mut(c: &mut Config) -> &mut Vec<Self>;
    /// Why it can't be deleted, if it can't.
    fn in_use(_c: &Config, _id: &str) -> Option<String> {
        None
    }
}

impl Item for Zone {
    const KIND: &'static str = "zone";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.zones
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.zones
    }
    fn in_use(c: &Config, id: &str) -> Option<String> {
        let users: Vec<&str> = c
            .announcements
            .iter()
            .filter(|a| a.zones.iter().any(|z| z == id))
            .map(|a| a.name.as_str())
            .collect();
        (!users.is_empty()).then(|| {
            format!(
                "zone \"{id}\" is used by {}; take it out of them first",
                users.join(", ")
            )
        })
    }
}

impl Item for Announcement {
    const KIND: &'static str = "announcement";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.announcements
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.announcements
    }
}

fn not_found<T: Item>(id: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no {} \"{id}\"", T::KIND))
}

/// Read the config, change it, check it and write it back, one change at a
/// time. Reading the file each time keeps edits made by hand.
async fn change<R: Send + 'static>(
    s: &AppState,
    f: impl FnOnce(&mut Config) -> ApiResult<R> + Send + 'static,
) -> ApiResult<R> {
    let _held = s.0.config_lock.lock().await;
    let path = s.0.config.clone();
    let r = tokio::task::spawn_blocking(move || {
        let mut c = store::load(&path).map_err(|e| ApiError::internal(format!("{e:#}")))?;
        let r = f(&mut c)?;
        c.validate().map_err(|e| ApiError::bad(format!("{e:#}")))?;
        store::save(&path, &c).map_err(|e| ApiError::internal(format!("{e:#}")))?;
        Ok::<_, ApiError>(r)
    })
    .await??;
    let _ = s.0.events.send(Event::ConfigChanged);
    Ok(r)
}

fn object(v: &mut Value) -> ApiResult<&mut serde_json::Map<String, Value>> {
    v.as_object_mut()
        .ok_or_else(|| ApiError::bad("expected a JSON object"))
}

fn parse<T: Item>(v: Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(|e| ApiError::bad(format!("{}: {e}", T::KIND)))
}

/// An id made from a name: "Lunch bell" → "lunch-bell", then "lunch-bell-2"...
fn new_id<T: Item>(name: &str, c: &Config) -> String {
    let mut base = String::new();
    for ch in name.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            base.push(ch);
        } else if !base.ends_with('-') && !base.is_empty() {
            base.push('-');
        }
    }
    let mut base: String = base.trim_end_matches('-').chars().take(48).collect();
    if base.is_empty() {
        base = T::KIND.to_string();
    }
    let taken = |id: &str| T::all(c).iter().any(|x| x.id() == id);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("some number is free")
}

pub(super) async fn list<T: Item>(State(s): State<AppState>) -> ApiResult<Json<Vec<T>>> {
    Ok(Json(T::all(&s.load().await?).clone()))
}

pub(super) async fn get_one<T: Item>(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<T>> {
    let c = s.load().await?;
    T::all(&c)
        .iter()
        .find(|x| x.id() == id)
        .cloned()
        .map(Json)
        .ok_or_else(|| not_found::<T>(&id))
}

/// Add one. Without an `id`, one is made from the name.
pub(super) async fn create<T: Item>(
    State(s): State<AppState>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<T>)> {
    let Json(mut v) = body?;
    let item = change(&s, move |c| {
        let obj = object(&mut v)?;
        let id = match obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|i| !i.is_empty())
        {
            Some(id) => id.to_string(),
            None => new_id::<T>(obj.get("name").and_then(Value::as_str).unwrap_or(""), c),
        };
        if T::all(c).iter().any(|x| x.id() == id) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("there's already a {} \"{id}\"", T::KIND),
            ));
        }
        obj.insert("id".into(), id.into());
        let item: T = parse(v)?;
        T::all_mut(c).push(item.clone());
        Ok(item)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(item)))
}

/// Replace one. Its id stays: leave `id` out, or give the same one.
pub(super) async fn put<T: Item>(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> ApiResult<Json<T>> {
    let Json(mut v) = body?;
    change(&s, move |c| {
        let obj = object(&mut v)?;
        if obj
            .get("id")
            .is_some_and(|given| given.as_str() != Some(id.as_str()))
        {
            return Err(ApiError::bad(format!(
                "a {}'s id can't be changed",
                T::KIND
            )));
        }
        obj.insert("id".into(), id.clone().into());
        let item: T = parse(v)?;
        let slot = T::all_mut(c)
            .iter_mut()
            .find(|x| x.id() == id)
            .ok_or_else(|| not_found::<T>(&id))?;
        *slot = item.clone();
        Ok(item)
    })
    .await
    .map(Json)
}

pub(super) async fn delete<T: Item>(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    change(&s, move |c| {
        let pos = T::all(c)
            .iter()
            .position(|x| x.id() == id)
            .ok_or_else(|| not_found::<T>(&id))?;
        if let Some(why) = T::in_use(c, &id) {
            return Err(ApiError::new(StatusCode::CONFLICT, why));
        }
        T::all_mut(c).remove(pos);
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- playing ----

pub(super) async fn play(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    let cfg = s.load().await?;
    if !cfg.announcements.iter().any(|a| a.id == id) {
        return Err(not_found::<Announcement>(&id));
    }
    let runner = s.0.runner.clone();
    let run = tokio::task::spawn_blocking(move || runner.play(&cfg, &id, Trigger::Manual))
        .await?
        .map_err(|e| match e {
            PlayError::NoAnnouncement(_) => ApiError::new(StatusCode::NOT_FOUND, e.to_string()),
            PlayError::Server(m) => ApiError::new(StatusCode::BAD_GATEWAY, m),
        })?;
    Ok((StatusCode::CREATED, Json(run)))
}

pub(super) async fn list_runs(State(s): State<AppState>) -> Json<Vec<Run>> {
    Json(s.0.runner.list())
}

fn no_run(id: u64) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no run {id}"))
}

pub(super) async fn get_run(
    State(s): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<Json<Run>> {
    s.0.runner.get(id).map(Json).ok_or_else(|| no_run(id))
}

/// Stop a run; returns once its senders have stopped.
pub(super) async fn stop_run(
    State(s): State<AppState>,
    Path(id): Path<u64>,
) -> ApiResult<Json<Run>> {
    let runner = s.0.runner.clone();
    tokio::task::spawn_blocking(move || runner.stop(id))
        .await?
        .map(Json)
        .ok_or_else(|| no_run(id))
}

/// All stop.
pub(super) async fn stop_all(State(s): State<AppState>) -> ApiResult<Json<Vec<Run>>> {
    let runner = s.0.runner.clone();
    tokio::task::spawn_blocking(move || {
        runner.stop_all();
        runner.list()
    })
    .await
    .map(Json)
    .map_err(Into::into)
}

// ---- from the mcast server ----

fn bad_gateway(e: crate::BackendError) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, e.message)
}

/// Media file names, for choosing audio and chimes.
pub(super) async fn media(State(s): State<AppState>) -> ApiResult<Json<Vec<String>>> {
    let b = s.0.backend.clone();
    tokio::task::spawn_blocking(move || b.media())
        .await?
        .map(Json)
        .map_err(bad_gateway)
}

/// Phonebook entries, for choosing groups.
pub(super) async fn phonebook(State(s): State<AppState>) -> ApiResult<Json<Vec<Entry>>> {
    let b = s.0.backend.clone();
    tokio::task::spawn_blocking(move || b.phonebook())
        .await?
        .map(Json)
        .map_err(bad_gateway)
}
