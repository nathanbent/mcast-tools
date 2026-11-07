//! The API's handlers: zones and announcements (the same way for both),
//! playing and stopping, and what the panel needs from the mcast server.

use super::{ApiError, ApiResult, AppState, Caller, Event};
use crate::backend::Entry;
use crate::model::{
    Announcement, Category, Config, DayOff, Defaults, Playlist, Schedule, ScheduleGroup, Site, Zone,
};
use crate::runner::{PlayError, Run};
use crate::scheduler;
use crate::store;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

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
    fn in_use(c: &Config, id: &str) -> Option<String> {
        let n = c.schedules.iter().filter(|s| s.announcement == id).count();
        (n > 0).then(|| {
            format!(
                "announcement \"{id}\" is played by {n} schedule{}; delete {} first",
                if n == 1 { "" } else { "s" },
                if n == 1 { "it" } else { "them" }
            )
        })
    }
}

impl Item for Schedule {
    const KIND: &'static str = "schedule";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.schedules
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.schedules
    }
}

impl Item for Site {
    const KIND: &'static str = "site";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.sites
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.sites
    }
    fn in_use(c: &Config, id: &str) -> Option<String> {
        let at = |site: &Option<String>| site.as_deref() == Some(id);
        let zones = c.zones.iter().filter(|z| at(&z.site)).count();
        let schedules = c.schedules.iter().filter(|s| at(&s.site)).count();
        let plural = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
        let parts: Vec<String> = [(zones, "zone"), (schedules, "schedule")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, w)| plural(n, w))
            .collect();
        (!parts.is_empty()).then(|| {
            format!(
                "site \"{id}\" has {}; move or delete them first",
                parts.join(" and ")
            )
        })
    }
}

impl Item for ScheduleGroup {
    const KIND: &'static str = "schedule group";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.schedule_groups
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.schedule_groups
    }
    fn in_use(c: &Config, id: &str) -> Option<String> {
        let n = c
            .schedules
            .iter()
            .filter(|s| s.group.as_deref() == Some(id))
            .count();
        (n > 0).then(|| {
            format!(
                "schedule group \"{id}\" has {n} schedule{}; move or delete {} first",
                if n == 1 { "" } else { "s" },
                if n == 1 { "it" } else { "them" }
            )
        })
    }
}

impl Item for Category {
    const KIND: &'static str = "category";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.categories
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.categories
    }
    fn in_use(c: &Config, id: &str) -> Option<String> {
        let users: Vec<&str> = c
            .announcements
            .iter()
            .filter(|a| a.category.as_deref() == Some(id))
            .map(|a| a.name.as_str())
            .collect();
        (!users.is_empty()).then(|| {
            format!(
                "category \"{id}\" has {}; move them out first",
                users.join(", ")
            )
        })
    }
}

impl Item for Playlist {
    const KIND: &'static str = "playlist";
    fn id(&self) -> &str {
        &self.id
    }
    fn all(c: &Config) -> &Vec<Self> {
        &c.playlists
    }
    fn all_mut(c: &mut Config) -> &mut Vec<Self> {
        &mut c.playlists
    }
    fn in_use(c: &Config, id: &str) -> Option<String> {
        let users = c.playlist_users(id);
        (!users.is_empty()).then(|| {
            format!(
                "playlist \"{id}\" is played by {}; change them first",
                users.join(", ")
            )
        })
    }
}

fn not_found<T: Item>(id: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no {} \"{id}\"", T::KIND))
}

/// Read the config, change it, check it and write it back, one change at a
/// time. Reading the file each time keeps edits made by hand.
pub(super) async fn change<R: Send + 'static>(
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
    s.0.scheduler.poke();
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
            None => {
                // Schedules' names are optional: name them after what they play.
                let base = ["name", "announcement"]
                    .iter()
                    .filter_map(|k| obj.get(*k).and_then(Value::as_str))
                    .find(|n| !n.trim().is_empty())
                    .unwrap_or("");
                new_id::<T>(base, c)
            }
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
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Run>)> {
    let cfg = s.load().await?;
    if !caller.may_play(&cfg, &id) {
        return Err(caller.cant(&format!("play \"{id}\"")));
    }
    match cfg.announcements.iter().find(|a| a.id == id) {
        None => return Err(not_found::<Announcement>(&id)),
        Some(a) if a.audio == crate::model::Audio::Live => {
            return Err(ApiError::bad(format!(
                "\"{}\" is live: talk on it with the panel's push to talk (/live/{id})",
                a.name
            )))
        }
        Some(_) => {}
    }
    let runner = s.0.runner.clone();
    let trigger = caller.trigger();
    let run = tokio::task::spawn_blocking(move || runner.play(&cfg, &id, trigger))
        .await?
        .map_err(|e| match e {
            PlayError::NoAnnouncement(_) => ApiError::new(StatusCode::NOT_FOUND, e.to_string()),
            PlayError::Server(m) => ApiError::new(StatusCode::BAD_GATEWAY, m),
        })?;
    Ok((StatusCode::CREATED, Json(run)))
}

/// Runs; a trigger token sees only those of its announcements.
pub(super) async fn list_runs(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> Json<Vec<Run>> {
    let mut runs = s.0.runner.list();
    let cfg = s.load().await.unwrap_or_default();
    runs.retain(|r| caller.may_play(&cfg, &r.announcement));
    Json(runs)
}

fn no_run(id: u64) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("no run {id}"))
}

/// A run the caller may see and stop.
async fn their_run(s: &AppState, caller: &Caller, id: u64) -> ApiResult<Run> {
    let run = s.0.runner.get(id).ok_or_else(|| no_run(id))?;
    if !caller.may_play(&s.load().await?, &run.announcement) {
        return Err(caller.cant(&format!("see or stop runs of \"{}\"", run.name)));
    }
    Ok(run)
}

pub(super) async fn get_run(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<u64>,
) -> ApiResult<Json<Run>> {
    their_run(&s, &caller, id).await.map(Json)
}

/// Stop a run; returns once its senders have stopped.
pub(super) async fn stop_run(
    State(s): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<u64>,
) -> ApiResult<Json<Run>> {
    their_run(&s, &caller, id).await?;
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

/// Errors from the mcast server: its own status for "no such chime" and
/// "bad request", else 502.
pub(super) fn passed_on(e: crate::BackendError) -> ApiError {
    match e.status {
        404 | 409 | 413 | 422 => ApiError::new(StatusCode::from_u16(e.status).unwrap(), e.message),
        _ => bad_gateway(e),
    }
}

/// The mcast server's built-in chimes, with which are hidden from lists.
pub(super) async fn chimes(State(s): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let b = s.0.backend.clone();
    tokio::task::spawn_blocking(move || b.chimes())
        .await?
        .map(Json)
        .map_err(bad_gateway)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HideChimes {
    hidden: Vec<String>,
}

/// `{"hidden": [names]}`: hide exactly these chimes from lists, on the mcast
/// server (so in its own GUI and TUI too).
pub(super) async fn put_chimes(
    State(s): State<AppState>,
    body: Result<Json<HideChimes>, JsonRejection>,
) -> ApiResult<Json<Vec<Value>>> {
    let Json(req) = body?;
    let b = s.0.backend.clone();
    tokio::task::spawn_blocking(move || b.hide_chimes(&req.hidden))
        .await?
        .map(Json)
        .map_err(passed_on)
}

/// A built-in chime as a WAV file, to listen to in the browser.
pub(super) async fn chime_preview(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let b = s.0.backend.clone();
    let wav = tokio::task::spawn_blocking(move || b.chime_preview(&name))
        .await?
        .map_err(passed_on)?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "audio/wav"),
            (axum::http::header::CACHE_CONTROL, "max-age=3600"),
        ],
        wav,
    )
        .into_response())
}

// ---- schedules ----

#[derive(serde::Deserialize)]
pub(super) struct UpcomingQuery {
    n: Option<usize>,
    /// Only this site's schedules; empty for those with no site.
    site: Option<String>,
}

/// The next times anything is scheduled, and the time zones they're in.
pub(super) async fn upcoming(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<UpcomingQuery>,
) -> ApiResult<Json<Value>> {
    let mut cfg = s.load().await?;
    let n = q.n.unwrap_or(10).clamp(1, 100);
    let now = jiff::Timestamp::now();
    if let Some(site) = &q.site {
        cfg.schedules
            .retain(|s| s.site.as_deref().unwrap_or("") == site.as_str());
    }
    let fires = scheduler::upcoming(&cfg, now, n);
    // Each schedule's next time, enabled or not (and paused or not), past
    // days off; null when it won't come again.
    let next: serde_json::Map<String, Value> = cfg
        .schedules
        .iter()
        .map(|s| {
            let at = scheduler::next_playing(&cfg, s, now).map(|t| t.as_millisecond());
            (s.id.clone(), json!(at))
        })
        .collect();
    let sites: serde_json::Map<String, Value> = cfg
        .sites
        .iter()
        .map(|site| {
            let tz = cfg.site_tz_name(Some(&site.id));
            (
                site.id.clone(),
                json!({ "time_zone": tz, "paused": site.paused }),
            )
        })
        .collect();
    Ok(Json(json!({
        "time_zone": cfg.tz_name(),
        "paused": cfg.schedules_paused,
        "now_ms": crate::now_ms(),
        "fires": fires,
        "next": next,
        "sites": sites,
    })))
}

fn settings(c: &Config) -> Value {
    json!({
        "time_zone": c.time_zone,
        "effective_time_zone": c.tz_name(),
        "defaults": c.defaults,
        "schedules_paused": c.schedules_paused,
        "days_off": c.days_off,
        "panel": c.panel.full(),
    })
}

/// The panel-wide settings: time zone, announcement defaults, pausing
/// schedules, and days off.
pub(super) async fn get_settings(State(s): State<AppState>) -> ApiResult<Json<Value>> {
    Ok(Json(settings(&s.load().await?)))
}

/// A field that can be left out (no change) or `null` (cleared).
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

/// Each field left out stays as it is.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings {
    /// `null` for the system's.
    #[serde(default, deserialize_with = "present")]
    time_zone: Option<Option<String>>,
    #[serde(default)]
    defaults: Option<Defaults>,
    #[serde(default)]
    schedules_paused: Option<bool>,
    #[serde(default)]
    days_off: Option<Vec<DayOff>>,
    /// How the Panel looks, replaced whole.
    #[serde(default)]
    panel: Option<crate::model::PanelLook>,
}

pub(super) async fn put_settings(
    State(s): State<AppState>,
    body: Result<Json<Settings>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(new) = body?;
    change(&s, move |c| {
        if let Some(tz) = new.time_zone {
            c.time_zone = tz.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        }
        if let Some(d) = new.defaults {
            c.defaults = d;
        }
        if let Some(p) = new.schedules_paused {
            c.schedules_paused = p;
        }
        if let Some(days) = new.days_off {
            c.days_off = days;
        }
        if let Some(look) = new.panel {
            c.panel = look;
        }
        Ok(settings(c))
    })
    .await
    .map(Json)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    order: Vec<String>,
}

/// `{"order": [section ids]}`: how a tab's sections are arranged, for
/// everyone. Returns the whole layout.
pub(super) async fn put_layout(
    State(s): State<AppState>,
    Path(tab): Path<String>,
    body: Result<Json<Layout>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(l) = body?;
    change(&s, move |c| {
        c.layout.insert(tab, l.order);
        Ok(json!(c.layout))
    })
    .await
    .map(Json)
}

/// Back to a tab's usual order. Returns the whole layout.
pub(super) async fn delete_layout(
    State(s): State<AppState>,
    Path(tab): Path<String>,
) -> ApiResult<Json<Value>> {
    change(&s, move |c| {
        c.layout.remove(&tab);
        Ok(json!(c.layout))
    })
    .await
    .map(Json)
}
